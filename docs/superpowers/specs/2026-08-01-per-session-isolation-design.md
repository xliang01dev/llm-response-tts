# Per-session isolation design

## Problem

`ingest`, `player`, and the Redis-backed ordering state (`ingress`) are all installed/deployed once,
globally, so the *same* running instance of this pipeline is meant to be usable from any project's
`MessageDisplay` hook, not just from within the `llm-response-tts` repo itself. But today the pipeline
behaves as if there is exactly one conversation happening at a time:

- One global `LLM_RESPONSE_TTS_SOUND_OUTPUT` directory — wav files from any two concurrently-running
  Claude Code sessions (or other LLM CLIs using the same hook protocol) land in the same directory and
  can't be told apart.
- One global `pending_ids` Redis list — every `player` instance polls the same ordering queue, so a
  second session's `player` would compete with the first's for the same ids regardless of which session
  actually generated them.
- One global player lock (`tmp/worker.lock`) — only one `player` process can ever run at a time, so a
  second concurrent session has no player of its own at all.
- One global epoch counter — `clear-speech` run from one session would silently discard in-flight
  synthesis for *every* session, not just its own.

This document describes isolating all of the above per invoking session, identified by the cwd the
session's `ingest`/`player`/`clear-speech` processes run from.

## Goals

- Multiple concurrent sessions (different projects, each with their own cwd) get independent wav output,
  independent playback ordering, and their own `player` process.
- `clear-speech`, run from within a session, only clears that session's pending speech.
- A new `clear-all-speech` tool for the "wipe every session" case.
- `work_queue` (synthesis) and the `next_id` counter stay global/shared — synthesis is inherently
  session-agnostic and parallelized across all 3 workers regardless of which session a job came from.

## Non-goals

- `tmp/` (message buffers, the `ingest-last-message.txt` dedupe marker) stays a single global location,
  not session-scoped. Buffer files are already keyed by `message_id` (assumed globally unique), so there's
  no collision risk there; the dedupe marker has a narrow theoretical race between two sessions finishing
  at the same moment, but that's a pre-existing, unrelated edge case, not something this change addresses.
- No automatic cleanup of old per-session output directories beyond what already applies today (macOS's
  periodic sweep of `/tmp` after a few days).

## Design

### 1. Session identity

Every `ingest`, `player`, `clear-speech`, and `clear-all-speech` invocation derives its session identity
from `std::env::current_dir()` at the moment it runs (inherited down the process chain: the invoking LLM
CLI's cwd → `ingest` → the `player` it spawns; `clear-speech` gets it the same way when run through the
same CLI).

Two derived values, used for different purposes:

- **`session_hash`** — the absolute cwd path hashed with MurmurHash3 (32-bit, x86 variant) and encoded as
  a fixed-width 6-character base62 string (`0-9A-Za-z`; 6 chars covers the full 32-bit space since
  `62^6 > 2^32`), e.g. `3fK9pQ`. Base62 rather than hex to keep it short and URL-shortener-style compact.
  Used everywhere a value needs to be URL/Redis-key-safe: the `session` field in `ingress` requests, and
  all session-scoped Redis keys.
- **`session_dir_name`** — `<session_hash>-<last-path-component-of-cwd>`, e.g. `3fK9pQ-llm-response-tts`.
  Used only for filesystem paths (the per-session output directory and lock directory), where a
  human-readable suffix helps when browsing `/tmp` directly. Never appears in a URL or as a bare Redis key.

Hash implementation: MurmurHash3_x86_32, hand-rolled (not `std::collections::hash_map::DefaultHasher` —
its algorithm is explicitly not guaranteed stable across Rust releases, which matters here since three
separately-compiled binaries must agree on the same hash for the same cwd; and not a `murmur3` crate, to
keep `host/tools` dependency-free). Correctness relative to the "official" MurmurHash3 spec doesn't
actually matter for this use case — nothing outside this project ever needs to reproduce these hashes —
only that it's deterministic, has reasonable distribution to avoid collisions between project directories,
and that the two copies agree byte-for-byte. Lives in `host/tools/src/common.rs` as
`session_key() -> (session_hash: String, session_dir_name: String)`, with `player` keeping its own
identical copy (matching the existing pattern where it already keeps its own `read_env_var` rather than
depending on the `tools` crate).

### 2. Per-session output directory

`LLM_RESPONSE_TTS_SOUND_OUTPUT` (env var, defaults to `/tmp/llm-response-tts/output`) becomes the *parent*
of per-session subdirectories rather than a flat directory: `<LLM_RESPONSE_TTS_SOUND_OUTPUT>/<session_dir_name>/`.

No `docker-compose.yml` changes are needed for this — the existing bind mount already maps the whole
`LLM_RESPONSE_TTS_SOUND_OUTPUT` directory (host path == container path), so subdirectories created
underneath it are automatically visible on both sides. `worker` just needs to `create_dir_all` the specific
per-job `output_dir` before writing, since a session's subdirectory may not exist yet on first use.

### 3. Per-session player lock

Replaces the current single global `tmp/worker.lock` (inside the repo). New location:
`/tmp/llm-response-tts/<session_dir_name>/player.lock` — deliberately under the fixed `/tmp/llm-response-tts`
base rather than `LLM_RESPONSE_TTS_SOUND_OUTPUT`, so lock location stays predictable even if that env var is
ever reconfigured to point somewhere else.

