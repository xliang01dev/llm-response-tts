// Shared by the speak-response and clear-speech binaries (src/bin/) via this crate's lib.rs.
// player/ is a separate workspace member with its own dependencies and install location, so
// it keeps its own small copy of read_env_var rather than depending on this crate.

pub fn script_dir() -> std::path::PathBuf {
    let exe = std::env::current_exe().unwrap().canonicalize().unwrap();
    let bin_dir = exe.parent().expect("executable has no parent dir");
    bin_dir.parent().expect("bin dir has no parent dir").to_path_buf()
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
