// Shared by the ingest, clear-speech, and clear-all-speech binaries (src/bin/) via this
// crate's lib.rs. player/ is a separate workspace member with its own dependencies and
// install location, so it keeps its own small copy of read_env_var rather than depending on
// this crate.

// These binaries are installed via `cargo install`, which lands flat in
// ~/.cargo/bin - so unlike the old bin/-relative layout, the repo root can no longer be
// derived from the exe's own path at runtime. It also can't come from cwd: the whole point of
// installing these globally is that a MessageDisplay hook in some *other* project's
// .claude/settings.json can invoke `ingest` while Claude Code's cwd is that other project, not
// this repo. So the repo root is instead baked into the binary at compile time, via
// CARGO_MANIFEST_DIR (this crate's own Cargo.toml location during that `cargo install`) - it
// always resolves back to this checkout's docker/.env, tmp/, etc regardless of where or how
// the binary is later invoked. Re-run `cargo install` after moving the repo to pick up the new
// location.
pub fn script_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("CARGO_MANIFEST_DIR has unexpected shape")
        .to_path_buf()
}

pub fn read_env_var(path: &std::path::Path, key: &str) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    for line in content.lines() {
        let (k, v) = line.trim().split_once('=')?;
        if k.trim() == key {
            return Some(v.trim().to_string());
        }
    }
    None
}

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

// Split from http_post so the request-string assembly (headers, Content-Length, CRLF layout)
// is checkable without an actual socket.
fn build_http_request(path: &str, token: &str, body: &str) -> String {
    format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:3000\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

// Minimal hand-rolled HTTP/1.1 POST (nginx on 127.0.0.1:3000) - kept dependency-free (no HTTP
// client crate) since the target and request shape never vary. Shared by ingest, clear-speech,
// and clear-all-speech - see module comment for why player keeps its own copy instead.
pub fn http_post(path: &str, token: &str, body: &str) -> std::io::Result<String> {
    use std::io::{Read, Write};
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", 3000))?;
    let request = build_http_request(path, token, body);
    stream.write_all(request.as_bytes())?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    Ok(response)
}

pub fn http_status_line(response: &str) -> &str {
    response.lines().next().unwrap_or("")
}

#[cfg(test)]
#[path = "common_tests.rs"]
mod tests;
