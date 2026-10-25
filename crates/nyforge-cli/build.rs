//! Puts a build identifier next to the version in `nyforge --version`, so you can tell which source a binary came from:
//! the git commit and date when built from a clone, or the same two values recorded in BUILD_ID when built from a source archive.
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string()).filter(|s| !s.is_empty())
}

fn main() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    println!("cargo:rerun-if-changed={}", root.join("BUILD_ID").display());
    println!("cargo:rerun-if-changed={}", root.join(".git/HEAD").display());
    let from_git = git(&["rev-parse", "--short=7", "HEAD"]).map(|h| match git(&["log", "-1", "--format=%cs"]) {
        Some(d) => format!("{h}, {d}"),
        None => h,
    });
    let from_file = std::fs::read_to_string(root.join("BUILD_ID")).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty() && !s.contains("$Format"));
    let id = from_git.or(from_file).unwrap_or_else(|| "unknown build".into());
    println!("cargo:rustc-env=NYFORGE_BUILD={id}");
}
