use super::*;

fn parse(s: &str) -> Json {
    Parser::new(s).parse_value().expect("expected valid JSON")
}

// --- Parser / Json::parse_value ---

#[test]
fn parse_value_main_flow_object_with_mixed_types() {
    let json = parse(r#"{"final":true,"message_id":"abc123","delta":"hello"}"#);
    assert!(as_bool(get(&json, "final")));
    assert_eq!(as_str(get(&json, "message_id")), "abc123");
    assert_eq!(as_str(get(&json, "delta")), "hello");
}

#[test]
fn parse_value_string_escapes() {
    let json = parse(r#"{"delta":"line1\nline2\ttab\\backslash\"quote"}"#);
    assert_eq!(as_str(get(&json, "delta")), "line1\nline2\ttab\\backslash\"quote");
}

#[test]
fn parse_value_unicode_escape() {
    let json = parse(r#"{"delta":"café"}"#);
    assert_eq!(as_str(get(&json, "delta")), "caf\u{e9}");
}

#[test]
fn parse_value_walks_past_number_and_array_without_keeping_payload() {
    // Number/Array fields carry no payload by design (see the module comment) - the parser
    // only needs to walk past them to reach whatever comes after in the same object.
    let json = parse(r#"{"count":42,"tags":["a","b"],"delta":"after"}"#);
    assert_eq!(as_str(get(&json, "delta")), "after");
}

#[test]
fn parse_value_null_and_missing_key() {
    let json = parse(r#"{"delta":null}"#);
    assert_eq!(as_str(get(&json, "delta")), ""); // null isn't Json::Str, as_str defaults to ""
    assert!(get(&json, "missing").is_none());
}

#[test]
fn parse_value_nested_object() {
    let json = parse(r#"{"outer":{"inner":"value"}}"#);
    let outer = get(&json, "outer").expect("outer key present");
    assert_eq!(as_str(get(outer, "inner")), "value");
}

#[test]
fn parse_value_empty_object_and_array() {
    assert!(matches!(parse("{}"), Json::Object(pairs) if pairs.is_empty()));
    assert!(matches!(parse("[]"), Json::Array));
}

#[test]
fn parse_value_rejects_malformed_input() {
    let invalid_inputs = [
        r#"{"final":"#,       // truncated after colon, no value
        r#"{"final": tru}"#,  // bad literal (truncated "true")
        r#""unterminated"#,   // opening quote with no closing quote
        r#"{"a": 1 "b": 2}"#, // missing comma between object entries
        r#""\u12"#,           // \u escape truncated before 4 hex digits
        r#""\uZZZZ""#,        // \u escape with non-hex digits
    ];
    for input in invalid_inputs {
        assert!(Parser::new(input).parse_value().is_err(), "expected error for input: {input}");
    }
}

// --- get / as_str / as_bool ---

#[test]
fn as_bool_reads_bool_and_string_true_false() {
    assert!(as_bool(Some(&Json::Bool(true))));
    assert!(!as_bool(Some(&Json::Bool(false))));
    assert!(as_bool(Some(&Json::Str("true".into()))));
    assert!(!as_bool(Some(&Json::Str("false".into()))));
    assert!(!as_bool(Some(&Json::Str("yes".into())))); // only the literal string "true" counts
    assert!(!as_bool(None));
}

#[test]
fn as_str_reads_string_and_defaults_to_empty_otherwise() {
    assert_eq!(as_str(Some(&Json::Str("hi".into()))), "hi");
    assert_eq!(as_str(Some(&Json::Bool(true))), "");
    assert_eq!(as_str(None), "");
}

// --- split_sentences ---

#[test]
fn split_sentences_cases() {
    let cases = [
        ("Hello world. How are you? Fine!", vec!["Hello world.", "How are you?", "Fine!"]),
        ("Note: this matters.", vec!["Note:", "this matters."]), // colon is a boundary too
        ("Pi is 3.14 today.", vec!["Pi is 3.14 today."]),        // no whitespace after "3." - not a boundary
        ("no ending punctuation here", vec!["no ending punctuation here"]),
        ("", vec![]),
        ("   \n\t  ", vec![]), // whitespace-only
        ("First.    \n\n  Second.", vec!["First.", "Second."]), // whitespace between sentences collapses
        ("Really?! Yes.", vec!["Really?!", "Yes."]), // only the last of a punctuation run is the boundary
    ];
    for (input, expected) in cases {
        assert_eq!(split_sentences(input), expected, "split_sentences({input:?})");
    }
}

// --- json_escape ---

#[test]
fn json_escape_cases() {
    let cases = [
        ("hello world", "hello world"),                 // plain text passes through unchanged
        ("a\"b\\c\nd\te\rf", "a\\\"b\\\\c\\nd\\te\\rf"), // named special chars
        ("\u{1}", "\\u0001"),                            // other control chars use a \u escape
    ];
    for (input, expected) in cases {
        assert_eq!(json_escape(input), expected, "json_escape({input:?})");
    }
}
