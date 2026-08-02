// MessageDisplay hook entrypoint. Reads one delta-event JSON payload from stdin, buffers
// deltas per message_id, and on the final delta POSTs the full text to the ingress service
// (through nginx) so it lands on the Redis work queue for the worker containers.
// Rebuild after editing: cargo build --release --manifest-path host/Cargo.toml
use llm_response_tts_tools::common::{http_post, http_status_line, read_env_var, script_dir, session_key};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

// Number/Array carry no payload: only `final`/`message_id`/`delta` (bool/string fields)
// are ever read back out, but the parser still needs to walk past any number or array
// value it encounters elsewhere in the payload to find the fields it does care about.
enum Json {
    Null,
    Bool(bool),
    Number,
    Str(String),
    Array,
    Object(Vec<(String, Json)>),
}

struct Parser<'a> {
    chars: std::iter::Peekable<std::str::Chars<'a>>,
}

impl<'a> Parser<'a> {
    fn new(s: &'a str) -> Self {
        Parser { chars: s.chars().peekable() }
    }

    fn skip_ws(&mut self) {
        while let Some(&c) = self.chars.peek() {
            if c.is_whitespace() {
                self.chars.next();
            } else {
                break;
            }
        }
    }

    fn expect(&mut self, c: char) -> Result<(), String> {
        match self.chars.next() {
            Some(x) if x == c => Ok(()),
            other => Err(format!("expected {:?}, got {:?}", c, other)),
        }
    }

    fn parse_string(&mut self) -> Result<String, String> {
        self.expect('"')?;
        let mut s = String::new();
        loop {
            match self.chars.next() {
                Some('"') => break,
                Some('\\') => match self.chars.next() {
                    Some('"') => s.push('"'),
                    Some('\\') => s.push('\\'),
                    Some('/') => s.push('/'),
                    Some('n') => s.push('\n'),
                    Some('t') => s.push('\t'),
                    Some('r') => s.push('\r'),
                    Some('b') => s.push('\u{8}'),
                    Some('f') => s.push('\u{c}'),
                    Some('u') => {
                        let hex: String = (0..4)
                            .map(|_| self.chars.next().ok_or("bad \\u escape"))
                            .collect::<Result<String, _>>()?;
                        let code = u32::from_str_radix(&hex, 16).map_err(|e| e.to_string())?;
                        if let Some(ch) = char::from_u32(code) {
                            s.push(ch);
                        }
                    }
                    Some(other) => s.push(other),
                    None => return Err("unterminated escape".into()),
                },
                Some(c) => s.push(c),
                None => return Err("unterminated string".into()),
            }
        }
        Ok(s)
    }

    fn parse_literal(&mut self, lit: &str) -> Result<(), String> {
        for expected in lit.chars() {
            self.expect(expected)?;
        }
        Ok(())
    }

    fn parse_number(&mut self) -> Result<f64, String> {
        let mut s = String::new();
        while let Some(&c) = self.chars.peek() {
            if c.is_ascii_digit() || c == '-' || c == '+' || c == '.' || c == 'e' || c == 'E' {
                s.push(c);
                self.chars.next();
            } else {
                break;
            }
        }
        s.parse::<f64>().map_err(|e| e.to_string())
    }

    fn parse_value(&mut self) -> Result<Json, String> {
        self.skip_ws();
        match self.chars.peek() {
            Some('"') => Ok(Json::Str(self.parse_string()?)),
            Some('{') => self.parse_object(),
            Some('[') => self.parse_array(),
            Some('t') => {
                self.parse_literal("true")?;
                Ok(Json::Bool(true))
            }
            Some('f') => {
                self.parse_literal("false")?;
                Ok(Json::Bool(false))
            }
            Some('n') => {
                self.parse_literal("null")?;
                Ok(Json::Null)
            }
            Some(c) if c.is_ascii_digit() || *c == '-' => {
                self.parse_number()?;
                Ok(Json::Number)
            }
            other => Err(format!("unexpected token {:?}", other)),
        }
    }

    fn parse_array(&mut self) -> Result<Json, String> {
        self.expect('[')?;
        self.skip_ws();
        if self.chars.peek() == Some(&']') {
            self.chars.next();
            return Ok(Json::Array);
        }
        loop {
            self.parse_value()?;
            self.skip_ws();
            match self.chars.next() {
                Some(',') => continue,
                Some(']') => break,
                other => return Err(format!("expected ',' or ']', got {:?}", other)),
            }
        }
        Ok(Json::Array)
    }

    fn parse_object(&mut self) -> Result<Json, String> {
        self.expect('{')?;
        let mut pairs = Vec::new();
        self.skip_ws();
        if self.chars.peek() == Some(&'}') {
            self.chars.next();
            return Ok(Json::Object(pairs));
        }
        loop {
            self.skip_ws();
            let key = self.parse_string()?;
            self.skip_ws();
            self.expect(':')?;
            let val = self.parse_value()?;
            pairs.push((key, val));
            self.skip_ws();
            match self.chars.next() {
                Some(',') => continue,
                Some('}') => break,
                other => return Err(format!("expected ',' or '}}', got {:?}", other)),
            }
        }
        Ok(Json::Object(pairs))
    }
}

fn get<'a>(obj: &'a Json, key: &str) -> Option<&'a Json> {
    match obj {
        Json::Object(pairs) => pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v),
        _ => None,
    }
}

fn as_str(v: Option<&Json>) -> String {
    match v {
        Some(Json::Str(s)) => s.clone(),
        _ => String::new(),
    }
}

fn as_bool(v: Option<&Json>) -> bool {
    match v {
        Some(Json::Bool(b)) => *b,
        Some(Json::Str(s)) => s == "true",
        _ => false,
    }
}

