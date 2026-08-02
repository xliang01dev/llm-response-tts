# text-to-speech

Speaks Claude Code's responses out loud using [kokoros](https://github.com/lucasjinreal/kokoros) (a Rust
implementation of Kokoro TTS), served locally in Docker, via a `MessageDisplay` hook.

## How to install

| Tool | Install | Why it's needed |
| --- | --- | --- |
| Rust | `brew install rust` | Provides `cargo`, which builds the four host binaries (`ingest`, `clear-speech`, `clear-all-speech`, `player`) |
| Docker | `brew install --cask docker` | Runs kokoros, Redis, `ingress`, and `worker` via Compose; open Docker Desktop once after installing so its daemon is running |

Run `./setup.sh` to do all of the below in one shot: installs Rust if missing (Docker must already be
installed), creates `docker/.env`, installs the host binaries, builds and starts the Docker stack, and
verifies it responds to a real request. Safe to re-run after a `git pull`. The steps below are what it's
doing, for anyone who wants to run or customize them individually.

1. Create `docker/.env` with a bearer token.

   ```bash
   echo "LLM_RESPONSE_TTS_BEARER_TOKEN=$(openssl rand -hex 32)" > docker/.env
   ```

2. Build and start the stack.

   ```bash
   docker compose up -d --build
   ```

3. Install the host binaries via `cargo install` (re-run after editing their source).

   ```bash
   cargo install --path host/tools --force
   cargo install --path host/player --force
   ```

4. Wire the hook in `.claude/settings.json` (already included in this repo).

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

5. Open Claude Code in this project directory and approve the `.claude/settings.json` trust prompt.

6. Talk to Claude normally; responses should now play back as audio.

### Customizing

| Setting | How to change | Notes |
| --- | --- | --- |
| Voice | Set `KOKOROS_VOICE` on the `worker` service in `docker-compose.yml`, then `docker compose up -d` (no rebuild needed - it's an env var, not baked into the image) | Default `af_heart`; see [available voices](docs/voices.md) |
| Server URL/port | Update the hardcoded `127.0.0.1:3000` in `host/tools/src/bin/ingest.rs`'s `post_text` and `host/player/src/main.rs`'s `BASE_URL`, then reinstall (step 3 above) | Only needed if you change the `nginx` host port mapping in `docker-compose.yml` |

## What it installs

**Host binaries** (built from the `host/` Cargo workspace, run directly on your machine, not in Docker):

| Binary | Installed as | What it does | Crate dependencies |
| --- | --- | --- | --- |
| `ingest` | `llm-response-tts-ingest` | `MessageDisplay` hook entrypoint: buffers streamed deltas, splits into sentences, enqueues each one | none |
| `clear-speech` | `llm-response-tts-clear-speech` | Drops everything queued for the calling session (see [session isolation](docs/architecture.md#session-isolation)) | none |
| `clear-all-speech` | `llm-response-tts-clear-all-speech` | Drops everything queued across every session | none |
| `player` | `llm-response-tts-player` | Plays back one session's synthesized wav files in order | `rodio`, `ureq` |

**Docker services** (built into images, run via `docker-compose.yml`):

| Service | Replicas | What it does |
| --- | --- | --- |
| `kokoros` | 1 | Kokoro-82M TTS model, served over an OpenAI-compatible `/v1/audio/speech` API |
| `redis` | 1 | Work queue and ordering state |
| `ingress` | 1 | Assigns each sentence its ordering id, pushes it onto the Redis work queue |
| `worker` | 3 | Transforms and synthesizes text |
| `nginx` | 1 | Only container with a host-published port; enforces the bearer-token check in front of everything else |

Neither `ingress` nor `worker` runs on the host directly. `docker/` holds their supporting config: nginx's
config template and bearer-token startup check, plus the gitignored `.env` holding the bearer token itself.

**Runtime state created on disk:**

| Path | What lives there |
| --- | --- |
| `/tmp/llm-response-tts/buffer/<session>/` | Per-session message delta buffers and the last-message dedupe marker; safe to delete while idle |
| `LLM_RESPONSE_TTS_SOUND_OUTPUT/<session>/` | Synthesized `.wav` files, one subdirectory per session |
| `/tmp/llm-response-tts/lock/<session>.lock/` | One lock dir per session (with a `pid` file), held by that session's running `player` |

See [architecture](docs/architecture.md) for why sound output lives outside the repo and split per session.

## Env variables

| Name | Set in | What it represents | Default |
| --- | --- | --- | --- |
| `LLM_RESPONSE_TTS_BEARER_TOKEN` | `docker/.env` (created by `setup.sh`, or manually) | Shared secret nginx requires on every request (`Authorization: Bearer <token>`); read by `ingest`, `clear-speech`, `clear-all-speech`, and `player` | none - nginx refuses to start without it |
| `LLM_RESPONSE_TTS_SOUND_OUTPUT` | Host shell env (optional) | Parent directory for synthesized wav files; each session writes/reads under its own `<session-hash>-<name>/` subdirectory - see [session isolation](docs/architecture.md#session-isolation) | `/tmp/llm-response-tts/output` |
| `CARGO_HOME` | Host shell env (optional) | cargo's own install root; `ingest` reads it to find where `player` was installed | `~/.cargo` (cargo's own default when unset) |
| `REDIS_URL` | `docker-compose.yml` (`ingress`, `worker`) | Redis connection string | `redis://redis:6379` |
| `KOKOROS_URL` | `docker-compose.yml` (`worker`) | kokoros TTS server URL | `http://kokoros:3000` |
| `KOKOROS_VOICE` | `docker-compose.yml` (`worker`) | Voice model used for synthesis | `af_heart` |
| `WORD_REFS_PATH` | `docker-compose.yml` (`worker`, not set by default) | Path *inside the worker container* to the word-reference substitutions JSON | `/app/word-references.json` |
| `STRIP_CHARS_PATH` | `docker-compose.yml` (`worker`, not set by default) | Path *inside the worker container* to the strip-characters JSON | `/app/strip-characters.json` |
| `UNITS_PATH` | `docker-compose.yml` (`worker`, not set by default) | Path *inside the worker container* to the measurement-units JSON | `/app/measurement-units.json` |

## More docs

[**Available voices**](docs/voices.md) - the full list of voice names kokoros accepts, beyond the handful
mentioned in "Customizing" above, plus where to look if you need one that isn't listed there.

[**Architecture**](docs/architecture.md) - how a message flows from the hook through `ingest`, `ingress`,
the `worker` pool, and `player`, including diagrams, the per-session isolation design, and how playback
ordering is kept correct despite parallel synthesis.

[**Security audit**](docs/security-audit.md) - what's exposed on the host, how the bearer token is enforced
and fails closed, what network egress this project has, and the trust boundary session isolation does (and
doesn't) provide.
