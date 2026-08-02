// Drops every session's queued/pending speech, not just the caller's own - see clear-speech
// for the per-session version most usage should reach for instead.
// Rebuild after editing: cargo build --release --manifest-path host/Cargo.toml
use llm_response_tts_tools::common::{http_post, http_status_line, read_env_var, script_dir};

fn clear_all(token: &str) -> std::io::Result<String> {
    http_post("/clear-all", token, "")
}

fn main() {
    let script_dir = script_dir();
    let env_file = script_dir.join("docker").join(".env");
    let token = read_env_var(&env_file, "LLM_RESPONSE_TTS_BEARER_TOKEN").unwrap_or_default();

    match clear_all(&token) {
        Ok(response) if http_status_line(&response).contains(" 204 ") => {
            println!("cleared pending speech for every session");
        }
        Ok(response) => {
            eprintln!("clear-all failed: {}", http_status_line(&response));
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("clear-all failed: {e}");
            std::process::exit(1);
        }
    }
}
