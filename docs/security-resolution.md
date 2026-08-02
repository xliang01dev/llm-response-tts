# Security audit findings — draft (not yet committed)

**Status:** #1, #2, #3, #4, #5a, and #5b applied and live-verified (see notes in each section
below); not yet committed to git. #5c is a proposed follow-up, not yet applied. #5d needs no
action (already in place).

Scope: `llm-response-tts` repo (this project) plus the kokoros checkout at
`/Volumes/Files/Development/Tooling/ai/claude_code/Kokoros`. Tests were run live against the
running stack where possible (curl/TCP probes, an isolated nginx-config reproduction,
`cargo audit` against every Rust lockfile in scope), not just inferred from reading config.

This draft focuses resolution ideas on issues **we can actually fix from this repo** - i.e.
not the vulnerabilities living inside kokoros' own dependency tree, which live in someone
else's `Cargo.lock` and aren't ours to patch directly.

---

## Fixable from this repo

### 1. kokoros' Docker build source is unpinned (High) - ✅ APPLIED

**Test:** read `docker-compose.yml` line 5, compared against the local kokoros checkout's
`git remote`/`git log -1`.

**Finding:** `build: https://github.com/lucasjinreal/kokoros.git` has no `#ref` suffix, so
Docker clones the upstream default branch's current HEAD on every `--build`. The local
checkout at `/Volumes/.../claude_code/Kokoros` (HEAD `b54354b`, 2026-06-19) is a disconnected
snapshot - it is not provably what's actually deployed.

**Why it matters:** every downstream claim about kokoros (its dependency list, its CORS
default, its lack of runtime egress) is only as trustworthy as whatever upstream's HEAD
happens to be at build time. No pinning = no reproducibility, no defense against a
compromised or unexpectedly-changed upstream push.

**Resolution idea:**
```yaml
# docker-compose.yml
kokoros:
  build: https://github.com/lucasjinreal/kokoros.git#b54354b860df94b064e254524777ce41d4d2c689
```
Pin to the commit that was actually audited (or whichever commit you choose to standardize
on). Re-audit and bump the pin deliberately when you want to pick up upstream changes,
rather than silently getting a new build every time.

**Effort:** trivial, one line.

**Applied:** `docker-compose.yml`'s `kokoros.build` now pins
`#b54354b860df94b064e254524777ce41d4d2c689`. Rebuilt (`docker compose up -d --build`) and
verified the stack still enqueues/synthesizes/plays correctly afterward.

---

### 2. `output_dir` is never validated against `session` (Medium) - ✅ APPLIED

**Test:** read `services/ingress/src/main.rs::enqueue()` and `services/worker/src/main.rs`'s
job-processing loop.

**Finding:** `EnqueueRequest { text, session, output_dir }` - `ingress` stores `output_dir`
verbatim into the `QueuedJob` it pushes, and `worker` writes to it verbatim
(`PathBuf::from(&job.output_dir)`). Nothing checks that `output_dir` actually corresponds to
`session`; they're independently trusted from the client.

**Why it matters:** an already-authenticated caller (anyone with the bearer token - the
existing trust boundary) could send `session: "<some other session's hash>"` paired with an
arbitrary `output_dir`, writing into or interfering with another session's directory tree.
Sharpens the doc's existing "session hash isn't a secret" note - it's not just that the hash
is guessable, nothing binds the two fields together server-side at all.

**Resolution idea:** stop trusting `output_dir` from the client. Derive it server-side in
`ingress` from `session` plus the known sound-output base, the same way `ingest` and `player`
already do:
```rust
// ingress/src/main.rs - enqueue()
// instead of trusting req.output_dir directly:
let output_dir = format!("{}/{}", SOUND_OUTPUT_BASE, req.session);
```
This requires `ingress` to know `LLM_RESPONSE_TTS_SOUND_OUTPUT` (it doesn't today - currently
only `ingest`/`player`/`worker` read it). Alternative if you want to keep the client-supplied
path (e.g. to preserve flexibility for non-default output locations per caller): validate
`output_dir` is a lexical subpath of `LLM_RESPONSE_TTS_SOUND_OUTPUT/<session>` and reject the
request (400) otherwise.

**Effort:** small - one new env read in `ingress`, or one path-prefix check.

