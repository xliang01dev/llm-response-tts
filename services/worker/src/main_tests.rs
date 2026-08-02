use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

fn unique_temp_path(label: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("llm-response-tts-worker-test-{}-{label}-{n}", std::process::id()))
}

fn write_temp_file(label: &str, contents: &str) -> PathBuf {
    let path = unique_temp_path(label);
    std::fs::write(&path, contents).expect("write temp file");
    path
}

// --- is_alnum_key / is_word_byte / utf8_char_len ---

#[test]
fn is_alnum_key_main_flow_and_edge_cases() {
    let cases = [
        ("SQL", true),
        ("k8s", true), // digits count as alphanumeric
        ("", false),
        ("CI/CD", false),  // contains a slash
        ("->", false),     // punctuation-only key
        ("caf\u{e9}", false), // non-ASCII letters aren't ascii_alphanumeric
    ];
    for (key, expected) in cases {
        assert_eq!(is_alnum_key(key), expected, "is_alnum_key({key:?})");
    }
}

#[test]
fn is_word_byte_main_flow_and_edge_cases() {
    for b in [b'a', b'Z', b'0', b'_'] {
        assert!(is_word_byte(b), "{} should be a word byte", b as char);
    }
    for b in [b' ', b'-', b'.', b'/'] {
        assert!(!is_word_byte(b), "{} should not be a word byte", b as char);
    }
}

#[test]
fn utf8_char_len_by_leading_byte() {
    assert_eq!(utf8_char_len(b'a'), 1); // ASCII
    assert_eq!(utf8_char_len(0xC2), 2); // 2-byte lead, e.g. "é"
    assert_eq!(utf8_char_len(0xE2), 3); // 3-byte lead, e.g. "→"
    assert_eq!(utf8_char_len(0xF0), 4); // 4-byte lead, e.g. an emoji
}

// --- replace_word_ci ---

#[test]
fn replace_word_ci_cases() {
    let cases = [
        ("I use SQL daily", "sql", "sequel", "I use sequel daily"), // case-insensitive text
        ("i use sql daily", "SQL", "sequel", "i use sequel daily"), // case-insensitive key
        ("SQLite is great", "SQL", "sequel", "SQLite is great"),    // respects word boundaries
        ("SQL", "SQL", "sequel", "sequel"),                         // whole-string match
        ("use SQL", "SQL", "sequel", "use sequel"),                 // match at end of string
        ("SQL rocks", "SQL", "sequel", "sequel rocks"),             // match at start of string
        ("SQL and sql and Sql", "SQL", "sequel", "sequel and sequel and sequel"), // every occurrence
        ("nothing to see here", "SQL", "sequel", "nothing to see here"), // no match
        ("", "SQL", "sequel", ""),                                  // empty text
        ("caf\u{e9} SQL caf\u{e9}", "SQL", "sequel", "caf\u{e9} sequel caf\u{e9}"), // unicode around match
    ];
    for (text, key, val, expected) in cases {
        assert_eq!(replace_word_ci(text, key, val), expected, "replace_word_ci({text:?}, {key:?}, {val:?})");
    }
}

// --- strip_chars ---

#[test]
fn strip_chars_cases() {
    let cases = [
        ("*bold* _em_ `code`", vec!['*', '_', '`'], "bold em code"),
        ("*unchanged*", vec![], "*unchanged*"),       // empty set is a no-op
        ("plain text", vec!['~', '^'], "plain text"), // no matches present in the text
    ];
    for (text, chars, expected) in cases {
        let set: HashSet<char> = chars.into_iter().collect();
        assert_eq!(strip_chars(text, &set), expected, "strip_chars({text:?})");
    }
}

// --- expand_measurements ---

fn sample_units() -> Vec<(String, String, String)> {
    vec![
        ("ms".to_string(), "millisecond".to_string(), "milliseconds".to_string()),
        ("in".to_string(), "inch".to_string(), "inches".to_string()),
    ]
}

#[test]
fn expand_measurements_cases() {
    let units = sample_units();
    let empty: Vec<(String, String, String)> = Vec::new();
    let cases = [
        ("wait 24ms please", &units, "wait 24 milliseconds please"),
        ("1ms delay", &units, "1 millisecond delay"),           // singular for value 1
        ("1.5in gap", &units, "1.5 inches gap"),                // decimal numbers
        ("room 24xyz wide", &units, "room 24xyz wide"),         // no matching unit
        ("24mstest", &units, "24mstest"),                       // unit glued to a longer word
        ("v24ms", &units, "v24ms"),                             // digits glued to a preceding word
        ("24ms", &empty, "24ms"),                               // empty units list is a no-op
    ];
    for (text, units, expected) in cases {
        assert_eq!(expand_measurements(text, units), expected, "expand_measurements({text:?})");
    }
}

// --- apply_transform ---

