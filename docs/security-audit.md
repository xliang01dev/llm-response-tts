# Security audit results

This describes the security posture of this pipeline as designed: what's applied, why, what it
buys you, and how far it goes toward eliminating the underlying risk.

## Network isolation: two networks instead of one flat one

Only `nginx` is bound to the host, and only on `127.0.0.1:3000`. Every other service - kokoros,
Redis, `ingress`, and all 3 `worker` containers - publishes no host port at all (`expose` only),
reachable solely from other containers on the compose network. That's a network-layer
restriction enforced by the OS before any request-level logic runs, which is a stronger
guarantee than something like CORS: CORS is a browser-only convention that doesn't stop
non-browser clients or even see the underlying TCP connection, whereas an unpublished port
genuinely cannot be dialed from outside the container network.

Services are further split across two Docker networks. `edge` is host-facing and holds only
`nginx`. `backend` holds everything else - Redis, `ingress`, `worker`, kokoros - and is marked
`internal: true`, meaning Docker gives it no default route to the internet. `nginx` is
multi-homed across both, so it can still reach `ingress`, but nothing on `backend` can reach the
internet regardless.

The benefit is specific: kokoros' own dependency tree carries several RustSec advisories
(certificate/CRL parsing bugs in `rustls-webpki`, an integer overflow in `bytes`, an
out-of-bounds read in `slab`) that live in kokoros' `Cargo.lock`, not ours, and can't be patched
directly from this repo. None of them are reachable in the current design, since kokoros makes
no outbound network calls at request time. `internal: true` converts that "not reachable in the
current design" into "not reachable no matter what the code does" - a guarantee that holds even
if a future kokoros version or a code path introduced an attempt to call out. That's the
difference between a risk that happens to be absent today and one that's structurally
impossible: it eliminates the exploitability of an entire class of vulnerability regardless of
what's sitting in kokoros' dependency tree, present or future, without requiring trust that
kokoros' code stays network-inert on its own.

## Bearer token authentication, enforced fail-closed

nginx requires every request to carry `Authorization: Bearer <token>` matching
`LLM_RESPONSE_TTS_BEARER_TOKEN` from `docker/.env` (see
`docker/nginx/templates/default.conf.template`, which proxies to `ingress` rather than talking
to kokoros directly). `ingest` and `player` both read that same `.env` file and attach the token
automatically, and the file is gitignored, so each machine needs its own. This is the only
authentication layer in the deployment - neither kokoros nor `ingress` implements auth of its
own - so it's the entire trust boundary, and its startup is designed to fail closed rather than
fail open.

Two checks enforce that. The nginx service's `env_file` is marked `required: true`, so Compose
refuses to start nginx at all if `docker/.env` doesn't exist. And
`docker/nginx/entrypoint.d/25-check-bearer-token.sh` runs as part of nginx's own startup
sequence and checks two things: that the token placeholder was actually substituted into the
rendered config (catches a missing `.env` var), and that the substituted value isn't empty
(catches `LLM_RESPONSE_TTS_BEARER_TOKEN=` with no value, which would otherwise render as
`if ($http_authorization != "Bearer ")` - satisfiable by literally sending
`Authorization: Bearer ` with nothing after it). The risk this eliminates is a silent downgrade
from "authenticated" to "open to anyone who can reach the port": a misconfigured `.env` produces
a hard startup failure instead of a server that looks like it's enforcing auth but isn't.

Once a message is past `ingress`, nothing else in the pipeline re-checks the token - `worker`
calling kokoros directly is trusted the same way nginx calling `ingress` is, because it's all on
`backend`, which only trusted containers can reach at all. That's an acceptable internal trust
model precisely because `backend` is unreachable from outside; the token check at the one actual
entry point is what does the work.

## Server-side output_dir derivation

Each enqueued job needs an `output_dir` for `worker` to write synthesized audio into and
`player` to read it from. `ingress` derives it entirely server-side, from its own
`LLM_RESPONSE_TTS_SOUND_OUTPUT` base plus a `session_dir` value the client sends - but
`session_dir` is validated (`valid_session_dir` in `services/ingress/src/main.rs`) before it's
ever used as a path component: it must equal the request's `session` or start with
`"{session}-"`, and it can't contain `/`, `\`, `.`, or `..`.

This closes off a specific class of bug: two fields that should be structurally tied together -
which session a request belongs to, and which directory it's allowed to write into - are never
allowed to disagree. Without that binding, an authenticated caller (i.e. anyone holding the
token) could send an arbitrary `output_dir` and have `worker` write into it, reaching another
session's directory or attempting path traversal outside the intended output tree. With the
binding in place, there's no client-controlled path component that reaches the filesystem
unvalidated, so cross-session targeting and traversal are both structurally impossible rather
than merely unobserved.

## Non-root containers on pinned, stable base images

`ingress` and `worker` build on `debian:bookworm-slim`, a stable release rather than a rolling
branch, and run as a dedicated non-root `appuser`. Every image in the stack - the
`rust:1.88.0-slim-trixie` builder stage, the `debian:bookworm-slim` runtime stage, nginx
(`nginx:1.31.3-alpine`), and Redis (`redis:7.4.10-alpine`) - is pinned by content digest
(`image:tag@sha256:...`), not just a version tag, and kokoros' Docker build source is pinned to a
specific audited commit (`b54354b860df94b064e254524777ce41d4d2c689`) rather than an unpinned git
URL that could resolve to a different upstream commit on any given build.