**Applied (refined from the idea above):** using bare `session` as the directory name would
have broken `player`, which reads from `<hash>-<cwd-folder-name>` (the human-readable form
from `session_key()`), not the bare hash. Instead, `ingest` now sends a second field,
`session_dir` (that human-readable name), and `ingress` validates it server-side before using
it - must be a single path component (no `/`, no `..`) and must equal `session` or start with
`"{session}-"`, so it can't be used to target another session's directory or escape via
traversal. `ingress` now reads `LLM_RESPONSE_TTS_SOUND_OUTPUT` itself (added to its
`environment:` block in `docker-compose.yml`) and derives `output_dir` server-side from
`sound_output_base()` + validated `session_dir`. `worker` is unchanged - it still just gets a
computed `output_dir` in the job payload. Live-verified: a mismatched `(session, session_dir)`
pair and a `session_dir: "../../etc"` traversal attempt both now get `400`; a correctly
paired request still gets `202` and worker writes to the expected path.

---

### 3. Our own containers run as root on an unpinned base image (Medium) - ✅ APPLIED

**Test:** read `services/ingress/Dockerfile`, `services/worker/Dockerfile`, and the `nginx`
service definition in `docker-compose.yml`.

**Finding:** `ingress` and `worker` both use `debian:sid-slim` (Debian's *unstable* rolling
branch, not a vetted stable release) with no `USER` directive in either Dockerfile, so both
run as root. `nginx:alpine` is also an unpinned floating tag.

**Why it matters:** if either service ever had a memory-safety or RCE-class bug exploited
(low likelihood given they're pure-Rust and now confirmed to have zero known vulnerable
dependencies, but not impossible via a future dependency), running as root maximizes the
blast radius inside the container. `sid` also means untested/unvetted package combinations
compared to a stable release.

**Resolution idea:**
```dockerfile
# services/ingress/Dockerfile and services/worker/Dockerfile
FROM debian:bookworm-slim AS runner
...
RUN useradd --system --no-create-home appuser
USER appuser
ENTRYPOINT ["./ingress"]   # or ./worker
```
Pin `nginx:alpine` to a specific version tag (e.g. `nginx:1.27-alpine`) or a digest
(`nginx:alpine@sha256:...`) instead of the floating `alpine` tag.

**Effort:** small - a few lines per Dockerfile; verify the binaries still have the
permissions/paths they need as a non-root user (should be fine, they don't bind privileged
ports or need root).

**Applied:** both `services/ingress/Dockerfile` and `services/worker/Dockerfile` now use
`debian:bookworm-slim` (Debian's current stable release) with a `useradd --system
--no-create-home appuser` + `USER appuser`. `docker-compose.yml`'s `nginx.image` pinned to
`nginx:1.31.3-alpine` (the version that was actually already running - confirmed the tag
exists via `docker pull` before using it). Rebuilt and live-verified: `docker exec ...
whoami` returns `appuser` in both containers, `worker` (non-root) still successfully writes
to the bind-mounted `/tmp/llm-response-tts/output` (no permission issue under Docker
Desktop's macOS bind-mount handling), and the full enqueue → synthesize → auth-check path
still works end-to-end.

---

### 4. Fail-closed nginx check misses the empty-token case (Medium, low likelihood) - ✅ APPLIED

**Test:** reproduced in isolation with `envsubst` against the real template (did not touch
the live stack) - rendered `docker/nginx/templates/default.conf.template` with
`LLM_RESPONSE_TTS_BEARER_TOKEN=""`, then ran the actual check script's logic
(`docker/nginx/entrypoint.d/25-check-bearer-token.sh`) against the result.

**Finding:** the check greps for the literal `${LLM_RESPONSE_TTS_BEARER_TOKEN}` placeholder
string to confirm substitution happened - it passes as soon as substitution occurs, even if
the substituted value is empty. An empty token renders as
`if ($http_authorization != "Bearer ") { return 401; }`, which anyone can satisfy by sending
`Authorization: Bearer ` (literal trailing space, no token).

**Why it matters:** low likelihood in practice - both `setup.sh` and the manual install step
always generate a real 256-bit token via `openssl rand -hex 32`, so this only triggers if
someone hand-edits `docker/.env` to an empty value. Live-tested against the actual running
token (confirmed non-empty): current enforcement is correct. This is a latent gap in the
defense-in-depth guarantee, not an active issue today.

**Resolution idea:**
```sh
# docker/nginx/entrypoint.d/25-check-bearer-token.sh
#!/bin/sh
set -e

if grep -Rq '${LLM_RESPONSE_TTS_BEARER_TOKEN}' /etc/nginx/conf.d/ 2>/dev/null; then
  echo "$0: LLM_RESPONSE_TTS_BEARER_TOKEN was not substituted into the nginx config (missing from docker/.env?). Refusing to start with an unauthenticated bearer-token check." >&2
  exit 1
fi

if grep -Rq 'Bearer "' /etc/nginx/conf.d/ 2>/dev/null; then
  echo "$0: LLM_RESPONSE_TTS_BEARER_TOKEN substituted to an empty value. Refusing to start with a bypassable bearer-token check." >&2
  exit 1
fi
```
(The second check looks for the literal rendered pattern `Bearer "` immediately followed by
the closing quote, i.e. an empty value baked into the `if` condition.)

