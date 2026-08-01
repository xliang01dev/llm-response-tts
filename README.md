# text-to-speech

Speaks Claude Code's responses out loud using [kokoros](https://github.com/lucasjinreal/kokoros) (a Rust
implementation of Kokoro TTS), served locally in Docker, via a `MessageDisplay` hook.

## How it works

- `ingest` (compiled from `host/tools/src/bin/ingest.rs`, installed as `llm-response-tts-ingest`) receives
  streamed message deltas from the hook and buffers them per `message_id`. Once the final delta arrives, it
  splits the full text into sentences (on `.`/`!`/`?`/`:`, only when followed by whitespace or end of text,
  so decimals and no-space abbreviations stay intact) and POSTs each one separately to nginx
  (`127.0.0.1:3000`, with the bearer token), which forwards it to the `ingress` service — so a long message
  becomes several small jobs instead of one big one.
- It also dedupes on `tmp/ingest-last-message.txt` so the same message isn't spoken twice.
- `ingress` assigns each sentence its own monotonically increasing id (via Redis `INCR`) and pushes it onto
  a Redis work queue. Three `worker` containers compete for jobs off that queue in parallel, each
  transforming the text — expanding glued number+unit tokens like `24ms` or `512Mi` into spoken words
  (`services/worker/measurement-units.json`), then word references and character stripping, the same logic
  `word-refs.rs` used to apply — and synthesizing it via kokoros's OpenAI-compatible `/v1/audio/speech` API,
  then writing the result to `<id>.wav` in `LLM_RESPONSE_TTS_SOUND_OUTPUT` (defaults to
  `/tmp/llm-response-tts/output`, bind-mounted into the container at that same path — a fixed system
  location rather than something repo-relative, so `worker` and `player` (see below) agree on where wav
  files live without any coordination between them). Splitting by sentence is what lets one long message
  actually use all 3 workers concurrently, rather than one worker synthesizing the whole thing serially.
