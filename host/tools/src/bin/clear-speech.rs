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