**Effort:** trivial, a few lines.

**Applied:** `docker/nginx/entrypoint.d/25-check-bearer-token.sh` now has the second check as
written above. Verified three ways: (1) isolated repro with an empty token now correctly
refuses to start (previously it would have started); (2) isolated repro with the real
non-empty token does *not* false-positive; (3) recreated the live nginx container with the
fixed script and the real token - it starts normally and the stack still enqueues correctly.

---

### 5. Reducing the *impact* of kokoros' vulnerabilities (without touching kokoros' source)

We can't patch kokoros' `Cargo.lock` from here, but we control the container it runs in and
the network it sits on - both are real levers for shrinking what those vulnerabilities can
actually do, independent of when/whether upstream fixes them.

**5a. Network-segment kokoros (and redis/ingress/worker) off the internet entirely.** Today
every service shares one flat network (`docker-compose.yml`'s `default`, named
`llm-response-tts-net`), which is a normal bridge (`internal: false` - confirmed via
`docker network inspect`) with a route out. Splitting into an edge network (host-facing,
just `nginx`) and an internal-only backend network neutralizes the `rustls-webpki`
advisories specifically: even in the narrow case where kokoros' `reqwest::get` fallback
fires (see the informational note above), it would have no route to the internet at all, so
the vulnerable cert/CRL-parsing code path can never actually execute against real network
data.
```yaml
# docker-compose.yml
networks:
  edge:
  backend:
    internal: true

services:
  nginx:
    networks: [edge, backend]
    ports:
      - "127.0.0.1:3000:3000"
  ingress:
    networks: [backend]
  redis:
    networks: [backend]
  worker:
    networks: [backend]
  kokoros:
    networks: [backend]
```
**✅ APPLIED.** `docker-compose.yml` now defines `edge` (nginx only, host-published port) and
`backend` (`internal: true`; redis, ingress, worker, kokoros) as written above. Verified live
after `docker compose down && docker compose up -d --build` (network changes need a full
recreate, not just `restart`):
- `docker network inspect llm-response-tts-backend` confirms `"Internal": true`;
  `llm-response-tts-edge` confirms `"Internal": false`.
- nginx's published port still works end-to-end: authenticated curl against
  `http://127.0.0.1:3000` returned `202`.
- `worker` can still reach `kokoros` over `backend`: `docker compose logs worker` showed
  multiple `wrote output for id N` lines, i.e. synthesis requests round-tripped successfully.
- `backend`'s egress is genuinely blocked: since kokoros' own image has no curl/wget to test
  from inside it directly, egress was proven at the network level instead with ephemeral
  `alpine` containers - `docker run --rm --network llm-response-tts-edge alpine wget -T5 -O
  /dev/null http://1.1.1.1` succeeded (control, proving the test itself works), while the
  identical command against `--network llm-response-tts-backend` failed with `Network
  unreachable`. Since kokoros/redis/ingress/worker are backend-only, this failure mode applies
  to all of them.
- `cargo test --manifest-path host/Cargo.toml` still passes (no regression from the network
  changes).

**✅ APPLIED.** `docker-compose.yml`'s kokoros service now has:
```yaml
kokoros:
  ...
  security_opt:
    - no-new-privileges:true
  cap_drop:
    - ALL
  mem_limit: 6g
```
`mem_limit` was set from observed usage (`docker stats` showed ~3.3GiB/7.75GiB at idle with 3
instances loaded), giving headroom above normal usage while still bounding a runaway process.
Verified live:
- `docker exec llm-response-tts-kokoros sh -c 'cat /proc/self/status | grep CapEff'` returned
  `CapEff: 0000000000000000` - zero effective capabilities, confirming `cap_drop: ALL` is
  genuinely in effect at the kernel level.