#[test]
fn apply_transform_main_flow_combines_measurement_word_ref_and_strip_passes() {
    let units = sample_units();
    let refs = vec![("SQL".to_string(), "sequel".to_string())];
    let strip: HashSet<char> = ['*'].into_iter().collect();
    assert_eq!(apply_transform("*SQL* takes 24ms", &units, &refs, &strip), "sequel takes 24 milliseconds");
}

#[test]
fn apply_transform_non_alnum_key_replaces_literally_ignoring_word_boundaries() {
    let refs = vec![("->".to_string(), "changed to".to_string())];
    assert_eq!(apply_transform("a->b", &[], &refs, &HashSet::new()), "achanged tob");
}

// --- load_json ---
//
// load_word_references/load_strip_chars/load_units are thin wrappers around this shared
// generic loader - its missing-file/malformed-content fallback-to-default behavior is tested
// once here rather than once per wrapper, since it's the same logic underneath each of them.

#[test]
fn load_json_main_flow_parses_valid_content() {
    let path = write_temp_file("load_json_ok", r#"{"a":"1"}"#);
    let result: HashMap<String, String> = load_json(path.to_str().unwrap());
    assert_eq!(result.get("a"), Some(&"1".to_string()));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn load_json_missing_file_or_malformed_content_falls_back_to_default() {
    let missing = unique_temp_path("load_json_missing");
    let from_missing: HashMap<String, String> = load_json(missing.to_str().unwrap());
    assert!(from_missing.is_empty());

    let bad = write_temp_file("load_json_bad", "not valid json");
    let from_bad: HashMap<String, String> = load_json(bad.to_str().unwrap());
    assert!(from_bad.is_empty());
    let _ = std::fs::remove_file(&bad);
}

// --- load_word_references / load_strip_chars / load_units ---
//
// Each of these adds its own transform on top of load_json (sort by length, keep first char,
// reshape the value tuple) - that transform, not the loading itself, is what's worth checking
// here.

#[test]
fn load_word_references_sorts_by_descending_key_length_and_preserves_content() {
    let path = write_temp_file("word_refs", r#"{"a":"1","longer":"2","bb":"3"}"#);
    let refs = load_word_references(path.to_str().unwrap());

    let lengths: Vec<usize> = refs.iter().map(|(k, _)| k.chars().count()).collect();
    let mut sorted_desc = lengths.clone();
    sorted_desc.sort_by(|a, b| b.cmp(a));
    assert_eq!(lengths, sorted_desc);

    let as_map: HashMap<String, String> = refs.into_iter().collect();
    assert_eq!(
        as_map,
        HashMap::from([
            ("a".to_string(), "1".to_string()),
            ("longer".to_string(), "2".to_string()),
            ("bb".to_string(), "3".to_string()),
        ])
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn load_strip_chars_keeps_only_the_first_char_of_each_entry() {
    let path = write_temp_file("strip_chars", r#"["*", "_", "ab"]"#);
    let set = load_strip_chars(path.to_str().unwrap());
    assert_eq!(set, HashSet::from(['*', '_', 'a'])); // "ab" contributes only 'a', not 'b'
    let _ = std::fs::remove_file(&path);
}

#[test]
fn load_units_sorts_by_descending_key_length_and_preserves_content() {
    let path =
        write_temp_file("units", r#"{"s":["second","seconds"],"ms":["millisecond","milliseconds"]}"#);
    let units = load_units(path.to_str().unwrap());

    assert_eq!(units[0].0.len(), 2, "\"ms\" (longer key) should sort before \"s\"");
    assert_eq!(units[1].0.len(), 1);

    let as_set: HashSet<(String, String, String)> = units.into_iter().collect();
    assert_eq!(
        as_set,
        HashSet::from([
            ("s".to_string(), "second".to_string(), "seconds".to_string()),
            ("ms".to_string(), "millisecond".to_string(), "milliseconds".to_string()),
        ])
    );
    let _ = std::fs::remove_file(&path);
}

// --- write_output ---

#[test]
fn write_output_writes_zero_padded_filename_and_renames_away_the_tmp_file() {
    let dir = unique_temp_path("write_output_dir");
    std::fs::create_dir_all(&dir).unwrap();

    write_output(&dir, 5, b"hello wav bytes").unwrap();

    let final_path = dir.join("0000000005.wav");
    assert_eq!(std::fs::read(&final_path).unwrap(), b"hello wav bytes");
    assert!(!dir.join("0000000005.wav.tmp").exists());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn write_output_errors_when_the_target_directory_does_not_exist() {
    let never_created = unique_temp_path("write_output_missing_dir").join("nested");
    assert!(write_output(&never_created, 1, b"data").is_err());
}

// --- redis key formatters ---

#[test]
fn redis_key_formatters() {
    assert_eq!(status_key(42), "llm-response-tts:status:42");
    assert_eq!(epoch_key("abc123"), "llm-response-tts:epoch:abc123");
}
