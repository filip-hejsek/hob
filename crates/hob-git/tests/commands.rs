//! Runs the git commands in temporary repositories, made with the `git`
//! program.

use std::fs;
use std::path::Path;
use std::process::Command;

use futures_executor::block_on_stream;
use hob_core::{Interpreter, Request};
use tempfile::TempDir;

/// The `git` program in `dir`, ignoring the user's configuration.
fn git_command(dir: &Path, args: &[&str]) -> Command {
    let mut command = Command::new("git");
    command
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com");
    command
}

/// Runs the `git` program in `dir`.
fn git(dir: &Path, args: &[&str]) {
    let status = git_command(dir, args).status().unwrap();
    assert!(status.success(), "git {args:?} failed");
}

/// Commits everything in `dir` with the message, at a fixed time `n`
/// seconds after the first commit, so the log's order is known.
fn commit(dir: &Path, n: u32, message: &str) {
    git(dir, &["add", "-A"]);
    let date = format!("@{} +0000", 1_700_000_000 + n);
    let status = git_command(dir, &["commit", "-q", "-m", message])
        .env("GIT_AUTHOR_DATE", &date)
        .env("GIT_COMMITTER_DATE", &date)
        .status()
        .unwrap();
    assert!(status.success(), "commit failed");
}

/// An empty repository on branch `main`, and an interpreter with the git
/// commands in it.
fn setup() -> (Interpreter, TempDir) {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q", "-b", "main"]);
    let mut interpreter = Interpreter::new();
    hob_git::register(&mut interpreter);
    interpreter.set_cwd(dir.path());
    (interpreter, dir)
}

/// A repository with two commits.
fn setup_with_commits() -> (Interpreter, TempDir) {
    let (i, dir) = setup();
    let root = dir.path();
    fs::create_dir(root.join("sub")).unwrap();
    fs::write(root.join("a.txt"), "one\ntwo\nthree\n").unwrap();
    fs::write(root.join("sub/b.txt"), "bee\n").unwrap();
    commit(root, 0, "First commit");
    fs::write(root.join("a.txt"), "one\ntwo\nthree\nfour\n").unwrap();
    commit(root, 1, "Add four\n\nWith a body.");
    (i, dir)
}

/// Everything the command prints and logs until it finishes.
fn run(interpreter: &Interpreter, line: &str) -> Vec<String> {
    block_on_stream(interpreter.run(line).unwrap())
        .map(|request| match request {
            Request::Print(message) => message,
            Request::Log(record) => record.to_string(),
            _ => panic!("unexpected request"),
        })
        .collect()
}

/// `run`, with commit ids replaced by `<id>`, since they depend on the
/// version of git.
fn run_without_ids(interpreter: &Interpreter, line: &str) -> Vec<String> {
    run(interpreter, line)
        .into_iter()
        .map(|message| {
            let lines: Vec<_> = (message.lines())
                .map(|line| match line.split_once(' ') {
                    Some((id, rest))
                        if id.len() >= 7 && id.chars().all(|c| c.is_ascii_hexdigit()) =>
                    {
                        format!("<id> {rest}")
                    }
                    _ => line.to_owned(),
                })
                .collect();
            lines.join("\n")
        })
        .collect()
}

#[test]
fn usage() {
    let (i, _dir) = setup();
    assert!(run(&i, "git")[0].starts_with("error: usage: git status"));
    assert!(run(&i, "git frob")[0].starts_with("error: unknown subcommand \"frob\""));
}

#[test]
fn outside_a_repository() {
    let dir = tempfile::tempdir().unwrap();
    let mut i = Interpreter::new();
    hob_git::register(&mut i);
    i.set_cwd(dir.path());
    // A temporary directory could be inside some repository after all.
    if inside_no_repository(dir.path()) {
        assert_eq!(
            run(&i, "git status"),
            [format!(
                "error: not a git repository (or any of its parents): {}",
                dir.path().display()
            )]
        );
    }
}

/// Whether no parent of `dir` is a repository.
fn inside_no_repository(dir: &Path) -> bool {
    dir.ancestors().all(|d| !d.join(".git").exists())
}

#[test]
fn status_of_an_empty_repository() {
    let (i, _dir) = setup();
    assert_eq!(
        run(&i, "git status"),
        [
            "On branch main (no commits yet)",
            "nothing to commit, working tree clean"
        ]
    );
}

#[test]
fn status_of_a_clean_repository() {
    let (i, _dir) = setup_with_commits();
    assert_eq!(
        run(&i, "git status"),
        ["On branch main", "nothing to commit, working tree clean"]
    );
}

#[test]
fn status_lists_changes() {
    let (i, dir) = setup_with_commits();
    let root = dir.path();
    fs::write(root.join("new.txt"), "new\n").unwrap();
    git(root, &["add", "new.txt"]);
    fs::write(root.join("a.txt"), "changed\n").unwrap();
    fs::remove_file(root.join("sub/b.txt")).unwrap();
    fs::write(root.join("untracked.txt"), "").unwrap();
    fs::create_dir(root.join("dir")).unwrap();
    fs::write(root.join("dir/x"), "").unwrap();
    assert_eq!(
        run(&i, "git status"),
        [
            "On branch main",
            "Changes to be committed:\n  new file:   new.txt",
            "Changes not staged for commit:\n  modified:   a.txt\n  deleted:    sub/b.txt",
            "Untracked files:\n  dir/\n  untracked.txt",
        ]
    );
}

