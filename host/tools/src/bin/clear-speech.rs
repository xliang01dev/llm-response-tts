// Drops every queued message so nothing more plays after whatever's currently speaking
// finishes. Doesn't interrupt audio already playing - see README's "Message queueing"
// section for why (the player binary blocks until playback finishes; stopping mid-sentence
// would need a different, more invasive design).
// Rebuild after editing: cargo build --release --manifest-path host/Cargo.toml
use kokoros_tools::common::{read_env_var, script_dir};
use std::io::{Read, Write};
use std::net::TcpStream;

fn clear(token: &str) -> std::io::Result<String> {
    let mut stream = TcpStream::connect(("127.0.0.1", 3000))?;
    let request = format!(
        "POST /clear HTTP/1.1\r\nHost: 127.0.0.1:3000\r\nAuthorization: Bearer {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
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
    let token = read_env_var(&env_file, "KOKOROS_BEARER_TOKEN").unwrap_or_default();

    match clear(&token) {
        Ok(response) if response.lines().next().unwrap_or("").contains(" 204 ") => {
            println!("cleared pending speech");
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
