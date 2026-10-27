use std::path::Path;
use std::process::Command;

fn main() {
    commit();
    tauri_build::build()
}

/// The commit this build is made from, for the plates: its short hash,
/// with a trailing `+` when the tree had changes not yet committed, or
/// `unknown` without git. Read from the local repository only.
fn commit() {
    let git = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    };
    let commit = match git(&["rev-parse", "--short", "HEAD"]) {
        Some(hash) if !hash.is_empty() => {
            let dirty = git(&["status", "--porcelain"]).is_some_and(|s| !s.is_empty());
            if dirty {
                hash + "+"
            } else {
                hash
            }
        }
        _ => "unknown".to_string(),
    };
    println!("cargo:rustc-env=BOMBECHAT_COMMIT={commit}");
    // build again when the commit or what is staged changes; git names
    // these files itself, so a worktree's own HEAD is the one watched
    for file in ["HEAD", "index", "logs/HEAD"] {
        if let Some(path) = git(&["rev-parse", "--git-path", file]) {
            if Path::new(&path).exists() {
                println!("cargo:rerun-if-changed={path}");
            }
        }
    }
}