// Splits text into sentence-ish chunks so each one is enqueued (and thus synthesized) as
// its own job - the 3 workers can then process one long message in parallel instead of one
// worker doing it serially, while pending_ids still keeps playback in the original order.
// Boundary: . ! ? or : followed by whitespace or end of text. Requiring trailing whitespace
// is what keeps this from splitting decimals ("3.14") or no-space abbreviations for free.
fn split_sentences(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut sentences = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < n {
        let at_boundary = matches!(chars[i], '.' | '!' | '?' | ':')
            && (i + 1 == n || chars[i + 1].is_whitespace());
        if at_boundary {
            let sentence: String = chars[start..=i].iter().collect();
            let trimmed = sentence.trim();
            if !trimmed.is_empty() {
                sentences.push(trimmed.to_string());
            }
            i += 1;
            while i < n && chars[i].is_whitespace() {
                i += 1;
            }
            start = i;
        } else {
            i += 1;
        }
    }
    if start < n {
        let trimmed: String = chars[start..].iter().collect::<String>().trim().to_string();
        if !trimmed.is_empty() {
            sentences.push(trimmed);
        }
    }
    sentences
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn post_text(token: &str, text: &str, session: &str, session_dir: &str) -> std::io::Result<()> {
    let body = format!(
        "{{\"text\":\"{}\",\"session\":\"{}\",\"session_dir\":\"{}\"}}",
        json_escape(text),
        json_escape(session),
        json_escape(session_dir)
    );
    let response = http_post("/", token, &body)?;
    let status_line = http_status_line(&response);
    if !(status_line.contains(" 200 ") || status_line.contains(" 202 ")) {
        eprintln!("ingest: enqueue failed: {status_line}");
    }
    Ok(())
}

fn run() -> std::io::Result<()> {
    let script_dir = script_dir();
    // Session-scoped (not repo-local): this binary is installed once globally and can be
    // invoked concurrently by Claude Code sessions in unrelated projects, each with their own
    // cwd. A single shared buffer dir would let one session's dedupe marker or delta buffer
    // get clobbered by another's. Fixed under /tmp/llm-response-tts (not
    // LLM_RESPONSE_TTS_SOUND_OUTPUT) for the same reason the per-session lock dir is - it
    // should stay predictable even if that env var is reconfigured.
    let (session_hash, session_dir_name) = session_key();
    let out_dir = PathBuf::from("/tmp/llm-response-tts/buffer").join(&session_dir_name);
    std::fs::create_dir_all(&out_dir)?;

    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;

    let json = Parser::new(&input).parse_value().unwrap_or(Json::Object(Vec::new()));
    let final_flag = as_bool(get(&json, "final"));
    let message_id = as_str(get(&json, "message_id"));
    let delta = as_str(get(&json, "delta"));

    if message_id.is_empty() {
        return Ok(());
    }

    let buffer_file = out_dir.join(format!("buffer-{}.txt", message_id));
    if !delta.is_empty() {
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&buffer_file)?;
        f.write_all(delta.as_bytes())?;
    }

    if !final_flag {
        return Ok(());
    }

    let state_file = out_dir.join("ingest-last-message.txt");
    if let Ok(prev) = std::fs::read_to_string(&state_file) {
        if prev == message_id {
            let _ = std::fs::remove_file(&buffer_file);
            return Ok(());
        }
    }
    std::fs::write(&state_file, &message_id)?;

    let text = std::fs::read_to_string(&buffer_file).unwrap_or_default();
    let _ = std::fs::remove_file(&buffer_file);
    if text.is_empty() {
        return Ok(());
    }

    let env_file = script_dir.join("docker").join(".env");
    let token = read_env_var(&env_file, "LLM_RESPONSE_TTS_BEARER_TOKEN").unwrap_or_default();
    for sentence in split_sentences(&text) {
        post_text(&token, &sentence, &session_hash, &session_dir_name)?;
    }

    // player must run from the boot volume - macOS kills CoreAudio-linked (cpal) binaries
    // executed from elsewhere with SIGKILL (Code Signature Invalid), a restriction that doesn't
    // apply to plain binaries like this one. ~/.cargo/bin (or $CARGO_HOME/bin, if set) is always
    // on the boot volume regardless of where this repo lives, so spawning the `cargo install`ed
    // copy from there - named llm-response-tts-player, not just player, since it's installed into
    // a global bin directory shared with every other cargo tool on this machine - sidesteps the
    // issue rather than working around it. Re-run `cargo install --path host/player --force`
    // after editing player's source to pick up the change here.
    // player finds docker/.env via its own CARGO_MANIFEST_DIR (baked in at compile time, same as
    // script_dir() above) and derives its own session_hash from cwd - Command::spawn children
    // inherit the parent's cwd by default, and ours is wherever the hook fired from, which is
    // exactly the cwd this ingest run computed session_hash from too, so the two agree without
    // an explicit handoff. player enforces its own per-session single-instance lock on startup
    // (mkdir /tmp/llm-response-tts/lock/<session-dir>.lock), so it's safe to always attempt a
    // spawn here - a redundant one just exits immediately.
    let cargo_home = std::env::var("CARGO_HOME").map(PathBuf::from).unwrap_or_else(|_| {
        let home = std::env::var("HOME").map(PathBuf::from).unwrap_or_else(|_| script_dir.clone());
        home.join(".cargo")
    });
    let player = cargo_home.join("bin").join("llm-response-tts-player");
    let _ = Command::new(player)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();

    Ok(())
}

fn main() {
    let _ = run();
}

#[cfg(test)]
#[path = "ingest_tests.rs"]
mod tests;
