use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

fn unique_temp_path(label: &str) -> std::path::PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("llm-response-tts-common-test-{}-{label}-{n}", std::process::id()))
}

fn write_temp_file(label: &str, contents: &str) -> std::path::PathBuf {
    let path = unique_temp_path(label);
    std::fs::write(&path, contents).expect("write temp file");
    path
}

// --- read_env_var ---

#[test]
fn read_env_var_cases() {
    let cases = [
        (Some("FOO=bar\nBAZ=qux\n"), "FOO", Some("bar")),
        (Some("FOO=bar\nBAZ=qux\n"), "BAZ", Some("qux")),
        (Some("  FOO  =  bar  \n"), "FOO", Some("bar")), // whitespace around key/value is trimmed
        (None, "FOO", None),                             // file doesn't exist
        (Some("FOO=bar\n"), "OTHER", None),               // key not present
        // A blank or malformed line short-circuits the *entire* scan (the `?` on split_once
        // bails out of read_env_var itself, not just that one line) - worth pinning down since
        // it means a stray blank line before the real KEY=VALUE line hides every key after it.
        (Some("\nFOO=bar\n"), "FOO", None),
    ];
    for (content, key, expected) in cases {
        let path = match content {
            Some(c) => write_temp_file("read_env_var_case", c),
            None => unique_temp_path("read_env_var_case_missing"),
        };
        assert_eq!(read_env_var(&path, key), expected.map(String::from), "content={content:?} key={key:?}");
        if content.is_some() {
            let _ = std::fs::remove_file(&path);
        }
    }
}

// --- murmurhash3_x86_32 ---

#[test]
fn murmurhash3_of_empty_input_is_zero() {
    assert_eq!(murmurhash3_x86_32(b"", 0), 0);
}

#[test]
fn murmurhash3_is_deterministic_and_input_sensitive() {
    let a = murmurhash3_x86_32(b"/Users/xliang/projects/foo", 0);
    let b = murmurhash3_x86_32(b"/Users/xliang/projects/foo", 0);
    let c = murmurhash3_x86_32(b"/Users/xliang/projects/bar", 0);
    assert_eq!(a, b, "same input must hash the same every time");
    assert_ne!(a, c, "different input should (almost always) hash differently");
}

// --- to_base62 ---

#[test]
fn to_base62_known_values() {
    let cases = [
        (0u32, "000000"),
        // 125 = 2*62 + 1 -> digits [2, 1] in base62, left-padded to width 6
        (125u32, "000021"),
    ];
    for (n, expected) in cases {
        assert_eq!(to_base62(n, 6), expected);
    }
}

#[test]
fn to_base62_is_fixed_width_and_valid_alphabet() {
    for n in [0u32, 1, 61, 62, 125, u32::MAX] {
        let s = to_base62(n, 6);
        assert_eq!(s.len(), 6);
        assert!(s.bytes().all(|b| BASE62_ALPHABET.contains(&b)));
    }
}

// --- session_key ---

#[test]
fn session_key_dir_name_starts_with_the_hash() {
    let (hash, dir_name) = session_key();
    assert_eq!(hash.len(), 6);
    assert!(dir_name.starts_with(&format!("{hash}-")));
}

// --- build_http_request ---

#[test]
fn build_http_request_main_flow() {
    let req = build_http_request("/clear", "tok123", "{\"session\":\"abc\"}");
    assert!(req.starts_with("POST /clear HTTP/1.1\r\n"));
    assert!(req.contains("Authorization: Bearer tok123\r\n"));
    assert!(req.contains("Content-Type: application/json\r\n"));
    assert!(req.contains("Content-Length: 17\r\n"));
    assert!(req.ends_with("{\"session\":\"abc\"}"));
}

#[test]
fn build_http_request_content_length_counts_bytes_not_chars() {
    // multi-byte UTF-8 body - Content-Length must be the byte length httpd expects to read,
    // not the (smaller) char count.
    let body = "{\"text\":\"caf\u{e9}\"}"; // "café" - the é is 2 bytes in UTF-8
    let req = build_http_request("/", "tok", body);
    assert_eq!(body.chars().count(), body.len() - 1);
    assert!(req.contains(&format!("Content-Length: {}\r\n", body.len())));
}

#[test]
fn build_http_request_empty_body() {
    let req = build_http_request("/clear-all", "tok", "");
    assert!(req.contains("Content-Length: 0\r\n"));
    assert!(req.ends_with("\r\n\r\n"));
}

// --- http_status_line ---

#[test]
fn http_status_line_cases() {
    let cases = [
        ("HTTP/1.1 204 No Content\r\nServer: nginx\r\n\r\n", "HTTP/1.1 204 No Content"),
        ("", ""),
    ];
    for (response, expected) in cases {
        assert_eq!(http_status_line(response), expected);
    }
}