`ingest` checks/spawns per-session now: each invocation attempts to acquire *its own session's* lock (by
spawning `player`, which does the actual `mkdir`-based acquire as today) rather than a single shared one —
so a second concurrent session gets its own `player` process instead of silently deferring to a first
session's player (which wouldn't even see its ids, since ordering is now per-session too).

### 4. Redis schema

| Key | Scope | Change |
| --- | --- | --- |
| `llm-response-tts:next_id` | global | unchanged |
| `llm-response-tts:work_queue` | global | unchanged key/semantics; job payload gains `session` and `output_dir` fields |
| `llm-response-tts:pending_ids` | → `llm-response-tts:pending_ids:<session_hash>` | now per-session |
| `llm-response-tts:epoch` | → `llm-response-tts:epoch:<session_hash>` | now per-session |
| `llm-response-tts:status:<id>` | global | unchanged — ids are already globally unique |
| `llm-response-tts:sessions` | global | **new** — a Set of every `session_hash` ever seen, so `/clear-all` knows what to iterate |

`QueuedJob` (shared shape between `ingress` and `worker`):

```rust
struct QueuedJob {
    id: i64,
    text: String,
    session: String,      // session_hash — new
    output_dir: String,   // new; absolute path, worker writes <id>.wav here verbatim
    epoch: i64,            // unchanged in spirit: the session's epoch value at enqueue time
}
```

`worker`'s existing epoch-fencing check (discard silently if the job's stamped epoch no longer matches
current) is unchanged in mechanism — it just reads `epoch:<job.session>` instead of the single global
`EPOCH_KEY`.

### 5. `ingress` HTTP API

| Endpoint | Change |
| --- | --- |
| `POST /` | request body gains `session` and `output_dir`; `ingress` also `SADD`s `session` into the `sessions` set; response unchanged (`202 {id}`) |
| `GET /next` | gains a required `session` query param; reads `pending_ids:<session>` instead of the global list |
| `POST /ack` | body gains `session`; pops from `pending_ids:<session>` |
| `POST /clear` | body gains `session`; only `DEL`s `pending_ids:<session>` and `INCR`s `epoch:<session>` — no longer touches `work_queue` at all (leaving other sessions' and this session's already-enqueued-but-unpopped jobs alone; the epoch bump is what makes any of *this* session's jobs a worker later pops a no-op) |
| `POST /clear-all` | **new**, no params — `SMEMBERS sessions`, then for each: `DEL pending_ids:<hash>` + `INCR epoch:<hash>`; also `DEL work_queue` outright (a full drain, unlike per-session `/clear`) |

### 6. Binaries

- **`ingest`**: computes `session_hash` + `output_dir` per invocation, sends both when enqueuing, spawns
  `player` against its own session's lock path.
- **`player`**: computes the same `session_hash`/`session_dir_name` independently (inherits cwd from
  `ingest`, unchanged mechanism from today), polls `/next`/`/ack` with `session` attached, reads/deletes
  wavs from its own session's output directory. `IDLE_EXIT` (10s) / `MAX_WAIT` (45s) behavior unchanged,
  just naturally scoped since it only ever sees its own session's ids.
- **`clear-speech`**: unchanged UX (no new CLI args) — computes its own `session_hash`, sends it to
  `POST /clear`.
- **`clear-all-speech`** (new `host/tools/src/bin/clear-all-speech.rs`, installed as
  `llm-response-tts-clear-all-speech`): near-identical to `clear-speech` but calls `POST /clear-all` with
  no session param.

## Example flow

Two Claude Code windows open, one in `~/projects/foo` (`session_hash` = `3fK9pQ`), one in
`~/projects/bar` (`session_hash` = `9mZ2xR`):

1. `foo`'s `ingest` enqueues text → `ingress` pushes id `501` onto `pending_ids:3fK9pQ` and a job
   `{id: 501, session: "3fK9pQ", output_dir: ".../3fK9pQ-foo", epoch: 0}` onto the shared `work_queue`.
2. `bar`'s `ingest` enqueues text moments later → id `502` onto `pending_ids:9mZ2xR`, job similarly
   tagged `session: "9mZ2xR"`.
3. Any of the 3 workers may pop either job in either order — doesn't matter, each writes to its own
   `output_dir`.
4. `foo`'s `player` (spawned by `foo`'s `ingest`, holding the lock at
   `/tmp/llm-response-tts/3fK9pQ-foo/player.lock`) polls `GET /next?session=3fK9pQ`, only ever sees id
   `501`. `bar`'s `player` only ever sees `502`. Neither can steal or block on the other's audio.
5. Running `clear-speech` from within `bar` only empties `pending_ids:9mZ2xR` and bumps
   `epoch:9mZ2xR` — `foo`'s pending speech is untouched.

## Testing / verification plan

- Unit-level: `session_key()` is deterministic for the same cwd across repeated calls and across the
  `tools`/`player` crates' independent implementations (same hash for same input).
- Manual: run two simulated `ingest` invocations from two different temp directories concurrently, confirm
  two `player` processes spawn (two distinct lock paths held), two distinct output subdirectories get
  created, and each session's audio only plays through its own `player`.
- Manual: `clear-speech` from one simulated session while the other has pending audio; confirm only the
  targeted session's queue empties.
- Manual: `clear-all-speech` with both sessions having pending audio; confirm both empty and `work_queue`
  is drained.
