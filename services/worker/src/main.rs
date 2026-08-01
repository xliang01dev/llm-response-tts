use redis::aio::ConnectionManager;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

const STATUS_TTL_SECS: u64 = 3600;
const WORK_QUEUE_KEY: &str = "kokoros:work_queue";
const EPOCH_KEY: &str = "kokoros:epoch";

fn status_key(id: i64) -> String {
    format!("kokoros:status:{id}")
}

#[derive(Deserialize)]
struct QueuedJob {
    id: i64,
    text: String,
    epoch: i64,
}

#[derive(Serialize)]
struct SpeechRequest<'a> {
    model: &'a str,
    input: &'a str,
    voice: &'a str,
    response_format: &'a str,
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

// --- word-reference / strip-character transform, ported from src/word-refs.rs ---
// JSON parsing uses serde_json here (already a dependency for the Redis job payload and
// the kokoros API call) rather than word-refs.rs's hand-rolled parser, which exists there
// specifically to keep that host-side binary dependency-free.

// Missing/unreadable/unparsable file all just mean "use the default" - none of these
// configs are required for the worker to function.
fn load_json<T: serde::de::DeserializeOwned + Default>(path: &str) -> T {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => {
            tracing::warn!("{path} not found, using default");
            return T::default();
        }
    };
    match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("failed to parse {path}: {e}");
            T::default()
        }
    }
}

fn load_word_references(path: &str) -> Vec<(String, String)> {
    let map: HashMap<String, String> = load_json(path);
    let mut pairs: Vec<(String, String)> = map.into_iter().collect();
    pairs.sort_by(|a, b| b.0.chars().count().cmp(&a.0.chars().count()));
    pairs
}

fn load_strip_chars(path: &str) -> HashSet<char> {
    let entries: Vec<String> = load_json(path);
    entries.into_iter().filter_map(|s| s.chars().next()).collect()
}

fn load_units(path: &str) -> Vec<(String, String, String)> {
    let map: HashMap<String, (String, String)> = load_json(path);
    let mut units: Vec<(String, String, String)> =
        map.into_iter().map(|(key, (singular, plural))| (key, singular, plural)).collect();
    units.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
    units
}

fn is_alnum_key(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric())
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn utf8_char_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b >> 5 == 0b110 {
        2
    } else if b >> 4 == 0b1110 {
        3
    } else if b >> 3 == 0b11110 {
        4
    } else {
        1
    }
}

// Mirrors perl's s/\b\Q$key\E\b/$val/gi for an all-ASCII-alnum key.
fn replace_word_ci(text: &str, key: &str, val: &str) -> String {
    let tb = text.as_bytes();
    let kb = key.as_bytes();
    let klen = kb.len();
    let n = tb.len();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < n {
        if i + klen <= n {
            let window = &tb[i..i + klen];
            let prev_ok = i == 0 || !is_word_byte(tb[i - 1]);
            let next_ok = i + klen == n || !is_word_byte(tb[i + klen]);
            if prev_ok && next_ok && window.eq_ignore_ascii_case(kb) {
                out.push_str(val);
                i += klen;
                continue;
            }
        }
        let ch_len = utf8_char_len(tb[i]);
        out.push_str(std::str::from_utf8(&tb[i..i + ch_len]).unwrap());
        i += ch_len;
    }
    out
}

fn strip_chars(text: &str, chars: &HashSet<char>) -> String {
    if chars.is_empty() {
        return text.to_string();
    }
    text.chars().filter(|c| !chars.contains(c)).collect()
}

