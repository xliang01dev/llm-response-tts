# Security Audit results

Only `nginx` is bound to the host, and only on `127.0.0.1:3000` - kokoros, Redis, `ingress`, and all 3
`worker` containers publish no host port at all (`expose` only in `docker-compose.yml`), reachable
solely from other containers on the compose-internal `llm-response-tts-net` network. That's a network-layer
restriction enforced by the OS before any request-level logic runs, which is a stronger guarantee than
CORS (a browser-only convention that doesn't stop non-browser clients or even see the underlying TCP
connection). nginx also requires every request to carry `Authorization: Bearer <token>` matching
`LLM_RESPONSE_TTS_BEARER_TOKEN` from `docker/.env` (see `docker/nginx/templates/default.conf.template`,
which proxies to `ingress` instead of talking to kokoros directly), so nothing else on the machine can
enqueue a message without the token either. `ingest` and `player` both read that same `.env` file and
attach the token automatically - `ingest` to enqueue, `player` to poll `/next` and call `/ack` - and the
file is gitignored, so each machine needs its own. Once a message is past `ingress`, nothing else in the
pipeline re-checks the token - `worker` calling kokoros directly is trusted the same way nginx calling
kokoros used to be, because it's still all on the private internal network only trusted containers can
reach.

nginx is the only authentication layer in this deployment - neither kokoros nor `ingress` implements auth
of its own - so its startup is required to fail closed rather than fail open. `docker/.env` is gitignored
and therefore always local to a given machine; if it were ever missing or misconfigured, the server must
refuse to start rather than come up in a state that quietly accepts requests. Two checks enforce that:
the nginx service's `env_file` is marked `required: true`, so Compose refuses to start nginx at all if
`docker/.env` doesn't exist, and `docker/nginx/entrypoint.d/25-check-bearer-token.sh` runs as part of
nginx's own startup sequence to verify the token was actually substituted into the rendered config,
aborting startup if it wasn't.

The full run path was also audited for network egress, to confirm no data leaves the machine except the
initial model download and the Docker build. The kokoros Rust workspace has exactly one outbound network
call in its entire dependency tree - the ONNX model and voices file are fetched once from a fixed GitHub
Releases URL during the Docker build and baked into the image, not re-fetched on container start - and no
telemetry, analytics, or update-check dependencies anywhere. The same holds for `ingress` and `worker`:
`ingress` only ever talks to Redis, and `worker`'s only outbound call is to `http://kokoros:3000` on the
internal network - neither has a code path that can reach anywhere outside `llm-response-tts-net`. Note
that `docker compose up -d --build` (as used when starting the stack - see the main
[README](../README.md#how-to-install)) touches the network on every invocation, not just the first, since
it re-clones the build context and re-pulls base images - drop `--build` for routine restarts if you want
network activity limited to true first-time setup.

Per-session isolation (see [Session isolation](architecture.md#session-isolation)) is an organizational and
UX boundary, not a security one: it keeps concurrent Claude Code sessions' audio and queues from interfering
with each other, but every session still authenticates with the same shared
`LLM_RESPONSE_TTS_BEARER_TOKEN` against the same `ingress` instance. A session hash is derived from a
directory path, not a secret, and isn't meant to be one - anyone who can reach `ingress` (i.e. anyone with
the bearer token) can address, enqueue into, or clear any other session's queue simply by supplying its
hash. That's an acceptable trade-off for a single-user local deployment where the token itself is already
the trust boundary, but it means session hashes shouldn't be treated as access control if this is ever
exposed beyond `127.0.0.1`.
