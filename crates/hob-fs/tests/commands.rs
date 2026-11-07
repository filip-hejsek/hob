//! Runs the filesystem commands in a temporary directory.

use std::fs;
use std::path::Path;

use futures_executor::block_on_stream;
use hob_core::{Interpreter, Request};
use tempfile::TempDir;

/// An interpreter with the filesystem commands, in a temporary directory
/// with some files.
fn setup() -> (Interpreter, TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir(root.join("sub")).unwrap();
    fs::write(root.join("sub/inner.txt"), "inside\n").unwrap();
    fs::write(root.join("b.txt"), "line 1\nline 2\n").unwrap();
    fs::write(root.join("a.txt"), "no newline").unwrap();
    fs::write(root.join(".hidden"), "").unwrap();
    fs::write(root.join("empty"), "").unwrap();
    fs::write(root.join("binary"), b"\x7fELF\0\0").unwrap();
    let mut interpreter = Interpreter::new();
    hob_fs::register(&mut interpreter);
    interpreter.set_cwd(root);
    (interpreter, dir)
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

/// The path in the temporary directory, as text.
fn path(dir: &TempDir, relative: &str) -> String {
    dir.path().join(relative).display().to_string()
}

#[test]
fn ls() {
    let (i, dir) = setup();
    assert_eq!(run(&i, "ls"), ["a.txt\nb.txt\nbinary\nempty\nsub/"]);
    assert_eq!(
        run(&i, "ls -a"),
        [".hidden\na.txt\nb.txt\nbinary\nempty\nsub/"]
    );
    assert_eq!(run(&i, "ls sub"), ["inner.txt"]);
    assert_eq!(run(&i, "ls a.txt"), ["a.txt"]);
    assert_eq!(
        run(&i, "ls nope sub a.txt"),
        [
            format!("error: {}: No such file or directory", path(&dir, "nope")),
            "sub:\ninner.txt".to_owned(),
            "a.txt".to_owned(),
        ]
    );
    assert_eq!(run(&i, "ls -x"), ["error: unknown option -x"]);
}

#[test]
fn ls_of_an_empty_directory_prints_nothing() {
    let (i, dir) = setup();
    fs::create_dir(dir.path().join("void")).unwrap();
    assert_eq!(run(&i, "ls void"), [] as [String; 0]);
}

#[test]
fn cat() {
    let (i, dir) = setup();
    assert_eq!(run(&i, "cat b.txt"), ["line 1\nline 2"]);
    assert_eq!(
        run(&i, "cat a.txt empty sub/inner.txt"),
        ["no newline", "inside"]
    );
    assert_eq!(
        run(&i, "cat binary sub"),
        [
            format!("error: {}: not a text file", path(&dir, "binary")),
            format!("error: {}: Is a directory", path(&dir, "sub")),
        ]
    );
    assert_eq!(run(&i, "cat"), ["error: usage: cat FILE..."]);
}

#[test]
fn cat_refuses_big_files() {
    let (i, dir) = setup();
    fs::write(dir.path().join("big"), vec![b'x'; 1024 * 1024 + 1]).unwrap();
    assert_eq!(
        run(&i, "cat big"),
        [format!(
            "error: {}: too big to show (over 1 MiB)",
            path(&dir, "big")
        )]
    );
}

#[test]
fn cd_and_pwd() {
    let (i, dir) = setup();
    assert_eq!(run(&i, "pwd"), [dir.path().display().to_string()]);
    assert_eq!(run(&i, "cd sub"), [] as [String; 0]);
    assert_eq!(i.cwd(), dir.path().join("sub"));
    assert_eq!(run(&i, "ls"), ["inner.txt"]);
    assert_eq!(run(&i, "cat inner.txt"), ["inside"]);
    assert_eq!(run(&i, "cd ./../sub/.."), [] as [String; 0]);
    assert_eq!(i.cwd(), dir.path());
    assert_eq!(run(&i, "cd /"), [] as [String; 0]);
    assert_eq!(run(&i, "cd .."), [] as [String; 0]);
    assert_eq!(run(&i, "pwd"), ["/"]);
}

#[test]
#[cfg(unix)]
fn cd_goes_through_symlinks_and_back() {
    let (i, dir) = setup();
    let other = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(other.path(), dir.path().join("link")).unwrap();
    run(&i, "cd link");
    assert_eq!(i.cwd(), dir.path().join("link"));
    run(&i, "cd ..");
    assert_eq!(i.cwd(), dir.path());
}

#[test]
fn cd_without_arguments_goes_home() {
    let (i, _dir) = setup();
    run(&i, "cd");
    assert_eq!(Some(i.cwd()), std::env::home_dir());
}

#[test]
fn cd_checks_the_directory() {
    let (i, dir) = setup();
    assert_eq!(
        run(&i, "cd a.txt"),
        [format!("error: {}: not a directory", path(&dir, "a.txt"))]
    );
    assert_eq!(
        run(&i, "cd nope"),
        [format!(
            "error: {}: No such file or directory",
            path(&dir, "nope")
        )]
    );
    assert_eq!(run(&i, "cd a b"), ["error: usage: cd [DIR]"]);
    assert_eq!(i.cwd(), Path::new(dir.path()));
}
