# Per-Session Isolation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give every concurrently-running session (identified by the cwd of whatever process invoked `ingest`) its own wav output directory, playback ordering, and `player` process, so two Claude Code windows in different projects never have their audio conflated or compete for the same queue.

**Architecture:** A `session_hash` (MurmurHash3_x86_32 of the invoking cwd, base62-encoded to 6 chars) becomes the routing key for a new set of per-session Redis structures (`pending_ids:<session>`, `epoch:<session>`) and a per-session output directory/lock. The existing shared `work_queue` and `next_id` counter are untouched — synthesis stays global/parallel across all 3 workers; each job just carries `session` and `output_dir` fields so `worker` knows where to write and which epoch to check.

**Tech Stack:** Rust (host binaries: `ingest`, `clear-speech`, new `clear-all-speech`, `player`; services: `ingress` on axum, `worker` on tokio), Redis, Docker Compose. No existing test framework in this repo — verification is either `#[test]` unit tests for pure functions (new for this change) or manual integration checks (curl / `redis-cli` / process inspection), matching how every other feature in this repo has been verified so far.

## Global Constraints

- `host/tools` (crate `llm-response-tts-tools`, builds `ingest`/`clear-speech`/`clear-all-speech`) has **zero third-party dependencies** — do not add any crate to its `Cargo.toml`.
- `host/player` keeps its own copies of anything it needs from `host/tools` rather than depending on that crate (established pattern — see its existing `read_env_var` copy). The new hash/base62/`session_key` functions follow the same rule.
- No code comments explaining *what* code does (names should do that) — only *why*, when non-obvious. Match the terse, no-narration style already in every file touched here.
- After any change to a host binary (`ingest`, `clear-speech`, `clear-all-speech`, `player`): rebuild with `cargo build --release --manifest-path host/Cargo.toml`, reinstall with `cargo install --path host/tools --force` and/or `cargo install --path host/player --force`, and if you're iterating on `player` specifically, also refresh the dev copy with `bash host/player/build.sh` (otherwise `ingest` will keep spawning the stale one — this exact failure mode happened earlier in this project's history).
- After any change to `services/ingress` or `services/worker`: `docker compose up -d --build ingress worker` (or just the one service that changed).
- All Redis keys are prefixed `llm-response-tts:` — keep that prefix on any new key.

---

## Task 1: MurmurHash3 + base62 `session_key()` in `host/tools/src/common.rs`

**Files:**
- Modify: `host/tools/src/common.rs`

**Interfaces:**
- Produces: `pub fn session_key() -> (String, String)` — returns `(session_hash, session_dir_name)`. `session_hash` is a 6-character base62 string. `session_dir_name` is `"{session_hash}-{last_path_component_of_cwd}"`.
- Produces: `pub fn sound_output_base() -> std::path::PathBuf` — reads `LLM_RESPONSE_TTS_SOUND_OUTPUT` env var, defaults to `/tmp/llm-response-tts/output`.

- [ ] **Step 1: Write the failing tests**

Add to the bottom of `host/tools/src/common.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn murmurhash3_of_empty_input_is_zero() {
        assert_eq!(murmurhash3_x86_32(b"", 0), 0);
    }

    #[test]
    fn murmurhash3_is_deterministic() {
        let a = murmurhash3_x86_32(b"/Users/xliang/projects/foo", 0);
        let b = murmurhash3_x86_32(b"/Users/xliang/projects/foo", 0);
        assert_eq!(a, b);
    }

    #[test]
    fn murmurhash3_differs_for_different_input() {
        let a = murmurhash3_x86_32(b"/Users/xliang/projects/foo", 0);
        let b = murmurhash3_x86_32(b"/Users/xliang/projects/bar", 0);
        assert_ne!(a, b);
    }

    #[test]
    fn to_base62_of_zero_is_all_zero_chars() {
        assert_eq!(to_base62(0, 6), "000000");
    }

    #[test]
    fn to_base62_roundtrips_small_value() {
        // 125 = 2*62 + 1 -> digits [2, 1] in base62, left-padded to width 6
        assert_eq!(to_base62(125, 6), "000021");
    }

    #[test]
    fn to_base62_is_fixed_width_and_valid_alphabet() {
        for n in [0u32, 1, 61, 62, 125, u32::MAX] {
            let s = to_base62(n, 6);
            assert_eq!(s.len(), 6);
            assert!(s.bytes().all(|b| BASE62_ALPHABET.contains(&b)));
        }
    }

    #[test]
    fn session_key_dir_name_starts_with_the_hash() {
        let (hash, dir_name) = session_key();
        assert_eq!(hash.len(), 6);
        assert!(dir_name.starts_with(&format!("{hash}-")));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path host/Cargo.toml -p llm-response-tts-tools`
Expected: FAIL to compile — `murmurhash3_x86_32`, `to_base62`, `BASE62_ALPHABET`, `session_key` not defined.

- [ ] **Step 3: Implement**

Add above the `#[cfg(test)]` block in `host/tools/src/common.rs` (after the existing `read_env_var` function):

```rust
const BASE62_ALPHABET: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

// Hand-rolled rather than std::collections::hash_map::DefaultHasher (its algorithm isn't
// guaranteed stable across Rust releases, which matters since ingest, clear-speech, and
// player - three separately-compiled binaries - all need to agree on the same hash for the
// same cwd) or a murmur3 crate (this crate stays dependency-free). Correctness relative to
// the "official" MurmurHash3 spec doesn't matter here - nothing outside this project ever
// needs to reproduce these hashes - only that it's deterministic and well-distributed enough
// to avoid collisions between project directories.
fn murmurhash3_x86_32(data: &[u8], seed: u32) -> u32 {
    const C1: u32 = 0xcc9e2d51;
    const C2: u32 = 0x1b873593;

    let mut hash = seed;
    let chunks = data.chunks_exact(4);
    let tail = chunks.remainder();

    for chunk in chunks {
        let mut k = u32::from_le_bytes(chunk.try_into().unwrap());
        k = k.wrapping_mul(C1);
        k = k.rotate_left(15);
        k = k.wrapping_mul(C2);
        hash ^= k;
        hash = hash.rotate_left(13);
        hash = hash.wrapping_mul(5).wrapping_add(0xe6546b64);
    }

    let mut k1: u32 = 0;
    for (i, &byte) in tail.iter().enumerate() {
        k1 ^= (byte as u32) << (8 * i);
    }
    if !tail.is_empty() {
        k1 = k1.wrapping_mul(C1);
        k1 = k1.rotate_left(15);
        k1 = k1.wrapping_mul(C2);
        hash ^= k1;
    }

    hash ^= data.len() as u32;
    hash ^= hash >> 16;
    hash = hash.wrapping_mul(0x85ebca6b);
    hash ^= hash >> 13;
    hash = hash.wrapping_mul(0xc2b2ae35);
    hash ^= hash >> 16;

    hash
}

fn to_base62(mut n: u32, width: usize) -> String {
    let mut chars = Vec::with_capacity(width);
    loop {
        chars.push(BASE62_ALPHABET[(n % 62) as usize]);
        n /= 62;
        if n == 0 {
            break;
        }
    }
    while chars.len() < width {
        chars.push(BASE62_ALPHABET[0]);
    }
    chars.reverse();
    String::from_utf8(chars).unwrap()
}

pub fn session_key() -> (String, String) {
    let cwd = std::env::current_dir().expect("failed to get current dir");
    let cwd_str = cwd.to_string_lossy();
    let session_hash = to_base62(murmurhash3_x86_32(cwd_str.as_bytes(), 0), 6);
    let last_component = cwd
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "root".to_string());
    (session_hash.clone(), format!("{session_hash}-{last_component}"))
}

pub fn sound_output_base() -> std::path::PathBuf {
    std::env::var("LLM_RESPONSE_TTS_SOUND_OUTPUT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("/tmp/llm-response-tts/output"))
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --manifest-path host/Cargo.toml -p llm-response-tts-tools`
Expected: PASS (7 tests)

