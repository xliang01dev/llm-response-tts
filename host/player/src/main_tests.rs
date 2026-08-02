use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

// murmurhash3/to_base62/session_key here are intentionally a separate compiled copy of
// host/tools/src/common.rs's implementation (see the comment on murmurhash3_x86_32 above) -
// these tests exist to independently verify *this* crate's copy, not to duplicate
// common_tests.rs's coverage of the other one.

#[test]
fn murmurhash3_of_empty_input_is_zero() {
    assert_eq!(murmurhash3_x86_32(b"", 0), 0);
}

#[test]
fn murmurhash3_is_deterministic_and_input_sensitive() {
    let a = murmurhash3_x86_32(b"/Users/xliang/projects/foo", 0);
    let b = murmurhash3_x86_32(b"/Users/xliang/projects/foo", 0);
    let c = murmurhash3_x86_32(b"/Users/xliang/projects/bar", 0);
    assert_eq!(a, b);
    assert_ne!(a, c);
}

#[test]
fn to_base62_known_values() {
    let cases = [(0u32, "000000"), (125u32, "000021")];
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

#[test]
fn session_key_dir_name_starts_with_the_hash() {
    let (hash, dir_name) = session_key();
    assert_eq!(hash.len(), 6);
    assert!(dir_name.starts_with(&format!("{hash}-")));
}

fn unique_temp_path(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("llm-response-tts-player-test-{}-{label}-{n}", std::process::id()))
}

// --- playback_speed ---
//
// Mutates the process-wide LLM_RESPONSE_TTS_PLAYBACK_SPEED env var - safe here specifically
// because this is the only test in this binary that reads or writes it, so there's nothing
// else for it to race with regardless of how the test harness interleaves threads.

#[test]
fn playback_speed_cases() {
    let cases = [
        (Some("1.5"), 1.5),
        (Some("0.75"), 0.75),
        (None, 1.0),                 // env var unset - default
        (Some("not-a-number"), 1.0), // unparseable - default
        (Some("0"), 1.0),            // zero rejected - default
        (Some("-1"), 1.0),           // negative rejected - default
    ];
    for (value, expected) in cases {
        match value {
            Some(v) => unsafe { std::env::set_var("LLM_RESPONSE_TTS_PLAYBACK_SPEED", v) },
            None => unsafe { std::env::remove_var("LLM_RESPONSE_TTS_PLAYBACK_SPEED") },
        }
        assert_eq!(playback_speed(), expected, "value={value:?}");
    }
    unsafe { std::env::remove_var("LLM_RESPONSE_TTS_PLAYBACK_SPEED") };
}

// --- Lock ---

#[test]
fn lock_acquire_main_flow_then_releases_on_drop() {
    let dir = unique_temp_path("basic");
    {
        let lock = Lock::acquire(dir.clone());
        assert!(lock.is_some());
        assert!(dir.is_dir());
        assert!(dir.join("pid").is_file());
    }
    assert!(!dir.exists(), "Drop should remove the lock dir and pid file");
}

#[test]
fn lock_acquire_fails_while_held_by_a_live_process() {
    let dir = unique_temp_path("held");
    let _first = Lock::acquire(dir.clone()).expect("first acquire should succeed");
    // second attempt in the same process - the pid file records our own (very much alive) pid
    assert!(Lock::acquire(dir.clone()).is_none());
}

#[test]
fn lock_acquire_reclaims_a_stale_lock_left_by_a_dead_process() {
    let dir = unique_temp_path("stale");
    std::fs::create_dir_all(&dir).unwrap();
    // guaranteed-dead pid: spawn a process and wait for it to exit before using its pid
    let mut child = std::process::Command::new("true").spawn().expect("spawn short-lived process");
    let dead_pid = child.id() as i32;
    child.wait().expect("wait for child to exit");
    std::fs::write(dir.join("pid"), dead_pid.to_string()).unwrap();

    assert!(Lock::acquire(dir.clone()).is_some(), "a lock held by a dead pid should be reclaimed");

    let _ = std::fs::remove_file(dir.join("pid"));
    let _ = std::fs::remove_dir(&dir);
}
