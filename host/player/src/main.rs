// Plays synthesized wav files in strict id order. Ordering lives entirely in Redis
// (via the ingress service's /next and /ack), so there's no local state that can
// ever drift from what Redis actually has queued. Replaces player.sh: same lock,
// same poll/timeout schedule, but playback is via rodio instead of shelling out to
// ffplay, and HTTP calls are via ureq instead of curl.
use rodio::stream::DeviceSinkBuilder;
use serde::Deserialize;
use std::error::Error;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::time::Duration;

const POLL_INTERVAL: Duration = Duration::from_secs(2);
const MAX_WAIT: Duration = Duration::from_secs(45); // give up on a "PROCESSING" id after this long
const IDLE_EXIT: Duration = Duration::from_secs(10); // exit after this long with nothing pending
const BASE_URL: &str = "http://127.0.0.1:3000";

#[derive(Deserialize)]
struct NextResponse {
    id: i64,
    filename: String,
    status: String,
}

fn read_env_var(path: &Path, key: &str) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    for line in content.lines() {
        let (k, v) = line.trim().split_once('=')?;
        if k.trim() == key {
            return Some(v.trim().to_string());
        }
    }
    None
}

fn process_alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

struct Lock {
    dir: PathBuf,
}

impl Lock {
    // mkdir is atomic at the filesystem level, so if two ingest invocations
    // race to create it, exactly one acquires the lock. If the recorded holder pid
    // isn't running (e.g. a hard kill), the lock is stale - reclaim it instead of
    // blocking playback forever.
    fn acquire(dir: PathBuf) -> Option<Self> {
        if try_create(&dir) {
            return Some(Lock { dir });
        }

        let pid_file = dir.join("pid");
        let held_by = std::fs::read_to_string(&pid_file)
            .ok()
            .and_then(|s| s.trim().parse::<i32>().ok());
        if let Some(pid) = held_by {
            if process_alive(pid) {
                return None;
            }
        }
        eprintln!("player: reclaiming stale lock (pid {held_by:?} not running)");
        let _ = std::fs::remove_file(&pid_file);
        let _ = std::fs::remove_dir(&dir);

        if try_create(&dir) {
            return Some(Lock { dir });
        }
        None
    }
}

fn try_create(dir: &Path) -> bool {
    if std::fs::create_dir(dir).is_ok() {
        let _ = std::fs::write(dir.join("pid"), std::process::id().to_string());
        true
    } else {
        false
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(self.dir.join("pid"));
        let _ = std::fs::remove_dir(&self.dir);
    }
}

enum PollResult {
    Job(NextResponse),
    Empty,
    Transient,
}

fn fetch_next(token: &str) -> PollResult {
    let result = ureq::get(format!("{BASE_URL}/next"))
        .header("Authorization", &format!("Bearer {token}"))
        .call();
    match result {
        Ok(resp) if resp.status() == 204 => PollResult::Empty,
        Ok(mut resp) => match resp.body_mut().read_json::<NextResponse>() {
            Ok(job) => PollResult::Job(job),
            Err(_) => PollResult::Transient,
        },
        Err(_) => PollResult::Transient,
    }
}

fn ack(token: &str, id: i64) {
    let result = ureq::post(format!("{BASE_URL}/ack"))
        .header("Authorization", &format!("Bearer {token}"))
        .send_json(serde_json::json!({ "id": id }));
    match result {
        Ok(resp) if resp.status() == 204 => {}
        Ok(resp) => eprintln!(
            "player: ack for id {id} got http {}, will retry via next poll",
            resp.status()
        ),
        Err(e) => eprintln!("player: ack for id {id} failed: {e}, will retry via next poll"),
    }
}

fn play_wav(mixer: &rodio::mixer::Mixer, path: &Path) -> Result<(), Box<dyn Error>> {
    let file = BufReader::new(File::open(path)?);
    let player = rodio::stream::play(mixer, file)?;
    player.sleep_until_end();
    Ok(())
}

fn main() {
    // This binary is installed outside the repo (see ingest's spawn comment for why), so it
    // can't derive the repo root from its own exe location - LLM_RESPONSE_TTS_ROOT is
    // required, falling back to cwd for manual/dev runs from the repo root.
    let script_dir = std::env::var("LLM_RESPONSE_TTS_ROOT")
        .map(PathBuf::from)
        .or_else(|_| std::env::current_dir())
        .expect("LLM_RESPONSE_TTS_ROOT not set and failed to get current dir");
    let out_dir = script_dir.join("tmp");
    // Fixed system path, not repo-relative like the rest of script_dir's uses below - worker
    // (in its container) and player (on the host) both default to the same literal path
    // independently, so they agree on where wav files are without any coordination, and
    // docker-compose.yml bind-mounts the host path at that identical path in the container.
    let output_dir = std::env::var("LLM_RESPONSE_TTS_SOUND_OUTPUT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp/llm-response-tts/output"));
    let lock_dir = out_dir.join("worker.lock");
    let env_file = script_dir.join("docker").join(".env");

    let _ = std::fs::create_dir_all(&output_dir);

    let Some(_lock) = Lock::acquire(lock_dir) else {
        return; // lock held by a live process - nothing to do
    };

    let token = read_env_var(&env_file, "LLM_RESPONSE_TTS_BEARER_TOKEN").unwrap_or_default();

    let sink = match DeviceSinkBuilder::open_default_sink() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("player: failed to open audio output: {e}");
            return;
        }
    };
    let mixer = sink.mixer();

    let mut idle = Duration::ZERO;
    let mut waited = Duration::ZERO;

    while idle < IDLE_EXIT {
        match fetch_next(&token) {
            PollResult::Empty => {
                idle += POLL_INTERVAL;
                waited = Duration::ZERO;
                std::thread::sleep(POLL_INTERVAL);
            }
            PollResult::Transient => {
                std::thread::sleep(POLL_INTERVAL);
            }
            PollResult::Job(job) => {
                idle = Duration::ZERO;
                let wav_path = output_dir.join(&job.filename);

                if job.status == "COMPLETE" && wav_path.is_file() {
                    if let Err(e) = play_wav(mixer, &wav_path) {
                        eprintln!("player: playback failed for {}: {e}", job.filename);
                    }
                    let _ = std::fs::remove_file(&wav_path);
                    ack(&token, job.id);
                    waited = Duration::ZERO;
                    continue;
                }

                // still PROCESSING (or COMPLETE but the file mysteriously isn't there) -
                // never wait more than MAX_WAIT on one id so a crashed worker can't
                // stall playback forever.
                waited += POLL_INTERVAL;
                if waited >= MAX_WAIT {
                    eprintln!("player: id {} still not playable after {:?}, skipping", job.id, waited);
                    ack(&token, job.id);
                    waited = Duration::ZERO;
                    continue;
                }
                std::thread::sleep(POLL_INTERVAL);
            }
        }
    }
}