- [ ] **Step 5: Commit**

```bash
git add host/tools/src/common.rs
git commit -m "Add session_key() and sound_output_base() to host/tools common.rs"
```

---

## Task 2: Duplicate hash/base62/`session_key` into `host/player/src/main.rs`

**Files:**
- Modify: `host/player/src/main.rs`

**Interfaces:**
- Consumes: nothing from Task 1 (deliberately duplicated, not imported — `player` is a separate crate that doesn't depend on `host/tools`).
- Produces: same two functions as Task 1, private to this crate: `session_key() -> (String, String)`, `sound_output_base() -> PathBuf`.

- [ ] **Step 1: Write the failing tests**

Add to the bottom of `host/player/src/main.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn murmurhash3_of_empty_input_is_zero() {
        assert_eq!(murmurhash3_x86_32(b"", 0), 0);
    }

    #[test]
    fn to_base62_of_zero_is_all_zero_chars() {
        assert_eq!(to_base62(0, 6), "000000");
    }

    #[test]
    fn to_base62_roundtrips_small_value() {
        assert_eq!(to_base62(125, 6), "000021");
    }

    #[test]
    fn session_key_dir_name_starts_with_the_hash() {
        let (hash, dir_name) = session_key();
        assert_eq!(hash.len(), 6);
        assert!(dir_name.starts_with(&format!("{hash}-")));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --manifest-path host/Cargo.toml -p llm-response-tts-player`
Expected: FAIL to compile — functions not defined.

- [ ] **Step 3: Implement**

Add near the top of `host/player/src/main.rs`, after the existing `use` statements (before `const POLL_INTERVAL`):

```rust
const BASE62_ALPHABET: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

// Identical logic to host/tools/src/common.rs::murmurhash3_x86_32 - duplicated rather than
// shared, since player is a separate crate that keeps its own small copies of anything it
// needs from tools (see its existing read_env_var). The two copies must stay byte-for-byte
// identical for ingest and player to agree on the same session_hash for the same cwd.
fn murmurhash3_x86_32(data: &[u8], seed: u32) -> u32 {
    const C1: u32 = 0xcc9e2d51;
    const C2: u32 = 0x1b873593;

    let mut hash = seed;
    let chunks = data.chunks_exact(4);
    let tail = chunks.remainder();

    for chunk in chunks {
        let mut k = u32::from_le_bytes(chunk.try_into().unwrap());
        k = k.wrapping_mul(C1);
        k = k.rotate_left(15);
        k = k.wrapping_mul(C2);
        hash ^= k;
        hash = hash.rotate_left(13);
        hash = hash.wrapping_mul(5).wrapping_add(0xe6546b64);
    }

    let mut k1: u32 = 0;
    for (i, &byte) in tail.iter().enumerate() {
        k1 ^= (byte as u32) << (8 * i);
    }
    if !tail.is_empty() {
        k1 = k1.wrapping_mul(C1);
        k1 = k1.rotate_left(15);
        k1 = k1.wrapping_mul(C2);
        hash ^= k1;
    }

    hash ^= data.len() as u32;
    hash ^= hash >> 16;
    hash = hash.wrapping_mul(0x85ebca6b);
    hash ^= hash >> 13;
    hash = hash.wrapping_mul(0xc2b2ae35);
    hash ^= hash >> 16;

    hash
}

fn to_base62(mut n: u32, width: usize) -> String {
    let mut chars = Vec::with_capacity(width);
    loop {
        chars.push(BASE62_ALPHABET[(n % 62) as usize]);
        n /= 62;
        if n == 0 {
            break;
        }
    }
    while chars.len() < width {
        chars.push(BASE62_ALPHABET[0]);
    }
    chars.reverse();
    String::from_utf8(chars).unwrap()
}

fn session_key() -> (String, String) {
    let cwd = std::env::current_dir().expect("failed to get current dir");
    let cwd_str = cwd.to_string_lossy();
    let session_hash = to_base62(murmurhash3_x86_32(cwd_str.as_bytes(), 0), 6);
    let last_component = cwd
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "root".to_string());
    (session_hash.clone(), format!("{session_hash}-{last_component}"))
}

fn sound_output_base() -> PathBuf {
    std::env::var("LLM_RESPONSE_TTS_SOUND_OUTPUT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp/llm-response-tts/output"))
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --manifest-path host/Cargo.toml -p llm-response-tts-player`
Expected: PASS (4 tests)

- [ ] **Step 5: Commit**

```bash
git add host/player/src/main.rs
git commit -m "Add duplicated session_key()/hash/base62 to player"
```

---

## Task 3: `ingress` — per-session Redis schema, `/clear-all`

**Files:**
- Modify: `services/ingress/src/main.rs`

**Interfaces:**
- Produces (HTTP contract, consumed by Tasks 5-7):
  - `POST /` body `{"text": string, "session": string, "output_dir": string}` → `202 {"id": i64}` (unchanged response shape)
  - `GET /next?session=<string>` → `200 {"id": i64, "filename": string, "status": "PROCESSING"|"COMPLETE"}` or `204` empty
  - `POST /ack` body `{"id": i64, "session": string}` → `204`
  - `POST /clear` body `{"session": string}` → `204`
  - `POST /clear-all` no body → `204`
- Produces (Redis contract, consumed by Task 4's `worker`): jobs pushed onto `llm-response-tts:work_queue` are JSON `{"id": i64, "text": string, "session": string, "output_dir": string, "epoch": i64}`; per-session epoch lives at `llm-response-tts:epoch:<session>`.

This is a full rewrite of the file — no unit test framework exists for it and adding one (mocking Redis) would be disproportionate; verify with the manual `curl`/`redis-cli` steps below instead, matching how every other part of this project has been verified.

- [ ] **Step 1: Replace the file contents**

Replace all of `services/ingress/src/main.rs` with:

```rust
use axum::{
    extract::{Query, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use redis::aio::ConnectionManager;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

#[derive(Clone)]
struct AppState {
    redis: ConnectionManager,
}

#[derive(Deserialize)]
struct EnqueueRequest {
    text: String,
    session: String,
    output_dir: String,
}

#[derive(Serialize)]
struct EnqueueResponse {
    id: i64,
}

#[derive(Serialize)]
struct QueuedJob {
    id: i64,
    text: String,
    session: String,
    output_dir: String,
    epoch: i64,
}

#[derive(Serialize)]
struct NextResponse {
    id: i64,
    filename: String,
    status: &'static str,
}

#[derive(Deserialize)]
struct SessionQuery {
    session: String,
}

#[derive(Deserialize)]
struct AckRequest {
    id: i64,
    session: String,
}

#[derive(Deserialize)]
struct ClearRequest {
    session: String,
}

const NEXT_ID_KEY: &str = "llm-response-tts:next_id";
const WORK_QUEUE_KEY: &str = "llm-response-tts:work_queue";
const SESSIONS_KEY: &str = "llm-response-tts:sessions";

fn pending_ids_key(session: &str) -> String {
    format!("llm-response-tts:pending_ids:{session}")
}

fn epoch_key(session: &str) -> String {
    format!("llm-response-tts:epoch:{session}")
}

fn wav_filename(id: i64) -> String {
    format!("{:010}.wav", id)
}

fn status_key(id: i64) -> String {
    format!("llm-response-tts:status:{id}")
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let redis_url = std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://redis:6379".to_string());
    let client = redis::Client::open(redis_url).expect("invalid REDIS_URL");
    let redis = ConnectionManager::new(client)
        .await
        .expect("failed to connect to redis on startup");

    let app = Router::new()
        .route("/", post(enqueue))
        .route("/next", get(next))
        .route("/ack", post(ack))
        .route("/clear", post(clear))
        .route("/clear-all", post(clear_all))
        .with_state(AppState { redis });

    let addr = SocketAddr::from(([0, 0, 0, 0], 3001));
    tracing::info!("ingress listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

async fn enqueue(
    State(mut state): State<AppState>,
    Json(req): Json<EnqueueRequest>,
) -> Result<(StatusCode, Json<EnqueueResponse>), StatusCode> {
    if req.text.trim().is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    let id: i64 = state
        .redis
        .incr(NEXT_ID_KEY, 1)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;

    let epoch: i64 = state
        .redis
        .get(epoch_key(&req.session))
        .await
        .unwrap_or(None)
        .unwrap_or(0);

    let payload = serde_json::to_string(&QueuedJob {
        id,
        text: req.text,
        session: req.session.clone(),
        output_dir: req.output_dir,
        epoch,
    })
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    redis::pipe()
        .atomic()
        .cmd("LPUSH").arg(WORK_QUEUE_KEY).arg(payload).ignore()
        .cmd("RPUSH").arg(pending_ids_key(&req.session)).arg(id).ignore()
        .cmd("SADD").arg(SESSIONS_KEY).arg(&req.session).ignore()
        .query_async::<()>(&mut state.redis)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;

    Ok((StatusCode::ACCEPTED, Json(EnqueueResponse { id })))
}

async fn next(
    State(mut state): State<AppState>,
    Query(q): Query<SessionQuery>,
) -> Result<Json<NextResponse>, StatusCode> {
    let id: Option<i64> = state
        .redis
        .lindex(pending_ids_key(&q.session), 0)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;

    let Some(id) = id else {
        return Err(StatusCode::NO_CONTENT);
    };

    let complete: bool = state
        .redis
        .exists(status_key(id))
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;

    Ok(Json(NextResponse {
        id,
        filename: wav_filename(id),
        status: if complete { "COMPLETE" } else { "PROCESSING" },
    }))
}

async fn ack(State(mut state): State<AppState>, Json(req): Json<AckRequest>) -> StatusCode {
    let popped: Option<i64> = state
        .redis
        .lpop(pending_ids_key(&req.session), None)
        .await
        .unwrap_or(None);
    if popped != Some(req.id) {
        tracing::warn!("ack mismatch: requested {}, popped {:?}", req.id, popped);
    }
    let _: Result<(), _> = state.redis.del(status_key(req.id)).await;
    StatusCode::NO_CONTENT
}

// Drops everything not yet playing *for this session*: clears its ordering list so player's
// next poll sees nothing pending, and bumps its epoch so any job a worker already popped (and
// is mid-synthesis) gets silently discarded instead of writing an orphaned wav nobody will ever
// ask for. work_queue itself is left alone - it's shared across sessions now, and the epoch
// bump is what neutralizes this session's still-queued-but-unpopped jobs once a worker gets to
// them. Whatever's already playing on the host finishes on its own - this only stops what comes
// after it.
async fn clear(State(mut state): State<AppState>, Json(req): Json<ClearRequest>) -> StatusCode {
    let result: Result<(), _> = redis::pipe()
        .atomic()
        .cmd("INCR").arg(epoch_key(&req.session)).ignore()
        .cmd("DEL").arg(pending_ids_key(&req.session)).ignore()
        .query_async::<()>(&mut state.redis)
        .await;

    match result {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(_) => StatusCode::SERVICE_UNAVAILABLE,
    }
}

// Same as clear(), but for every session that's ever enqueued something, plus a full
// work_queue drain - safe here specifically because every session's epoch is bumped in the
// same pipeline, so any job any worker pops afterward (regardless of which session it's
// tagged with) gets silently discarded anyway.
async fn clear_all(State(mut state): State<AppState>) -> StatusCode {
    let sessions: Vec<String> = match state.redis.smembers(SESSIONS_KEY).await {
        Ok(s) => s,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE,
    };

    let mut pipe = redis::pipe();
    pipe.atomic().cmd("DEL").arg(WORK_QUEUE_KEY).ignore();
    for session in &sessions {
        pipe.cmd("INCR").arg(epoch_key(session)).ignore();
        pipe.cmd("DEL").arg(pending_ids_key(session)).ignore();
    }

    match pipe.query_async::<()>(&mut state.redis).await {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(_) => StatusCode::SERVICE_UNAVAILABLE,
    }
}
```

- [ ] **Step 2: Rebuild and restart ingress**

Run: `cd /Volumes/Files/Development/Tooling/ai/personal/llm-response-tts && docker compose up -d --build ingress`
Expected: builds and starts without error; `docker compose logs ingress --tail 5` shows `ingress listening on 0.0.0.0:3001`.

- [ ] **Step 3: Manually verify via curl through nginx**

Get the bearer token value first (don't print it — assign to a shell variable):
```bash
cd /Volumes/Files/Development/Tooling/ai/personal/llm-response-tts
TOKEN=$(grep LLM_RESPONSE_TTS_BEARER_TOKEN docker/.env | cut -d= -f2-)
```

Enqueue with session metadata:
```bash
curl -s -w '\n%{http_code}\n' -X POST -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  -d '{"text":"plan verification","session":"abc123","output_dir":"/tmp/llm-response-tts/output/abc123-test"}' \
  http://127.0.0.1:3000/
```
Expected: `202` with a JSON body containing an `id`.

Poll for it, scoped to that session:
```bash
curl -s -w '\n%{http_code}\n' -H "Authorization: Bearer $TOKEN" \
  'http://127.0.0.1:3000/next?session=abc123'
```
Expected: `200` with `{"id":<same id>,"filename":"...","status":"PROCESSING"}` (worker isn't wired to this schema yet at this point in the plan, so it'll stay `PROCESSING` forever — that's fine, this task only verifies `ingress`).

Poll a *different* session — should see nothing:
```bash
curl -s -w '\n%{http_code}\n' -H "Authorization: Bearer $TOKEN" \
  'http://127.0.0.1:3000/next?session=different'
```
Expected: `204`, empty body.

Clear the first session, confirm it's gone:
```bash
curl -s -w '\n%{http_code}\n' -X POST -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  -d '{"session":"abc123"}' http://127.0.0.1:3000/clear
curl -s -w '\n%{http_code}\n' -H "Authorization: Bearer $TOKEN" 'http://127.0.0.1:3000/next?session=abc123'
```
Expected: `clear` returns `204`; the follow-up `/next` now returns `204` (empty) too.

- [ ] **Step 4: Commit**

```bash
git add services/ingress/src/main.rs
git commit -m "ingress: per-session pending_ids/epoch, add POST /clear-all"
```

---

## Task 4: `worker` — per-job `output_dir`/`session`, `docker-compose.yml` cleanup

**Files:**
- Modify: `services/worker/src/main.rs`
- Modify: `docker-compose.yml`

**Interfaces:**
- Consumes: the `QueuedJob` JSON shape from Task 3 (`id`, `text`, `session`, `output_dir`, `epoch`).

- [ ] **Step 1: Update `QueuedJob` and remove the flat `EPOCH_KEY`**

In `services/worker/src/main.rs`, replace:
```rust
const STATUS_TTL_SECS: u64 = 3600;
const WORK_QUEUE_KEY: &str = "llm-response-tts:work_queue";
const EPOCH_KEY: &str = "llm-response-tts:epoch";

fn status_key(id: i64) -> String {
    format!("llm-response-tts:status:{id}")
}

#[derive(Deserialize)]
struct QueuedJob {
    id: i64,
    text: String,
    epoch: i64,
}
```
with:
```rust
const STATUS_TTL_SECS: u64 = 3600;
const WORK_QUEUE_KEY: &str = "llm-response-tts:work_queue";

fn status_key(id: i64) -> String {
    format!("llm-response-tts:status:{id}")
}

fn epoch_key(session: &str) -> String {
    format!("llm-response-tts:epoch:{session}")
}

#[derive(Deserialize)]
struct QueuedJob {
    id: i64,
    text: String,
    session: String,
    output_dir: String,
    epoch: i64,
}
```

- [ ] **Step 2: Move output-dir handling from startup to per-job**

Replace the `main()` variable setup:
```rust
    let redis_url = env_or("REDIS_URL", "redis://redis:6379");
    let kokoros_url = env_or("KOKOROS_URL", "http://kokoros:3000");
    let voice = env_or("KOKOROS_VOICE", "af_heart");
    let output_dir = PathBuf::from(env_or("LLM_RESPONSE_TTS_SOUND_OUTPUT", "/tmp/llm-response-tts/output"));
    let word_refs_path = env_or("WORD_REFS_PATH", "/app/word-references.json");
    let strip_chars_path = env_or("STRIP_CHARS_PATH", "/app/strip-characters.json");
    let units_path = env_or("UNITS_PATH", "/app/measurement-units.json");

    std::fs::create_dir_all(&output_dir).expect("failed to create output dir");
```
with (drops the now-session-specific `output_dir` and its startup `create_dir_all` — each job carries its own):
```rust
    let redis_url = env_or("REDIS_URL", "redis://redis:6379");
    let kokoros_url = env_or("KOKOROS_URL", "http://kokoros:3000");
    let voice = env_or("KOKOROS_VOICE", "af_heart");
    let word_refs_path = env_or("WORD_REFS_PATH", "/app/word-references.json");
    let strip_chars_path = env_or("STRIP_CHARS_PATH", "/app/strip-characters.json");
    let units_path = env_or("UNITS_PATH", "/app/measurement-units.json");
```

- [ ] **Step 3: Use the job's own session/output_dir in the main loop**

Replace:
```rust
        let text = apply_transform(&job.text, &units, &refs, &strip_set);
        match synthesize(&http, &kokoros_url, &voice, &text).await {
            Ok(bytes) => {
                let current_epoch: i64 = conn.get(EPOCH_KEY).await.unwrap_or(None).unwrap_or(0);
                if current_epoch != job.epoch {
                    tracing::info!("id {} cleared mid-job (epoch {} -> {}), discarding", job.id, job.epoch, current_epoch);
                    continue;
                }
                if let Err(e) = write_output(&output_dir, job.id, &bytes) {
                    tracing::error!("failed to write output for id {}: {e}", job.id);
                } else if let Err(e) = conn.set_ex::<_, _, ()>(status_key(job.id), "COMPLETE", STATUS_TTL_SECS).await {
                    tracing::error!("failed to mark id {} complete: {e}", job.id);
                } else {
                    tracing::info!("wrote output for id {}", job.id);
                }
            }
            Err(e) => tracing::error!("synthesis failed for id {}: {e}", job.id),
        }
```
with:
```rust
        let text = apply_transform(&job.text, &units, &refs, &strip_set);
        match synthesize(&http, &kokoros_url, &voice, &text).await {
            Ok(bytes) => {
                let current_epoch: i64 = conn.get(epoch_key(&job.session)).await.unwrap_or(None).unwrap_or(0);
                if current_epoch != job.epoch {
                    tracing::info!("id {} cleared mid-job (epoch {} -> {}), discarding", job.id, job.epoch, current_epoch);
                    continue;
                }
                let output_dir = PathBuf::from(&job.output_dir);
                if let Err(e) = std::fs::create_dir_all(&output_dir) {
                    tracing::error!("failed to create output dir {} for id {}: {e}", job.output_dir, job.id);
                    continue;
                }
                if let Err(e) = write_output(&output_dir, job.id, &bytes) {
                    tracing::error!("failed to write output for id {}: {e}", job.id);
                } else if let Err(e) = conn.set_ex::<_, _, ()>(status_key(job.id), "COMPLETE", STATUS_TTL_SECS).await {
                    tracing::error!("failed to mark id {} complete: {e}", job.id);
                } else {
                    tracing::info!("wrote output for id {}", job.id);
                }
            }
            Err(e) => tracing::error!("synthesis failed for id {}: {e}", job.id),
        }
```

- [ ] **Step 4: Simplify `docker-compose.yml`'s worker service**

`worker` no longer reads `LLM_RESPONSE_TTS_SOUND_OUTPUT` itself (every job now carries its own absolute `output_dir`), so the env var and the YAML anchor built around it are dead. The bind mount is still required — it's what makes any `output_dir` a job carries (always a subdirectory of the host's `/tmp/llm-response-tts/output` by construction, since `ingest`/`player` build it from `sound_output_base()`) actually writable from inside the container.

Replace:
```yaml
    environment:
      REDIS_URL: redis://redis:6379
      KOKOROS_URL: http://kokoros:3000
      KOKOROS_VOICE: af_heart
      LLM_RESPONSE_TTS_SOUND_OUTPUT: &sound_output /tmp/llm-response-tts/output
    volumes:
      - ./services/worker/word-references.json:/app/word-references.json:ro
      - ./services/worker/strip-characters.json:/app/strip-characters.json:ro
      - ./services/worker/measurement-units.json:/app/measurement-units.json:ro
      # Bind-mounted at the identical path inside the container (not the usual /app/...)
      # so the host path matches LLM_RESPONSE_TTS_SOUND_OUTPUT's default in player/worker
      # verbatim - player (on the host) and worker (in this container) then agree on
      # where wav files live without any coordination between them.
      - type: bind
        source: *sound_output
        target: *sound_output
```
with:
```yaml
    environment:
      REDIS_URL: redis://redis:6379
      KOKOROS_URL: http://kokoros:3000
      KOKOROS_VOICE: af_heart
    volumes:
      - ./services/worker/word-references.json:/app/word-references.json:ro
      - ./services/worker/strip-characters.json:/app/strip-characters.json:ro
      - ./services/worker/measurement-units.json:/app/measurement-units.json:ro
      # Bind-mounted at the identical path inside the container (not the usual /app/...) so
      # any output_dir a job carries (always a subdirectory of this - built by ingest/player
      # from LLM_RESPONSE_TTS_SOUND_OUTPUT, default /tmp/llm-response-tts/output) resolves to
      # the same real files whether worker or player accesses it.
      - /tmp/llm-response-tts/output:/tmp/llm-response-tts/output
```

- [ ] **Step 5: Rebuild and restart worker**

Run: `cd /Volumes/Files/Development/Tooling/ai/personal/llm-response-tts && docker compose up -d --build worker`
Expected: 3 worker containers recreated and running (`docker compose ps` shows `worker-1`/`2`/`3` Up).

- [ ] **Step 6: Manually verify by pushing a crafted job directly onto Redis**

```bash
cd /Volumes/Files/Development/Tooling/ai/personal/llm-response-tts
docker compose exec redis redis-cli LPUSH llm-response-tts:work_queue \
  '{"id":999001,"text":"worker session test","session":"abc123","output_dir":"/tmp/llm-response-tts/output/abc123-test","epoch":0}'
sleep 3
ls /tmp/llm-response-tts/output/abc123-test/
docker compose logs worker --tail 5
```
Expected: `0000999001.wav` exists in that exact subdirectory; worker logs show `wrote output for id 999001`.

Verify the epoch check works — push a job with a stale epoch after bumping the session's epoch:
```bash
docker compose exec redis redis-cli INCR llm-response-tts:epoch:abc123
docker compose exec redis redis-cli LPUSH llm-response-tts:work_queue \
  '{"id":999002,"text":"stale epoch test","session":"abc123","output_dir":"/tmp/llm-response-tts/output/abc123-test","epoch":0}'
sleep 3
ls /tmp/llm-response-tts/output/abc123-test/
docker compose logs worker --tail 5
```
Expected: no `0000999002.wav` appears; worker logs show `id 999002 cleared mid-job (epoch 0 -> 1), discarding`.

- [ ] **Step 7: Commit**

```bash
git add services/worker/src/main.rs docker-compose.yml
git commit -m "worker: read output_dir/session from job, per-session epoch check; simplify compose"
```

---

## Task 5: `ingest` — compute and send `session`/`output_dir`

**Files:**
- Modify: `host/tools/src/bin/ingest.rs`

**Interfaces:**
- Consumes: `session_key() -> (String, String)` and `sound_output_base() -> PathBuf` from Task 1.
- Produces: enqueue POST body now includes `session` and `output_dir` (matches Task 3's `EnqueueRequest`).

- [ ] **Step 1: Import the new functions and update `post_text`**

Change the import line:
```rust
use llm_response_tts_tools::common::{read_env_var, script_dir};
```
to:
```rust
use llm_response_tts_tools::common::{read_env_var, script_dir, session_key, sound_output_base};
```

Replace `post_text`:
```rust
fn post_text(token: &str, text: &str) -> std::io::Result<()> {
    let body = format!("{{\"text\":\"{}\"}}", json_escape(text));
    let mut stream = TcpStream::connect(("127.0.0.1", 3000))?;
    let request = format!(
        "POST / HTTP/1.1\r\nHost: 127.0.0.1:3000\r\nContent-Type: application/json\r\nContent-Length: {}\r\nAuthorization: Bearer {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        token,
        body
    );
    stream.write_all(request.as_bytes())?;

    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    let status_line = response.lines().next().unwrap_or("");
    if !(status_line.contains(" 200 ") || status_line.contains(" 202 ")) {
        eprintln!("ingest: enqueue failed: {status_line}");
    }
    Ok(())
}
```
with:
```rust
fn post_text(token: &str, text: &str, session: &str, output_dir: &str) -> std::io::Result<()> {
    let body = format!(
        "{{\"text\":\"{}\",\"session\":\"{}\",\"output_dir\":\"{}\"}}",
        json_escape(text),
        json_escape(session),
        json_escape(output_dir)
    );
    let mut stream = TcpStream::connect(("127.0.0.1", 3000))?;
    let request = format!(
        "POST / HTTP/1.1\r\nHost: 127.0.0.1:3000\r\nContent-Type: application/json\r\nContent-Length: {}\r\nAuthorization: Bearer {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        token,
        body
    );
    stream.write_all(request.as_bytes())?;

    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    let status_line = response.lines().next().unwrap_or("");
    if !(status_line.contains(" 200 ") || status_line.contains(" 202 ")) {
        eprintln!("ingest: enqueue failed: {status_line}");
    }
    Ok(())
}
```

- [ ] **Step 2: Compute session info and pass it through in `run()`**

Replace:
```rust
    let env_file = script_dir.join("docker").join(".env");
    let token = read_env_var(&env_file, "LLM_RESPONSE_TTS_BEARER_TOKEN").unwrap_or_default();
    for sentence in split_sentences(&text) {
        post_text(&token, &sentence)?;
    }
```
with:
```rust
    let env_file = script_dir.join("docker").join(".env");
    let token = read_env_var(&env_file, "LLM_RESPONSE_TTS_BEARER_TOKEN").unwrap_or_default();
    let (session_hash, session_dir_name) = session_key();
    let output_dir = sound_output_base().join(&session_dir_name);
    let output_dir_str = output_dir.to_string_lossy().to_string();
    for sentence in split_sentences(&text) {
        post_text(&token, &sentence, &session_hash, &output_dir_str)?;
    }
```

- [ ] **Step 3: Rebuild and reinstall**

```bash
cd /Volumes/Files/Development/Tooling/ai/personal/llm-response-tts
cargo build --release --manifest-path host/Cargo.toml -p llm-response-tts-tools
cargo install --path host/tools --force
```
Expected: builds and installs without error.

- [ ] **Step 4: Manually verify end-to-end**

```bash
cd /Volumes/Files/Development/Tooling/ai/personal/llm-response-tts
echo '{"message_id":"session-test-1","delta":"Testing session enqueue.","final":true}' | llm-response-tts-ingest
sleep 3
docker compose exec redis redis-cli KEYS 'llm-response-tts:pending_ids:*'
```
Expected: a key like `llm-response-tts:pending_ids:<6-char-hash>` for this repo's own cwd (since you're running this from within the repo). Note the hash — it should be the same every time you run this from this same directory.

- [ ] **Step 5: Commit**

```bash
git add host/tools/src/bin/ingest.rs
git commit -m "ingest: compute and send session/output_dir when enqueuing"
```

---

## Task 6: `player` — per-session output dir, lock, and polling

**Files:**
- Modify: `host/player/src/main.rs`

**Interfaces:**
- Consumes: `session_key()` / `sound_output_base()` from Task 2; `ingress`'s `session`-scoped `/next`/`/ack` from Task 3.

- [ ] **Step 1: Update `fetch_next` and `ack` to take a session**

Replace:
```rust
fn fetch_next(token: &str) -> PollResult {
    let result = ureq::get(format!("{BASE_URL}/next"))
        .header("Authorization", &format!("Bearer {token}"))
        .call();
    match result {
        Ok(resp) if resp.status() == 204 => PollResult::Empty,
        Ok(mut resp) => match resp.body_mut().read_json::<NextResponse>() {
            Ok(job) => PollResult::Job(job),
            Err(_) => PollResult::Transient,
        },
        Err(_) => PollResult::Transient,
    }
}

fn ack(token: &str, id: i64) {
    let result = ureq::post(format!("{BASE_URL}/ack"))
        .header("Authorization", &format!("Bearer {token}"))
        .send_json(serde_json::json!({ "id": id }));
    match result {
        Ok(resp) if resp.status() == 204 => {}
        Ok(resp) => eprintln!(
            "player: ack for id {id} got http {}, will retry via next poll",
            resp.status()
        ),
        Err(e) => eprintln!("player: ack for id {id} failed: {e}, will retry via next poll"),
    }
}
```
with:
```rust
fn fetch_next(token: &str, session: &str) -> PollResult {
    let result = ureq::get(format!("{BASE_URL}/next?session={session}"))
        .header("Authorization", &format!("Bearer {token}"))
        .call();
    match result {
        Ok(resp) if resp.status() == 204 => PollResult::Empty,
        Ok(mut resp) => match resp.body_mut().read_json::<NextResponse>() {
            Ok(job) => PollResult::Job(job),
            Err(_) => PollResult::Transient,
        },
        Err(_) => PollResult::Transient,
    }
}

fn ack(token: &str, session: &str, id: i64) {
    let result = ureq::post(format!("{BASE_URL}/ack"))
        .header("Authorization", &format!("Bearer {token}"))
        .send_json(serde_json::json!({ "id": id, "session": session }));
    match result {
        Ok(resp) if resp.status() == 204 => {}
        Ok(resp) => eprintln!(
            "player: ack for id {id} got http {}, will retry via next poll",
            resp.status()
        ),
        Err(e) => eprintln!("player: ack for id {id} failed: {e}, will retry via next poll"),
    }
}
```

- [ ] **Step 2: Rework `main()`'s setup to be per-session**

Replace the whole setup block, from the start of `main()` through `let Some(_lock) = ...`:
```rust
fn main() {
    // This binary is installed outside the repo (see ingest's spawn comment for why), and can
    // be spawned while Claude Code's cwd is some *other* project entirely - not this repo - so
    // the root can't come from cwd or the exe's own path either. Baked in at compile time
    // instead, via CARGO_MANIFEST_DIR (this crate's own Cargo.toml location during `cargo
    // install`), same approach as host/tools/src/common.rs::script_dir(). Re-run `cargo
    // install` after moving the repo to pick up the new location.
    let script_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("CARGO_MANIFEST_DIR has unexpected shape")
        .to_path_buf();
    let out_dir = script_dir.join("tmp");
    // Fixed system path, not repo-relative like the rest of script_dir's uses below - worker
    // (in its container) and player (on the host) both default to the same literal path
    // independently, so they agree on where wav files are without any coordination, and
    // docker-compose.yml bind-mounts the host path at that identical path in the container.
    let output_dir = std::env::var("LLM_RESPONSE_TTS_SOUND_OUTPUT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp/llm-response-tts/output"));
    let lock_dir = out_dir.join("worker.lock");
    let env_file = script_dir.join("docker").join(".env");

    let _ = std::fs::create_dir_all(&output_dir);

    let Some(_lock) = Lock::acquire(lock_dir) else {
        return; // lock held by a live process - nothing to do
    };
```
with:
```rust
fn main() {
    let (session_hash, session_dir_name) = session_key();

    // This binary is installed outside the repo (see ingest's spawn comment for why), and can
    // be spawned while Claude Code's cwd is some *other* project entirely - not this repo - so
    // the root can't come from cwd or the exe's own path either. Baked in at compile time
    // instead, via CARGO_MANIFEST_DIR (this crate's own Cargo.toml location during `cargo
    // install`), same approach as host/tools/src/common.rs::script_dir(). Re-run `cargo
    // install` after moving the repo to pick up the new location.
    let script_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("CARGO_MANIFEST_DIR has unexpected shape")
        .to_path_buf();
    let env_file = script_dir.join("docker").join(".env");

    let output_dir = sound_output_base().join(&session_dir_name);
    // Fixed system base, not under LLM_RESPONSE_TTS_SOUND_OUTPUT - lock location should stay
    // predictable even if that env var is ever reconfigured to point somewhere else.
    let lock_dir = PathBuf::from("/tmp/llm-response-tts")
        .join(&session_dir_name)
        .join("player.lock");

    let _ = std::fs::create_dir_all(&output_dir);
    if let Some(parent) = lock_dir.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let Some(_lock) = Lock::acquire(lock_dir) else {
        return; // lock held by a live process for this session - nothing to do
    };
```

- [ ] **Step 3: Pass `session_hash` through to `fetch_next`/`ack` in the poll loop**

Replace:
```rust
    while idle < IDLE_EXIT {
        match fetch_next(&token) {
```
with:
```rust
    while idle < IDLE_EXIT {
        match fetch_next(&token, &session_hash) {
```

Replace both call sites of `ack(&token, job.id)` with `ack(&token, &session_hash, job.id)` (there are two — the successful-playback branch and the `MAX_WAIT` giveup branch).

- [ ] **Step 4: Rebuild, reinstall, and refresh the dev copy**

```bash
cd /Volumes/Files/Development/Tooling/ai/personal/llm-response-tts
cargo build --release --manifest-path host/Cargo.toml -p llm-response-tts-player
cargo install --path host/player --force
bash host/player/build.sh
```
Expected: all three succeed without error.

- [ ] **Step 5: Manually verify end-to-end, including two-session isolation**

```bash
cd /Volumes/Files/Development/Tooling/ai/personal/llm-response-tts
echo '{"message_id":"session-test-2","delta":"Verifying per session playback.","final":true}' | llm-response-tts-ingest
sleep 5
ps aux | grep llm-response-tts-player | grep -v grep
```
Expected: a `player` process is running; find its lock: `find /tmp/llm-response-tts -maxdepth 2 -name player.lock` should show `/tmp/llm-response-tts/<hash>-llm-response-tts/player.lock`, and `cat` that dir's `pid` file should match the running process's pid.

Confirm the wav actually plays and gets cleaned up:
```bash
sleep 8
ls /tmp/llm-response-tts/output/<hash>-llm-response-tts/
```
Expected: empty (played and deleted) or draining if other messages queued concurrently during this session.

- [ ] **Step 6: Commit**

```bash
git add host/player/src/main.rs
git commit -m "player: per-session output dir, lock path, and next/ack scoping"
```

---

## Task 7: `clear-speech` session scoping + new `clear-all-speech`

**Files:**
- Modify: `host/tools/src/bin/clear-speech.rs`
- Create: `host/tools/src/bin/clear-all-speech.rs`
- Modify: `host/tools/Cargo.toml`

**Interfaces:**
- Consumes: `session_key()` from Task 1; `ingress`'s `POST /clear` (session-scoped) and `POST /clear-all` from Task 3.

- [ ] **Step 1: Update `clear-speech.rs` to send its session**

Replace the whole file:
```rust
// Drops every queued message so nothing more plays after whatever's currently speaking
// finishes. Doesn't interrupt audio already playing - see README's "Message queueing"
// section for why (the player binary blocks until playback finishes; stopping mid-sentence
// would need a different, more invasive design).
// Rebuild after editing: cargo build --release --manifest-path host/Cargo.toml
use llm_response_tts_tools::common::{read_env_var, script_dir};
use std::io::{Read, Write};
use std::net::TcpStream;

fn clear(token: &str) -> std::io::Result<String> {
    let mut stream = TcpStream::connect(("127.0.0.1", 3000))?;
    let request = format!(
        "POST /clear HTTP/1.1\r\nHost: 127.0.0.1:3000\r\nAuthorization: Bearer {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        token
    );
    stream.write_all(request.as_bytes())?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    Ok(response)
}

fn main() {
    let script_dir = script_dir();
    let env_file = script_dir.join("docker").join(".env");
    let token = read_env_var(&env_file, "LLM_RESPONSE_TTS_BEARER_TOKEN").unwrap_or_default();

    match clear(&token) {
        Ok(response) if response.lines().next().unwrap_or("").contains(" 204 ") => {
            println!("cleared pending speech");
        }
        Ok(response) => {
            eprintln!("clear failed: {}", response.lines().next().unwrap_or(""));
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("clear failed: {e}");
            std::process::exit(1);
        }
    }
}
```
with:
```rust
// Drops everything queued *for this session* (identified by cwd - see common.rs::session_key)
// so nothing more plays after whatever's currently speaking finishes. Doesn't interrupt audio
// already playing - see README's "Message queueing" section for why (player blocks until
// playback finishes; stopping mid-sentence would need a different, more invasive design). Use
// clear-all-speech instead to clear every session, not just this one.
// Rebuild after editing: cargo build --release --manifest-path host/Cargo.toml
use llm_response_tts_tools::common::{read_env_var, script_dir, session_key};
use std::io::{Read, Write};
use std::net::TcpStream;

fn clear(token: &str, session: &str) -> std::io::Result<String> {
    let body = format!("{{\"session\":\"{session}\"}}");
    let mut stream = TcpStream::connect(("127.0.0.1", 3000))?;
    let request = format!(
        "POST /clear HTTP/1.1\r\nHost: 127.0.0.1:3000\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        token,
        body.len(),
        body
    );
    stream.write_all(request.as_bytes())?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    Ok(response)
}

fn main() {
    let script_dir = script_dir();
    let env_file = script_dir.join("docker").join(".env");
    let token = read_env_var(&env_file, "LLM_RESPONSE_TTS_BEARER_TOKEN").unwrap_or_default();
    let (session_hash, _) = session_key();

    match clear(&token, &session_hash) {
        Ok(response) if response.lines().next().unwrap_or("").contains(" 204 ") => {
            println!("cleared pending speech for this session");
        }
        Ok(response) => {
            eprintln!("clear failed: {}", response.lines().next().unwrap_or(""));
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("clear failed: {e}");
            std::process::exit(1);
        }
    }
}
```

- [ ] **Step 2: Create `clear-all-speech.rs`**

Create `host/tools/src/bin/clear-all-speech.rs`:
```rust
// Drops every session's queued/pending speech, not just the caller's own - see clear-speech
// for the per-session version most usage should reach for instead.
// Rebuild after editing: cargo build --release --manifest-path host/Cargo.toml
use llm_response_tts_tools::common::{read_env_var, script_dir};
use std::io::{Read, Write};
use std::net::TcpStream;

fn clear_all(token: &str) -> std::io::Result<String> {
    let mut stream = TcpStream::connect(("127.0.0.1", 3000))?;
    let request = format!(
        "POST /clear-all HTTP/1.1\r\nHost: 127.0.0.1:3000\r\nAuthorization: Bearer {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        token
    );
    stream.write_all(request.as_bytes())?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    Ok(response)
}

fn main() {
    let script_dir = script_dir();
    let env_file = script_dir.join("docker").join(".env");
    let token = read_env_var(&env_file, "LLM_RESPONSE_TTS_BEARER_TOKEN").unwrap_or_default();

    match clear_all(&token) {
        Ok(response) if response.lines().next().unwrap_or("").contains(" 204 ") => {
            println!("cleared pending speech for every session");
        }
        Ok(response) => {
            eprintln!("clear-all failed: {}", response.lines().next().unwrap_or(""));
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("clear-all failed: {e}");
            std::process::exit(1);
        }
    }
}
```

- [ ] **Step 3: Register the new binary in `host/tools/Cargo.toml`**

Append to `host/tools/Cargo.toml`:
```toml

[[bin]]
name = "llm-response-tts-clear-all-speech"
path = "src/bin/clear-all-speech.rs"
```

- [ ] **Step 4: Rebuild and reinstall**

```bash
cd /Volumes/Files/Development/Tooling/ai/personal/llm-response-tts
cargo build --release --manifest-path host/Cargo.toml
cargo install --path host/tools --force
```
Expected: builds `llm-response-tts-clear-all-speech` alongside the other two; `cargo install` reports all three (re)installed.

- [ ] **Step 5: Manually verify both binaries, including cross-session isolation**

Enqueue in two different simulated sessions (different temp dirs), then clear only one:
```bash
mkdir -p /tmp/plan-verify-session-a /tmp/plan-verify-session-b
cd /Volumes/Files/Development/Tooling/ai/personal/llm-response-tts

(cd /tmp/plan-verify-session-a && echo '{"message_id":"clear-test-a","delta":"Session A message.","final":true}' | llm-response-tts-ingest)
(cd /tmp/plan-verify-session-b && echo '{"message_id":"clear-test-b","delta":"Session B message.","final":true}' | llm-response-tts-ingest)
sleep 2
docker compose exec redis redis-cli KEYS 'llm-response-tts:pending_ids:*'

(cd /tmp/plan-verify-session-a && llm-response-tts-clear-speech)
sleep 1
docker compose exec redis redis-cli KEYS 'llm-response-tts:pending_ids:*'
```
Expected: after the first `KEYS`, two `pending_ids:<hash>` keys exist (one per temp dir). After clearing from session A's directory, only session B's key remains (session A's list emptied — note an empty Redis list key may or may not still show in `KEYS` depending on timing/whether it was ack'd already; the important check is that a `LRANGE` on session A's key is empty while session B's still has its id).

Then verify `clear-all-speech` wipes both:
```bash
(cd /tmp/plan-verify-session-b && echo '{"message_id":"clear-test-b2","delta":"Another B message.","final":true}' | llm-response-tts-ingest)
sleep 2
llm-response-tts-clear-all-speech
sleep 1
docker compose exec redis redis-cli LLEN llm-response-tts:work_queue
```
Expected: `clear-all-speech` prints `cleared pending speech for every session`; `work_queue` length is `0`.

Clean up the temp session dirs afterward: `rm -rf /tmp/plan-verify-session-a /tmp/plan-verify-session-b`.

- [ ] **Step 6: Commit**

```bash
git add host/tools/src/bin/clear-speech.rs host/tools/src/bin/clear-all-speech.rs host/tools/Cargo.toml
git commit -m "clear-speech: scope to caller's session; add clear-all-speech"
```

---

## Task 8: README updates

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Update "What it installs"**

Add `clear-all-speech` to the host binaries list (it's a fourth `src/bin/` target in the `tools` package, alongside `ingest` and `clear-speech`) — one line, matching the existing bullet style for `clear-speech`.

- [ ] **Step 2: Update the "Env variables" table**

Update the `LLM_RESPONSE_TTS_SOUND_OUTPUT` row's description to reflect that it's now the *parent* of per-session subdirectories, not a flat directory used directly.

- [ ] **Step 3: Add a "Session isolation" subsection under Architecture**

Add a new subsection (after "Message queueing", before the Security section) explaining: `session_hash` is derived from the invoking process's cwd (MurmurHash3 + base62, 6 chars); it scopes `pending_ids`, `epoch`, and the output/lock directories per session; `work_queue` and `next_id` stay global since synthesis itself doesn't care which session a job came from; `clear-speech` only clears the caller's own session, `clear-all-speech` clears every session. Reference `docs/superpowers/specs/2026-08-01-per-session-isolation-design.md` for the full design rationale rather than duplicating it.

- [ ] **Step 4: Add a note to the Security Audit results section**

Add one paragraph clarifying that session isolation is an organizational/UX boundary, not a security one — every session still shares the same single `LLM_RESPONSE_TTS_BEARER_TOKEN`, so any caller that has the token (i.e., anything already trusted enough to reach nginx) could address another session's `/clear`, `/next`, etc. by guessing or observing its `session_hash`. This doesn't change the existing trust model (same as `worker`→kokoros being unauthenticated on the internal network) — just worth stating explicitly now that there's more than one logical "queue."

- [ ] **Step 5: Commit**

```bash
cd /Volumes/Files/Development/Tooling/ai/personal/llm-response-tts
git add README.md
git commit -m "README: document per-session isolation, clear-all-speech, updated env var table"
```

---

## Task 9: DRY/KISS/YAGNI review pass

Re-read every file touched by Tasks 1-8 with fresh eyes and check for:

- **DRY**: any logic now duplicated beyond the intentional `common.rs`/`player` split (which is itself an established, deliberate exception — don't "fix" that one).
- **KISS**: any step that ended up more convoluted than it needed to be once you can see the whole picture — e.g. does `worker`'s per-job `create_dir_all` belong somewhere else, does `player`'s `main()` read cleanly top-to-bottom now.
- **YAGNI**: anything added that nothing actually uses — e.g. leftover fields, an unused `out_dir` variable in `player` (it was used for both the old flat output dir and the old lock path; confirm both call sites were actually migrated off it and the variable itself was removed, not just shadowed).
- Confirm `cargo build --release --manifest-path host/Cargo.toml` and `cargo test --manifest-path host/Cargo.toml` both pass clean with no warnings.
- Confirm `docker compose up -d --build` (full stack) starts clean with no errors in any service's logs.

Fix anything found inline. If nothing needs fixing, say so explicitly rather than skipping the pass.

- [ ] **Step 1: Run the full verification suite**

```bash
cd /Volumes/Files/Development/Tooling/ai/personal/llm-response-tts
cargo build --release --manifest-path host/Cargo.toml 2>&1 | tail -20
cargo test --manifest-path host/Cargo.toml 2>&1 | tail -30
docker compose up -d --build 2>&1 | tail -20
docker compose logs --tail 10
```
Expected: no warnings, no errors, no failed tests.

- [ ] **Step 2: Fix anything found, or confirm clean**

- [ ] **Step 3: Final commit (if Step 2 made changes)**

```bash
git add -A
git commit -m "Simplify per-session isolation implementation after review"
```
