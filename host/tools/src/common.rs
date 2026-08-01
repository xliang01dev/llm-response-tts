// Shared by the ingest and clear-speech binaries (src/bin/) via this crate's lib.rs.
// player/ is a separate workspace member with its own dependencies and install location, so
// it keeps its own small copy of read_env_var rather than depending on this crate.

// ingest and clear-speech are installed via `cargo install`, which lands flat in
// ~/.cargo/bin - so unlike the old bin/-relative layout, the repo root can no longer be
// derived from the exe's own path. Same constraint player already has (see its main.rs);
// the hook (and any manual run) is expected to have cwd set to the repo root.
pub fn script_dir() -> std::path::PathBuf {
    std::env::current_dir().expect("failed to get current dir")
}

pub fn read_env_var(path: &std::path::Path, key: &str) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    for line in content.lines() {
        let (k, v) = line.trim().split_once('=')?;
        if k.trim() == key {
            return Some(v.trim().to_string());
        }
    }
    None
}