- The first `ingest` invocation to see an empty worker lock spawns the player binary (compiled from
  `host/player/src/main.rs`, installed via `cargo install` to `~/.cargo/bin/llm-response-tts-player` — see
  Setup step 3 for why it can't just live in this repo like `ingest` and `clear-speech` can) in the
  background, which plays wav files back in strict id order — never whichever one finishes synthesizing
  first — and exits after 10s of nothing left to play.
- Synthesis is entirely local — kokoros runs the Kokoro-82M model in-container and doesn't make any
  outbound network calls, so no audio or text leaves the machine. Redis, `ingress`, and the `worker`
  containers are all internal-only too (no host-published ports), same as kokoros always was.

## Code organization

- `host/` — a Cargo workspace of binaries that run directly on your machine, not in Docker. Two members:
  `tools` (package `llm-response-tts-tools`, zero third-party dependencies) builds `ingest` (from
  `src/bin/ingest.rs`, installed as `llm-response-tts-ingest`) and `clear-speech` (installed as
  `llm-response-tts-clear-speech`) as `src/bin/` targets sharing `host/tools/src/common.rs` via `lib.rs`;
  `player` (package `llm-response-tts-player`, real dependencies — `rodio` for playback, `ureq` for HTTP)
  is kept separate because it needs its own install path (see Setup step 3).
- `services/` — a second Cargo workspace, this one built into Docker images: `ingress` (assigns each
  sentence its ordering id and pushes it onto the Redis work queue) and `worker` (transforms and
  synthesizes text, ×3 replicas). Neither runs on the host directly.
- `docker/` — `docker-compose.yml` support files: nginx's config template and bearer-token startup check,
  plus the gitignored `.env` holding the bearer token itself.
- `tmp/` — gitignored runtime state (message buffers, last-message marker); safe to delete while idle.
  Generated `.wav` files live outside the repo under `LLM_RESPONSE_TTS_SOUND_OUTPUT` instead (defaults to
  `/tmp/llm-response-tts/output`) — see "How it works" above.

## Message queueing

Claude Code can emit several messages back-to-back — sometimes overlapping in time — and each one triggers
its own `ingest` invocation, which itself now splits into multiple sentence-level jobs (see "How it
works" above). Either way, synthesis happens across 3 parallel workers, so a later sentence — from the same
message or a different one — can easily finish before an earlier one; playback still has to happen in the
order Claude actually generated the text, so ordering can no longer be inferred from "whichever wav file
shows up first."

The id `ingress` assigns via Redis `INCR` is the fix: each sentence gets one assigned once, atomically, at
the moment it's accepted, before any parallel processing happens, so it reflects true generation order
regardless of which worker later picks up the job or how long synthesis takes. That same id is also
pushed onto a second Redis list, `llm-response-tts:pending_ids`, purely to track playback order — separate
from `llm-response-tts:work_queue`, which is what the workers pull jobs from.

Ordering state lives entirely in Redis, not in a local file. The player binary doesn't track "the next id" on
disk at all; it just asks `ingress` — `GET /next` peeks the front of `pending_ids` and returns
`{"id", "filename", "status"}`, where `status` is `PROCESSING` or `COMPLETE` depending on whether a worker
has finished writing that id's wav yet (workers report completion into Redis right after they write the
file). the player binary polls that endpoint every couple seconds; once it sees `COMPLETE`, it plays the file
from `LLM_RESPONSE_TTS_SOUND_OUTPUT` via `rodio`, deletes it, and calls `POST /ack` to pop that id off `pending_ids` before
moving to the next one. If an id stays `PROCESSING` for more than 45 seconds (long enough to cover normal
CPU-bound synthesis time, since Docker on macOS has no GPU passthrough — a worker that's actually crashed
mid-job needs this much slack too), it gives up, acks it anyway, and moves on — so one dead job can't
stall everything behind it. With nothing pending at all for 10 seconds, the player binary exits.

Keeping this state server-side instead of in a local watermark file removes a whole category of bugs: a
local file can drift from what Redis actually has queued (e.g. if `tmp/` gets wiped while Redis keeps
counting, or Redis restarts while the local file doesn't) — `pending_ids` can't drift from itself.

Only one instance of the player binary may run at a time, enforced with `mkdir tmp/worker.lock` as before: `mkdir` is atomic
at the filesystem level, so if two `ingest` invocations race to create it, exactly one spawns the
player; the rest just enqueue their message and trust the already-running player to reach it in order.

```mermaid
sequenceDiagram
    participant H as ingest
    participant N as nginx
    participant I as ingress
    participant R as Redis
    participant W as worker (×3)
    participant O as LLM_RESPONSE_TTS_SOUND_OUTPUT
    participant P as player

    loop once per sentence
        H->>N: POST / (Bearer token)
        N->>I: proxy_pass
        I->>R: INCR next_id, LPUSH work_queue, RPUSH pending_ids
        I-->>H: 202 {id}
    end

    par competing consumers
        W->>R: BRPOP work_queue
        W->>W: transform + synthesize (kokoros)
        W->>O: write <id>.wav.tmp, rename to <id>.wav
        W->>R: SET status:<id> COMPLETE
    end

    loop poll-and-play in order
        P->>N: GET /next (Bearer token)
        N->>I: proxy_pass
        I->>R: LINDEX pending_ids 0, check status:<id>
        I-->>P: {id, filename, status}
        P->>O: once COMPLETE, play + delete <filename>
        P->>N: POST /ack {id}
        N->>I: proxy_pass
        I->>R: LPOP pending_ids, DEL status:<id>
    end
```

To stop everything queued (e.g. Claude said something long and you don't want to hear the rest), run `llm-response-tts-clear-speech`. It calls `ingress`'s `POST /clear`, which empties `work_queue` and `pending_ids` — so the player binary sees nothing pending on its next poll — and bumps an epoch counter in Redis so any job a worker already popped and is mid-synthesis gets silently discarded instead of writing a wav nobody will ever ask for. This doesn't interrupt whatever's playing on the host right now, only what would've come after it; the player binary blocks until playback finishes before its next poll, so cutting off mid-sentence would need a different design.

## Prerequisites

Install via Homebrew:

```bash
brew install rust
```

- **rust** — provides `cargo`, which builds all three host binaries (`llm-response-tts-ingest`,
  `llm-response-tts-clear-speech`, `llm-response-tts-player`) from the `host/` Cargo workspace. Audio
  playback is via the `rodio` crate, so no external player binary like `ffplay` is needed.
- **Docker** (with Compose) — runs the kokoros TTS server plus Redis and the queue/worker services. See
  `docker-compose.yml`.

## Setup

Run `./setup.sh` to do all of the below in one shot — installs rust via Homebrew if missing, creates
`docker/.env` if missing, installs all three host binaries via `cargo install`, makes sure `~/.cargo/bin`
is actually on `PATH` (Homebrew's `rust` formula doesn't add it there the way `rustup` would), builds and
starts the Docker stack, and verifies it actually responds to a real enqueue request before declaring
success. If nginx was already running from a previous `setup.sh` run and ends up holding a stale connection
to a container that just got rebuilt, the verification step recreates nginx and retries once rather than
silently leaving you with a stack that looks up but doesn't actually work. Safe to re-run after a
`git pull` to rebuild everything; it won't touch an existing `docker/.env`. The steps below are what it's
doing, for anyone who wants to run or customize them individually.

1. Create `docker/.env` with a bearer token used to authenticate requests to the TTS server:

   ```bash
   echo "LLM_RESPONSE_TTS_BEARER_TOKEN=$(openssl rand -hex 32)" > docker/.env
   ```

2. Start the stack (builds native images for kokoros, `ingress`, and `worker` on first run):

   ```bash
   docker compose up -d --build
   ```

3. Install the host-side Rust binaries (rebuild after editing the source or upgrading rustc/cargo). `host/`
   is a Cargo workspace with two members: `tools` (package `llm-response-tts-tools`, zero third-party
   dependencies — builds `ingest` and `clear-speech`, sharing `host/tools/src/common.rs`) and `player`
   (package `llm-response-tts-player`, real dependencies). Both are installed via `cargo install`, which
   always lands in `~/.cargo/bin` — a fixed, global location, so the hook (step 4) can reference a binary by
   name alone rather than a path tied to this specific checkout:

   ```bash
   cargo install --path host/tools --force
   cargo install --path host/player --force
   ```

   This matters even more for `player`: it must run from the boot volume. If this repo lives on a non-boot
   volume (e.g. an external or secondary drive, as `/Volumes/...` paths do on macOS), the OS kills
   CoreAudio-linked binaries (`player` links `cpal` for audio output) executed from that volume with
   `SIGKILL (Code Signature Invalid)` — a restriction that doesn't apply to `ingest` and `clear-speech`,
   neither of which has such a dependency, but `cargo install` sidesteps it for all three either way since
   `~/.cargo/bin` is always on the boot volume. All three are named `llm-response-tts-*` rather than their
   bare role names (`ingest`, `clear-speech`, `player`) since `~/.cargo/bin` is a global directory shared
   with every other cargo tool on the machine — same reasoning as the `llm-response-tts-*` container names
   in `docker-compose.yml`.

   That `cargo install` above is the **release** path — the one `setup.sh` runs, and the one most people
   need. There's also a separate **dev** path for `player` specifically, `host/player/build.sh`, for when
   you're actively editing its source: it builds player and, if this repo isn't already on the boot volume,
   copies the result to `/tmp/llm-response-tts/llm-response-tts-player` instead of touching `~/.cargo/bin`.
   The reason it's a separate `/tmp` copy rather than just re-running `cargo install --force` is that
   `~/.cargo/bin` may currently be backing an already-running, working `player` process — you don't want
   every work-in-progress test build to clobber the stable install. `/tmp` is also on the boot volume and
   gets auto-purged by macOS after a few days, so it's a natural disposable staging spot. `ingest` resolves
   player in priority order: the `LLM_RESPONSE_TTS_PLAYER_BIN` env var if set (an explicit override for
   pointing at a debug build somewhere else entirely), then `/tmp/llm-response-tts/llm-response-tts-player`
   (the dev build, if present), then `~/.cargo/bin/llm-response-tts-player` (the release install) — so a
   dev build automatically takes priority when one exists, with no configuration needed. Either way,
   `ingest` passes the repo's actual location to player via a `LLM_RESPONSE_TTS_ROOT` environment variable
   when it spawns it, so player still finds `tmp/`, `docker/.env`, etc. correctly.

4. Wire the hook in `.claude/settings.json` (project-level, already included in this repo):

   ```json
   {
     "hooks": {
       "MessageDisplay": [
         {
           "matcher": "",
           "hooks": [
             {
               "type": "command",
               "command": "llm-response-tts-ingest"
             }
           ]
         }
       ]
     }
   }
   ```

   This references the binary by name alone rather than a path into this checkout, so it works regardless
   of where the repo lives — as long as `~/.cargo/bin` is on `PATH` (step 3's `setup.sh` run checks this and
   fixes it if needed; if you skip `setup.sh`, make sure of it yourself, or the hook will silently do
   nothing).

5. Open Claude Code in this project directory. On first run it will prompt you to trust the project's
   `.claude/settings.json` since hooks execute arbitrary shell commands — approve it.

6. Talk to Claude normally; responses should now play back as audio.

## Customizing

- **Voice**: change the `KOKOROS_VOICE` environment variable on the `worker` service in
  `docker-compose.yml` to any voice supported by kokoros (e.g. `af_sky`, `af_bella`, `bm_daniel`,
  `bm_george`), then `docker compose up -d` to pick it up (no rebuild needed — it's an env var, not baked
  into the image).
- **Server URL/port**: if you change the host port mapping in `docker-compose.yml`'s `nginx` service,
  update the hardcoded `127.0.0.1:3000` in `host/tools/src/bin/ingest.rs`'s `post_text` function and
  `host/player/src/main.rs`'s `BASE_URL`, then recompile both (see Setup step 3).

## Security

Only `nginx` is bound to the host, and only on `127.0.0.1:3000` — kokoros, Redis, `ingress`, and all 3
`worker` containers publish no host port at all (`expose` only in `docker-compose.yml`), reachable
solely from other containers on the compose-internal `llm-response-tts-net` network. That's a network-layer
restriction enforced by the OS before any request-level logic runs, which is a stronger guarantee than
CORS (a browser-only convention that doesn't stop non-browser clients or even see the underlying TCP
connection). nginx also requires every request to carry `Authorization: Bearer <token>` matching
`LLM_RESPONSE_TTS_BEARER_TOKEN` from `docker/.env` (see `docker/nginx/templates/default.conf.template`,
which now proxies to `ingress` instead of talking to kokoros directly), so nothing else on the machine can
enqueue a message without the token either. `ingest` and the player binary both read that same `.env` file
and attach the token automatically — `ingest` to enqueue, the player binary to poll `/next` and call `/ack`
— and the file is gitignored, so each machine needs its own. Once a message is past `ingress`,
nothing else in the pipeline re-checks the token — `worker` calling kokoros directly is trusted the same
way nginx calling kokoros used to be, because it's still all on the private internal network only trusted
containers can reach.

nginx is the only authentication layer in this deployment — neither kokoros nor `ingress` implements auth
of its own — so its startup is required to fail closed rather than fail open. `docker/.env` is gitignored
and therefore always local to a given machine; if it were ever missing or misconfigured, the server must
refuse to start rather than come up in a state that quietly accepts requests. Two checks enforce that:
the nginx service's `env_file` is marked `required: true`, so Compose refuses to start nginx at all if
`docker/.env` doesn't exist, and `docker/nginx/entrypoint.d/25-check-bearer-token.sh` runs as part of
nginx's own startup sequence to verify the token was actually substituted into the rendered config,
aborting startup if it wasn't.

The full run path was also audited for network egress, to confirm no data leaves the machine except the
initial model download and the Docker build. The kokoros Rust workspace has exactly one outbound network
call in its entire dependency tree — the ONNX model and voices file are fetched once from a fixed GitHub
Releases URL during the Docker build and baked into the image, not re-fetched on container start — and no
telemetry, analytics, or update-check dependencies anywhere. The same holds for `ingress` and `worker`:
`ingress` only ever talks to Redis, and `worker`'s only outbound call is to `http://kokoros:3000` on the
internal network — neither has a code path that can reach anywhere outside `llm-response-tts-net`. Note that
`docker compose up -d --build` (as used in step 2 above) touches the network on every invocation, not just
the first, since it re-clones the build context and re-pulls base images — drop `--build` for routine
restarts if you want network activity limited to true first-time setup.

## Notes

- `tmp/` holds runtime state (buffers and the last-message marker) and is gitignored — safe to delete at
  any time while idle. Generated `.wav` files live outside the repo, under `LLM_RESPONSE_TTS_SOUND_OUTPUT`
  (defaults to `/tmp/llm-response-tts/output`) instead — see "How it works" above for why. There's no local
  playback watermark to worry about anymore; ordering lives in Redis, so a `tmp/` wipe, a
  `LLM_RESPONSE_TTS_SOUND_OUTPUT` wipe, or a `redis` restart just means whatever was mid-flight gets lost,
  not a stuck or drifted player.
