// Shared by the ingest and clear-speech binaries (src/bin/) via this crate's lib.rs.
// player/ is a separate workspace member with its own dependencies and install location, so
// it keeps its own small copy of read_env_var rather than depending on this crate.

// ingest and clear-speech are installed via `cargo install`, which lands flat in
// ~/.cargo/bin - so unlike the old bin/-relative layout, the repo root can no longer be
// derived from the exe's own path at runtime. It also can't come from cwd: the whole point of
// installing these globally is that a MessageDisplay hook in some *other* project's
// .claude/settings.json can invoke `ingest` while Claude Code's cwd is that other project, not
// this repo. So the repo root is instead baked into the binary at compile time, via
// CARGO_MANIFEST_DIR (this crate's own Cargo.toml location during that `cargo install`) - it
// always resolves back to this checkout's docker/.env, tmp/, etc regardless of where or how
// the binary is later invoked. Re-run `cargo install` after moving the repo to pick up the new
// location.
pub fn script_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("CARGO_MANIFEST_DIR has unexpected shape")
        .to_path_buf()
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