- The same container is still `uid=0(root)` (kokoros' own Dockerfile has no `USER` directive
  and isn't ours to edit) - `cap_drop: ALL` and non-root are independent axes; this shows the
  capability drop holds regardless of the uid.
- Synthesis still works end-to-end post-hardening (see the `worker` log check under 5a above),
  confirming `cap_drop: ALL` didn't break anything kokoros actually needs to do.

`read_only: true` + `tmpfs: [/tmp]` would be the next incremental step (blocks writes anywhere
the process wasn't explicitly given a writable mount), but needs checking against what kokoros
actually writes to at runtime beyond the `./docker/tmp:/app/tmp` mount already in place - not
applied yet, don't want to silently break it without confirming first.

**5c. Fail loud instead of silently falling back to a network call.** Right now if
`checkpoints`/`data` were ever missing at container start (e.g. a bad volume mount), kokoros
would silently attempt `reqwest::get` against a hardcoded GitHub URL. A `depends_on` healthcheck
or a startup assertion that those paths are non-empty turns a silent network fallback into an
explicit, loud failure - complementary to 5a (which would just make that fallback fail
anyway), belt-and-suspenders so a misconfiguration is caught immediately rather than masked.

**5d. Already-present mitigation worth noting:** `restart: unless-stopped` on the kokoros
service means a crash from a memory-safety bug (e.g. if `bytes`/`slab`'s issues were ever
triggered) gets auto-recovered rather than causing a lasting outage - partial mitigation for
availability impact, doesn't address confidentiality/integrity impact.

**Effort:** 5a and 5b are both applied and verified above. 5c is a proposed follow-up, not yet
implemented. 5d requires no change - already in place.

---

## Not fixable from this repo (kokoros' own dependency tree)

`cargo audit` against the local kokoros checkout's `Cargo.lock` (304 crates) found 7 known
RustSec advisories (`bytes` integer overflow, `rustls-webpki` x3 cert/CRL parsing bugs,
`slab` OOB access + yanked, `tracing-subscriber` log/ANSI injection) plus 2 unmaintained
crates (`audiopus_sys`, `number_prefix`). These live in kokoros' own `Cargo.lock`, not ours -
we can't patch them directly. The levers we have from this repo instead:

- **Reduce impact via containment** - see item 5 above (network segmentation, capability
  dropping, memory limits). This is the main lever: it shrinks what these specific
  vulnerabilities can actually do without waiting on an upstream fix.
- **Pin the build source** (see #1 above) so we at least know exactly which version of these
  vulnerabilities we're running, instead of an unpinned moving target.
- **Track upstream**: watch the kokoros repo for a `cargo update` / dependency bump, and bump
  our pinned commit when one lands.
- **Fork if it matters enough**: if a fix is wanted sooner than upstream ships one, fork
  kokoros, run `cargo update -p <crate>` ourselves, and point the pinned build source at the
  fork instead.

None of these are exploitable in the current trust model as far as I could verify (kokoros
makes no outbound TLS calls at request time, so the `rustls-webpki` bugs specifically aren't
reachable here), but they're real, tracked issues sitting in a component that processes 100%
of the audio pipeline.

---

## Confirmed clean (no action needed)

- This repo's own dependency trees: `host/Cargo.lock` (189 crates) and `services/Cargo.lock`
  (167 crates) - zero known vulnerabilities, zero unmaintained/yanked warnings via
  `cargo audit`.
- Only one `unsafe` block anywhere in our own Rust code (`libc::kill(pid, 0)` in `player`,
  process-liveness check) - narrow, standard, justified.
- `redis` (6379) and `ingress` (3001) are genuinely unreachable from the host - live-verified
  via curl and raw TCP connect, not just absent from `docker-compose.yml`.
- nginx's published port is bound to `127.0.0.1` specifically (`docker inspect` + `lsof`),
  not `0.0.0.0` - not reachable from other devices on the LAN.
- Every route and HTTP method tested (`POST /`, `GET /next`, `POST /clear-all`, `OPTIONS`,
  `HEAD`) correctly requires the token; only the correct token gets 202.
- `ingest`'s hand-rolled HTTP client JSON-escapes text/session/output_dir before placing them
  in a `Content-Length`-bounded body - no CRLF/header-injection path from message content.
- `session_dir_name` comes from `cwd.file_name()`, a single OS path component - no
  path-traversal vector via project folder naming.
- `docker/.env`/`.env` are gitignored and confirmed never committed in git history.
- Bearer token generation (`openssl rand -hex 32`) is 256 bits of CSPRNG entropy.

---

## Minor / informational (kokoros, not something we control)

- kokoros-openai's server has `CorsLayer::permissive()` unconditionally on. Irrelevant today
  (kokoros has no host-published port in our compose file), but would matter immediately if
  anyone ever added a `ports:` mapping for it directly - worth a warning comment near that
  service definition so nobody does this without knowing.
- kokoros' only outbound HTTP call (`reqwest::get` in `fileio.rs`) is reachable solely via the
  model/voices download path with hardcoded GitHub URLs, and only fires if those files are
  missing at container startup - normally dormant since the Dockerfile pre-fetches them at
  build time. Worth softening the doc's current absolute "not re-fetched on container start"
  phrasing to reflect this narrow, self-healing exception.