#[test]
fn status_shows_paths_relative_to_the_working_directory() {
    let (i, dir) = setup_with_commits();
    let root = dir.path();
    fs::write(root.join("a.txt"), "changed\n").unwrap();
    fs::write(root.join("sub/b.txt"), "changed\n").unwrap();
    i.set_cwd(root.join("sub"));
    assert_eq!(
        run(&i, "git status"),
        [
            "On branch main",
            "Changes not staged for commit:\n  modified:   ../a.txt\n  modified:   b.txt",
        ]
    );
}

#[test]
fn status_on_a_detached_head() {
    let (i, dir) = setup_with_commits();
    git(dir.path(), &["checkout", "-q", "--detach", "HEAD~"]);
    assert_eq!(
        run_without_ids(&i, "git status")[0]
            .rsplit_once(' ')
            .unwrap()
            .0,
        "HEAD detached at"
    );
}

#[test]
fn log() {
    let (i, _dir) = setup_with_commits();
    assert_eq!(
        run_without_ids(&i, "git log"),
        ["<id> Add four\n<id> First commit"]
    );
    assert_eq!(run_without_ids(&i, "git log HEAD~"), ["<id> First commit"]);
    assert_eq!(
        run_without_ids(&i, "git log -n 1"),
        [
            "<id> Add four",
            "info: showing the first 1 commits; use -n to see more"
        ]
    );
    assert_eq!(run_without_ids(&i, "git log -n1 main~").len(), 1);
    assert_eq!(
        run(&i, "git log -x"),
        ["error: unknown option -x; usage: git log [-n N] [REV]"]
    );
    assert_eq!(run(&i, "git log -n x"), ["error: not a number: \"x\""]);
    assert!(run(&i, "git log nope")[0].starts_with("error: nope: "));
}

#[test]
fn log_of_an_empty_repository() {
    let (i, _dir) = setup();
    assert_eq!(run(&i, "git log"), ["error: there are no commits yet"]);
}

#[test]
fn branch() {
    let (i, dir) = setup_with_commits();
    git(dir.path(), &["branch", "feature"]);
    assert_eq!(run(&i, "git branch"), ["  feature\n* main"]);
    git(dir.path(), &["switch", "-q", "feature"]);
    assert_eq!(run(&i, "git branch"), ["* feature\n  main"]);
}

#[test]
fn branch_of_an_empty_repository() {
    let (i, _dir) = setup();
    assert_eq!(run(&i, "git branch"), ["* main"]);
}

#[test]
fn diff() {
    let (i, dir) = setup_with_commits();
    let root = dir.path();
    fs::write(root.join("a.txt"), "one\n2\nthree\nfour\n").unwrap();
    fs::remove_file(root.join("sub/b.txt")).unwrap();
    assert_eq!(
        run(&i, "git diff"),
        [
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n\
             @@ -1,4 +1,4 @@\n one\n-two\n+2\n three\n four",
            "diff --git a/sub/b.txt b/sub/b.txt\ndeleted file\n\
             --- a/sub/b.txt\n+++ /dev/null\n@@ -1 +0,0 @@\n-bee",
        ]
    );
    // Nothing is staged.
    assert_eq!(run(&i, "git diff --cached"), [] as [String; 0]);
}

#[test]
fn diff_cached() {
    let (i, dir) = setup_with_commits();
    let root = dir.path();
    fs::write(root.join("c.txt"), "sea\n").unwrap();
    fs::write(root.join("a.txt"), "one\n").unwrap();
    git(root, &["add", "c.txt", "a.txt"]);
    // Not staged, so not shown.
    fs::write(root.join("sub/b.txt"), "changed\n").unwrap();
    i.set_cwd(root.join("sub"));
    assert_eq!(
        run(&i, "git diff --cached"),
        [
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n\
             @@ -1,4 +1 @@\n one\n-two\n-three\n-four",
            "diff --git a/c.txt b/c.txt\nnew file\n\
             --- /dev/null\n+++ b/c.txt\n@@ -0,0 +1 @@\n+sea",
        ]
    );
}

#[test]
fn diff_marks_a_missing_final_newline() {
    let (i, dir) = setup_with_commits();
    fs::write(dir.path().join("sub/b.txt"), "bee").unwrap();
    assert_eq!(
        run(&i, "git diff"),
        [
            "diff --git a/sub/b.txt b/sub/b.txt\n--- a/sub/b.txt\n+++ b/sub/b.txt\n\
          @@ -1 +1 @@\n-bee\n+bee\n\\ No newline at end of file"
        ]
    );
}

#[test]
fn diff_of_a_binary_file() {
    let (i, dir) = setup_with_commits();
    fs::write(dir.path().join("a.txt"), b"\0binary").unwrap();
    assert_eq!(
        run(&i, "git diff"),
        ["diff --git a/a.txt b/a.txt\nBinary files a/a.txt and b/a.txt differ"]
    );
}
