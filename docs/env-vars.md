# Environment variables

## Docker env vars

Config for the containerized services - set in `docker-compose.yml` (or, for the bearer token,
`docker/.env`), never in your host shell.

| Name | Set in | What it represents | Default |
| --- | --- | --- | --- |
| `KOKOROS_URL` | `docker-compose.yml` (`worker`) | kokoros TTS server URL | `http://kokoros:3000` |
| `KOKOROS_VOICE` | `docker-compose.yml` (`worker`) | Voice model used for synthesis | `af_heart` |
| `LLM_RESPONSE_TTS_BEARER_TOKEN` | `docker/.env` (created by `setup.sh`, or manually) | Bearer token nginx requires on every request; also read by the host binaries so they can attach it | none - nginx refuses to start without it |
| `REDIS_URL` | `docker-compose.yml` (`ingress`, `worker`) | Redis connection string | `redis://redis:6379` |
| `STRIP_CHARS_PATH` | `docker-compose.yml` (`worker`, not set by default) | In-container path to the strip-characters JSON | `/app/strip-characters.json` |
| `UNITS_PATH` | `docker-compose.yml` (`worker`, not set by default) | In-container path to the measurement-units JSON | `/app/measurement-units.json` |
| `WORD_REFS_PATH` | `docker-compose.yml` (`worker`, not set by default) | In-container path to the word-reference JSON | `/app/word-references.json` |

## Local env vars

Config for the host binaries (`ingest`, `clear-speech`, `clear-all-speech`, `player`) - all
optional. Not `docker/.env`, which is Compose's own config file for provisioning containers, not
a general settings file for anything running on the host.

| Name | Set in | What it represents | Default |
| --- | --- | --- | --- |
| `CARGO_HOME` | Host shell (`.zshrc`, `.bashrc`, a local `.env` you source, etc.) | cargo's install root, used to locate `player` | `~/.cargo` (cargo's own default when unset) |
| `LLM_RESPONSE_TTS_PLAYBACK_SPEED` | Host shell (`.zshrc`, `.bashrc`, a local `.env` you source, etc.) | Playback speed multiplier (also shifts pitch) | `1.0` |
| `LLM_RESPONSE_TTS_SOUND_OUTPUT` | Host shell (`.zshrc`, `.bashrc`, a local `.env` you source, etc.) | Parent directory for synthesized wav files - see [session isolation](architecture.md#session-isolation) | `/tmp/llm-response-tts/output` |