A version tag alone names a release, not a fixed set of bytes: the same tag can be repointed to
different image content later, most commonly when an upstream maintainer rebuilds it to pick up
an OS-level patch without changing the version number. A digest pins the bytes themselves, so
`image:tag@sha256:...` can only ever resolve to the one image that hash matches, regardless of
where the tag itself moves afterward. Bumping any of these pins - base images or kokoros' commit
alike - is a deliberate, re-auditable act, never an implicit side effect of
`docker compose up --build`.

Pinning and non-root solve two different problems. Pinning is about provenance: it makes "what
was audited" and "what's deployed" provably the same thing, until the pin is deliberately bumped
and re-audited. Running as non-root is about blast radius: if a process running as root inside a
container were ever compromised - say, through a bug in a dependency it parses untrusted input
with - the damage it could do is bounded by root's capabilities inside that container's
namespace, which in the default Docker configuration is a meaningfully bigger set of
capabilities than a normal user has. Root inside a container isn't as dangerous as root on the
host (container namespacing still applies), but it's still strictly more capable than a process
whose whole job is running a small HTTP service needs to be. Neither control eliminates a
specific known vulnerability the way the `output_dir` binding does; both are defense-in-depth
that shrinks the blast radius of vulnerabilities that aren't yet known.

## Kokoros container hardening: capabilities and memory

kokoros' own `Cargo.lock` carries the RustSec advisories mentioned above, and it isn't this
repo's source to patch. Instead, compose-level configuration shrinks what a worst-case exploit
of that code could actually do, with no image rebuild required: `security_opt:
no-new-privileges:true`, `cap_drop: ALL`, and `mem_limit: 6g`.

`cap_drop: ALL` removes every Linux capability from the container - things like `CAP_NET_RAW`
(raw socket access) and `CAP_SYS_ADMIN` (a grab-bag of privileged operations) that Docker grants
by default - leaving the process with the bare minimum, confirmed at the kernel level via
`CapEff: 0000000000000000` inside the running container. kokoros' own Dockerfile has no `USER`
directive, so the process still runs as `uid=0` inside its container; `cap_drop` and non-root
are independent controls, and the capability drop holds regardless of that. `no-new-privileges`
closes a second, separate escalation path: even a setuid/setgid binary inside the container
can't use it to gain capabilities back. `mem_limit: 6g` (sized above kokoros' observed idle
usage of ~3.3GiB with 3 instances loaded) bounds how far a memory-safety bug - like the integer
overflow in `bytes` or the out-of-bounds read in `slab` that show up in kokoros' advisories -
could turn into host-level memory exhaustion, rather than letting a single runaway process
affect everything else on the machine.

None of this patches the underlying advisories - a sufficiently determined exploit chain could
still achieve something within that reduced capability set. What it does is collapse the
practical value of most of them: a vulnerability that might otherwise grant broad system access
instead grants access to a process that already has nothing to escalate with, can't gain
privileges back through a local kernel quirk, and is walled off from the network layer entirely
by the `backend` segmentation described above.

## Session isolation is UX, not access control

Per-session isolation (see [Session isolation](architecture.md#session-isolation)) keeps
concurrent sessions' audio and queues from interfering with each other, regardless of which LLM
tool each one belongs to. It is explicitly not an authorization boundary: every session
authenticates with the same shared `LLM_RESPONSE_TTS_BEARER_TOKEN` against the same `ingress`
instance, and a session hash is derived from a directory path, not a secret. Anyone who holds
the token can address, enqueue into, or clear any session's queue by supplying its hash - the
`session_dir` binding above prevents a *mismatched* session/session_dir pair from writing
outside its own tree, but it doesn't prevent a caller who already has the token from choosing
any session hash it wants. That's an acceptable trade-off for a single-user local deployment
where possession of the token is already the entire trust boundary, but session hashes should
not be treated as access control if this is ever exposed beyond `127.0.0.1`.

## Egress: what actually leaves the machine

No data leaves the machine except for the initial model download and the Docker build. kokoros'
Rust workspace has exactly one outbound network call in its entire dependency tree - the ONNX
model and voices file are fetched once from a fixed GitHub Releases URL during the Docker build
and baked into the image, not re-fetched on container start - and no telemetry, analytics, or
update-check dependencies anywhere. The same holds for `ingress` and `worker`: `ingress` only
ever talks to Redis, and `worker`'s only outbound call is to `http://kokoros:3000` on `backend`.
With `backend` marked `internal: true`, this isn't just a property of the current code paths -
it's enforced at the network layer regardless of what future code does. Note that
`docker compose up -d --build` touches the network on every invocation, not just the first,
since it re-clones the build context and re-pulls base images - drop `--build` for routine
restarts if network activity should be limited to true first-time setup.

## Known residual risk

`cargo audit` against kokoros' `Cargo.lock` still surfaces the advisories described above - they
live in a dependency tree this repo doesn't own, so they're contained rather than patched, via
the measures described in the sections above. A kokoros healthcheck that fails loudly if the
model files are missing at startup (rather than silently falling back to `reqwest::get` against
a hardcoded URL) would be a further, belt-and-suspenders complement to the network segmentation
above, not a fix for a currently-reachable gap. `restart: unless-stopped` on every service
provides availability recovery from a crash (e.g. a triggered memory-safety bug) but doesn't
address confidentiality or integrity impact, which is what the containment measures above are
for.
