use super::*;

// --- valid_session_dir ---

#[test]
fn valid_session_dir_main_flow_exact_and_prefix_match() {
    assert!(valid_session_dir("abc123", "abc123"));
    assert!(valid_session_dir("abc123", "abc123-my-project"));
}

#[test]
fn valid_session_dir_rejects_unsafe_or_unrelated_values() {
    let cases: &[(&str, &str)] = &[
        ("abc123", ""),               // empty
        ("abc123", "/"),               // contains a slash
        ("abc123", "abc123/../etc"),   // path traversal via slash
        ("abc123", "abc123\\etc"),     // contains a backslash
        ("abc123", "."),               // exactly "."
        ("abc123", ".."),              // exactly ".."
        ("abc", "abcdef"),             // shares a prefix but has no "-" separator
        ("abc", "xyz"),                // unrelated to session entirely
        ("abc123", "ABC123"),          // must match case-exactly, not case-insensitively
    ];
    for (session, session_dir) in cases {
        assert!(
            !valid_session_dir(session, session_dir),
            "expected {session_dir:?} to be rejected for session {session:?}"
        );
    }
}

// --- redis key / filename formatters ---

#[test]
fn redis_key_and_filename_formatters() {
    assert_eq!(pending_ids_key("abc123"), "llm-response-tts:pending_ids:abc123");
    assert_eq!(epoch_key("abc123"), "llm-response-tts:epoch:abc123");
    assert_eq!(status_key(42), "llm-response-tts:status:42");
    assert_eq!(wav_filename(5), "0000000005.wav");
    assert_eq!(wav_filename(0), "0000000000.wav");
}