// Expands "<number><unit>" tokens (e.g. "24ms", "1in") into "<number> <spoken unit>" so TTS
// doesn't try to sound out the abbreviation. Longest-match on configured units, case-sensitive
// (units like "MB" vs "mb" mean different things), and only when the unit isn't itself the
// start of a longer word (so "24mstest" is left alone).
fn expand_measurements(text: &str, units: &[(String, String, String)]) -> String {
    if units.is_empty() {
        return text.to_string();
    }
    let bytes = text.as_bytes();
    let n = bytes.len();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < n {
        let prev_is_word = i > 0 && is_word_byte(bytes[i - 1]);
        if !prev_is_word && bytes[i].is_ascii_digit() {
            let start = i;
            let mut j = i;
            while j < n && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if j < n && bytes[j] == b'.' && j + 1 < n && bytes[j + 1].is_ascii_digit() {
                j += 1;
                while j < n && bytes[j].is_ascii_digit() {
                    j += 1;
                }
            }
            let num_str = &text[start..j];

            let matched = units.iter().find(|(key, _, _)| {
                let klen = key.len();
                j + klen <= n
                    && bytes[j..j + klen] == key.as_bytes()[..]
                    && (j + klen == n || !is_word_byte(bytes[j + klen]))
            });

            if let Some((key, singular, plural)) = matched {
                out.push_str(num_str);
                out.push(' ');
                out.push_str(if num_str == "1" { singular } else { plural });
                i = j + key.len();
                continue;
            }

            out.push_str(num_str);
            i = j;
            continue;
        }
        let ch_len = utf8_char_len(bytes[i]);
        out.push_str(std::str::from_utf8(&bytes[i..i + ch_len]).unwrap());
        i += ch_len;
    }
    out
}

fn apply_transform(
    text: &str,
    units: &[(String, String, String)],
    refs: &[(String, String)],
    strip_set: &HashSet<char>,
) -> String {
    let mut text = expand_measurements(text, units);
    for (key, val) in refs {
        if is_alnum_key(key) {
            text = replace_word_ci(&text, key, val);
        } else {
            text = text.replace(key.as_str(), val.as_str());
        }
    }
    strip_chars(&text, strip_set)
}

// --- synthesis + output ---

async fn synthesize(
    client: &reqwest::Client,
    base_url: &str,
    voice: &str,
    text: &str,
) -> reqwest::Result<Vec<u8>> {
    let resp = client
        .post(format!("{base_url}/v1/audio/speech"))
        .json(&SpeechRequest {
            model: "kokoro",
            input: text,
            voice,
            response_format: "wav",
        })
        .send()
        .await?
        .error_for_status()?;
    Ok(resp.bytes().await?.to_vec())
}

fn write_output(dir: &Path, id: i64, bytes: &[u8]) -> std::io::Result<()> {
    let tmp_path = dir.join(format!("{:010}.wav.tmp", id));
    let final_path = dir.join(format!("{:010}.wav", id));
    std::fs::write(&tmp_path, bytes)?;
    std::fs::rename(&tmp_path, &final_path)
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let redis_url = env_or("REDIS_URL", "redis://redis:6379");
    let kokoros_url = env_or("KOKOROS_URL", "http://kokoros:3000");
    let voice = env_or("KOKOROS_VOICE", "af_heart");
    let output_dir = PathBuf::from(env_or("OUTPUT_DIR", "/app/output"));
    let word_refs_path = env_or("WORD_REFS_PATH", "/app/word-references.json");
    let strip_chars_path = env_or("STRIP_CHARS_PATH", "/app/strip-characters.json");
    let units_path = env_or("UNITS_PATH", "/app/measurement-units.json");

    std::fs::create_dir_all(&output_dir).expect("failed to create output dir");

    let refs = load_word_references(&word_refs_path);
    let strip_set = load_strip_chars(&strip_chars_path);
    let units = load_units(&units_path);

    let client = redis::Client::open(redis_url.as_str()).expect("invalid REDIS_URL");
    let mut conn = ConnectionManager::new(client)
        .await
        .expect("failed to connect to redis on startup");

    let http = reqwest::Client::new();

    tracing::info!("worker ready, polling {WORK_QUEUE_KEY}");
    loop {
        let popped: Option<(String, String)> = conn
            .brpop(WORK_QUEUE_KEY, 0.0)
            .await
            .expect("BRPOP failed");
        let Some((_key, payload)) = popped else {
            continue;
        };

        let job: QueuedJob = match serde_json::from_str(&payload) {
            Ok(j) => j,
            Err(e) => {
                tracing::warn!("bad job payload, dropping: {e}");
                continue;
            }
        };

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
    }
}
