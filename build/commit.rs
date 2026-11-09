//! A build script shared by the crates with binaries: it sets `HOB_COMMIT`
//! at compile time to the hash of the checked-out Git commit, for
//! `--version`. Building from a source tree without Git gives "unknown";
//! setting `HOB_COMMIT` when building overrides it.
//!
//! Uncommitted changes aren't noted: the script reruns only when HEAD
//! moves, not when files are edited.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=HOB_COMMIT");
    let commit = match std::env::var("HOB_COMMIT") {
        Ok(commit) => commit,
        Err(_) => {
            for path in watched_paths() {
                println!("cargo:rerun-if-changed={}", path.display());
            }
            git(&["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".to_owned())
        }
    };
    println!("cargo:rustc-env=HOB_COMMIT={commit}");
}

/// The Git files that change when HEAD moves. Only existing ones: Cargo
/// reruns the script every time for a missing one.
fn watched_paths() -> Vec<PathBuf> {
    let mut names = vec![
        "HEAD".to_owned(),
        "logs/HEAD".to_owned(),
        "packed-refs".to_owned(),
    ];
    names.extend(git(&["symbolic-ref", "-q", "HEAD"]));
    names
        .iter()
        .filter_map(|name| git(&["rev-parse", "--path-format=absolute", "--git-path", name]))
        .map(PathBuf::from)
        .filter(|path| path.exists())
        .collect()
}

/// Runs git, giving its output's first line if it succeeds.
fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    let stdout = String::from_utf8(output.stdout).ok()?;
    let line = stdout.lines().next()?.trim();
    (output.status.success() && !line.is_empty()).then(|| line.to_owned())
}
