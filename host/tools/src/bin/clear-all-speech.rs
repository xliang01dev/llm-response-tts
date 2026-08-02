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
