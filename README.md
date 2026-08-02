# LLM Text to Voice

Speaks LLM agent responses out loud using [kokoros](https://github.com/lucasjinreal/kokoros) (a Rust implementation of Kokoro TTS), served locally in Docker. The queue, worker pool, and player are LLM-agnostic - any tool that can stream its response text to `ingest` can use them. This repo ships the integration for Claude Code's `MessageDisplay` hook out of the box; Kiro, Codex, or anything else that speaks LLM can wire in the same way.

## Table of contents

- [Prerequisites](#prerequisites)
- [How to install](#how-to-install)
- [How to verify install succeeded](#how-to-verify-install-succeeded)
- [How to hook up to a coding agent](#how-to-hook-up-to-a-coding-agent)
  - [Claude Code](#claude-code)
  - [Other coding agents](#other-coding-agents)
- [How to customize voices](#how-to-customize-voices)
- [How to change voice speed](#how-to-change-voice-speed)
- [How is code tested](#how-is-code-tested)
- [How to uninstall](#how-to-uninstall)
- [Supporting documentation](#supporting-documentation)
  - [Available voices](#available-voices)
  - [Architecture](#architecture)
  - [Environment variables config](#environment-variables-config)
  - [Security audit](#security-audit)

## Prerequisites

| Tool | Install | Why it's needed |
| --- | --- | --- |
| Rust | `brew install rust` | Provides `cargo`, which builds the four host binaries (`ingest`, `clear-speech`, `clear-all-speech`, `player`) |
| Docker | `brew install --cask docker` | Runs kokoros, Redis, `ingress`, and `worker` via Compose; open Docker Desktop once after installing so its daemon is running |

## How to install

Install and start everything in one shot. Safe to re-run after a `git pull`.

```bash
./setup.sh
```

If you want to manually install the components, follow these instructions:

1. Create `docker/.env` with a bearer token.

   ```bash
   echo "LLM_RESPONSE_TTS_BEARER_TOKEN=$(openssl rand -hex 32)" > docker/.env
   ```

2. Build and start the stack.

   ```bash
   docker compose up -d --build
   ```

   If this repo isn't on your boot volume (e.g. it's on an external or secondary drive, as `/Volumes/...` paths are on macOS), Docker Desktop bind-mounts several of its files into containers (see [Architecture](docs/architecture.md)), so macOS will prompt for permission to access that volume - approve it.

3. Install the host binaries via `cargo install` (re-run after editing their source).

   ```bash
   cargo install --path host/tools --force
   cargo install --path host/player --force
   ```

At this point the pipeline is installed and running, but nothing is driving it automatically yet - see "How to hook up to a coding agent" below to wire one in.

## How to verify install succeeded

Turn your speakers on, then send a test message straight to `ingest` to confirm the whole pipeline works end to end.

```bash
echo '{"final":true,"message_id":"test-1","delta":"Hello world"}' | llm-response-tts-ingest
```

You should hear "Hello world" spoken back within a few seconds.

## How to hook up to a coding agent

The queue, worker pool, and player are LLM-agnostic - `ingest` is the only piece that's specific to whichever tool is calling it, and all it needs is streamed response text on stdin as it's generated. Any coding agent that can invoke a command per streamed chunk (or otherwise be made to) can drive this pipeline the same way Claude Code does below.

### Claude Code

This repo ships Claude Code's integration out of the box, via its `MessageDisplay` hook.

1. Wire the hook in `.claude/settings.json` (already included in this repo).

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

2. Open Claude Code in this project directory and approve the `.claude/settings.json` trust prompt.

3. Talk to Claude normally; responses should now play back as audio.

### Other coding agents

Kiro, Codex, or any other tool with an equivalent hook mechanism can wire `llm-response-tts-ingest` into their own settings the same way, to get responses converted to voice.

## How to customize voices

Set `KOKOROS_VOICE` on the `worker` service in `docker-compose.yml`, then `docker compose up -d` (no rebuild needed - it's an env var, not baked into the image):

```yaml
worker:
  environment:
    KOKOROS_VOICE: af_heart # change to any voice from docs/voices.md
```

Default is `af_heart`; see [available voices](docs/voices.md) for the full list.

## How to change voice speed

Set `LLM_RESPONSE_TTS_PLAYBACK_SPEED` in your host shell - e.g. `export LLM_RESPONSE_TTS_PLAYBACK_SPEED=1.5` for 1.5x. No rebuild or restart needed, since `player` reads it fresh on every launch. It's a playback-rate multiplier, not a pitch-preserving tempo change, so speeding up also raises pitch a bit (see [Local env vars](docs/env-vars.md#local-env-vars)).

## How is code tested

Both Cargo workspaces (`host/`, `services/`) have unit test coverage for their non-trivial logic (the hand-rolled JSON parser and sentence splitter in `ingest`, the word-reference/measurement text transforms in `worker`, `ingress`'s `session_dir` validation, `player`'s file-lock acquire/reclaim, and the shared HTTP/env-var helpers in `host/tools`), living in a sibling `*_tests.rs` next to each source file rather than inline.

```bash
./scripts/test.sh   # builds and tests both host/ and services/
```

`./setup.sh` runs `git config core.hooksPath .githooks` so `.githooks/pre-commit` - which just calls the script above - blocks a commit if the build or any test fails. Run it manually any time with `./scripts/test.sh`.

## How to uninstall

It removes:

- The Docker stack (`docker compose down` - stops and removes the containers and network, not the built images)
- The host binaries (`cargo uninstall` for `ingest`, `clear-speech`, `clear-all-speech`, and `player`)
- The pre-commit hook wiring (`git config --unset core.hooksPath`)

```bash
./uninstall.sh
```

It leaves a few things in place, since removing them is either destructive (your bearer token) or touches state outside the repo (your shell rc, `/tmp`) - remove these manually if you want a completely clean slate:

- `docker/.env`, your bearer token

  ```bash
  rm docker/.env
  ```

- The PATH line `setup.sh` may have added to your `.zshrc`/`.bash_profile`/`config.fish` - remove it by hand if you don't use `~/.cargo/bin` for anything else
- Runtime state under `/tmp/llm-response-tts` (queued audio, buffers, player locks)

  ```bash
  rm -rf /tmp/llm-response-tts
  ```

## Supporting documentation

### Available voices

Link to full [Available voices](docs/voices.md) document

The full list of voice names kokoros accepts, beyond the handful mentioned in "How to customize voices" above, plus where to look if you need one that isn't listed there.

### Architecture

Link to full [Architecture](docs/architecture.md) document

How a message flows from the hook through `ingest`, `ingress`, the `worker` pool, and `player`, including diagrams, the per-session isolation design, and how playback ordering is kept correct despite parallel synthesis. Also covers what gets installed where (host binaries, Docker services) and what runtime state each one leaves on disk.

### Environment variables config

Link to full [Environment variables config](docs/env-vars.md) document

Every environment variable this repo reads, split into what configures the Docker services versus what configures the host binaries running on your machine - and why those two are kept separate.

### Security audit

Link to full [Security audit](docs/security-audit.md) document

The architecture is designed so that what your LLM tool says back to you never has to leave your machine: a bearer token gates every request, and everything except `nginx` sits on an internal-only Docker network with no route out. The full write-up covers the network posture, container hardening, and what per-session isolation does and doesn't protect against.
