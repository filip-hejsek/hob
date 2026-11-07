//! Filesystem commands for hob: `pwd`, `cd`, `ls` and `cat`.
//!
//! They use the interpreter's simulated working directory
//! ([`Context::cwd`]); the process's own isn't changed.

use std::fs;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

use hob_core::{CommandResult, Context, Interpreter};

/// Registers all filesystem commands.
pub fn register(interpreter: &mut Interpreter) {
    interpreter.register("pwd", "Print the working directory", pwd);
    interpreter.register(
        "cd",
        "Change the working directory (to the home directory by default)",
        cd,
    );
    interpreter.register(
        "ls",
        "List directories (the working directory by default); -a shows hidden files",
        ls,
    );
    interpreter.register("cat", "Print the contents of files", cat);
}

/// The most `cat` prints of a file. The scrollback isn't made for more, and
/// it's saved with the session.
const CAT_LIMIT: u64 = 1024 * 1024;

fn pwd(ctx: &mut Context, args: &[String]) -> CommandResult {
    let (_, []) = options(args, "")? else {
        return Err("usage: pwd".into());
    };
    ctx.print(ctx.cwd().display().to_string());
    Ok(())
}

/// Changes the working directory. `..` is resolved lexically, like a shell's
/// `cd` does by default: `cd link/..` goes back to where it started, even
/// if `link` is a symlink to a directory elsewhere.
fn cd(ctx: &mut Context, args: &[String]) -> CommandResult {
    let path = match options(args, "")? {
        (_, []) => std::env::home_dir().ok_or("the home directory is unknown")?,
        (_, [path]) => normalize(&ctx.resolve(path)),
        _ => return Err("usage: cd [DIR]".into()),
    };
    match fs::metadata(&path) {
        Ok(metadata) if metadata.is_dir() => {
            ctx.set_cwd(path);
            Ok(())
        }
        Ok(_) => Err(format!("{}: not a directory", path.display()).into()),
        Err(e) => Err(io_error(&path, &e).into()),
    }
}

/// Removes `.` and resolves `..` without looking at the filesystem. `..` of
/// the root is the root.
fn normalize(path: &Path) -> PathBuf {
    let mut normal = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normal.pop();
            }
            _ => normal.push(component),
        }
    }
    normal
}

/// Lists each directory given (by default the working directory), or names
/// each file given, like `ls -F`: directories end in `/`.
fn ls(ctx: &mut Context, args: &[String]) -> CommandResult {
    let (flags, paths) = options(args, "a")?;
    let all = flags.contains('a');
    let paths = match paths {
        [] => &[".".to_owned()][..],
        paths => paths,
    };
    for path in paths {
        let resolved = ctx.resolve(path);
        let listing = fs::metadata(&resolved).and_then(|metadata| match metadata.is_dir() {
            // With several paths, say which directory is which.
            true if paths.len() > 1 => list(&resolved, all).map(|names| {
                let header = format!("{path}:");
                [header].into_iter().chain(names).collect()
            }),
            true => list(&resolved, all),
            false => Ok(vec![path.clone()]),
        });
        let names = match listing {
            Ok(names) => names,
            Err(e) => {
                // Like ls, go on with the other paths.
                ctx.error(io_error(&resolved, &e));
                continue;
            }
        };
        if !names.is_empty() {
            ctx.print(names.join("\n"));
        }
    }
    Ok(())
}

/// The names in a directory, sorted, with a `/` after directories. Hidden
/// ones (starting with `.`) are left out unless `all` is set.
fn list(dir: &Path, all: bool) -> io::Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let mut name = entry.file_name().to_string_lossy().into_owned();
        if !all && name.starts_with('.') {
            continue;
        }
        // Follows symlinks, so a link to a directory counts as one.
        if entry.path().is_dir() {
            name.push('/');
        }
        names.push(name);
    }
    names.sort();
    Ok(names)
}

/// Prints the contents of each file given. Files that aren't text, or are too
/// big to show, are reported as errors.
fn cat(ctx: &mut Context, args: &[String]) -> CommandResult {
    let (_, paths) = options(args, "")?;
    if paths.is_empty() {
        return Err("usage: cat FILE...".into());
    }
    for path in paths {
        let path = ctx.resolve(path);
        match read_text(&path) {
            // Lines are printed without their line break.
            Ok(text) => match text.strip_suffix('\n').unwrap_or(&text) {
                "" => {}
                text => ctx.print(text),
            },
            Err(e) => ctx.error(e),
        }
    }
    Ok(())
}

/// Reads a text file, up to [`CAT_LIMIT`]. Invalid UTF-8 is replaced.
fn read_text(path: &Path) -> Result<String, String> {
    let mut bytes = Vec::new();
    (fs::File::open(path))
        .and_then(|file| file.take(CAT_LIMIT + 1).read_to_end(&mut bytes))
        .map_err(|e| io_error(path, &e))?;
    let path = path.display();
    if bytes.len() as u64 > CAT_LIMIT {
        return Err(format!("{path}: too big to show (over 1 MiB)"));
    }
    if bytes.contains(&0) {
        return Err(format!("{path}: not a text file"));
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Splits arguments into flags and operands, like `getopt`: arguments
/// starting with `-` before the first operand (or `--`) are flags, each
/// letter separately. `allowed` lists the letters the command takes. A lone
/// `-` is an operand.
fn options<'a>(args: &'a [String], allowed: &str) -> Result<(String, &'a [String]), String> {
    let mut flags = String::new();
    for (i, arg) in args.iter().enumerate() {
        if arg == "--" {
            return Ok((flags, &args[i + 1..]));
        }
        let Some(letters) = arg.strip_prefix('-').filter(|l| !l.is_empty()) else {
            return Ok((flags, &args[i..]));
        };
        for letter in letters.chars() {
            if !allowed.contains(letter) {
                return Err(format!("unknown option -{letter}"));
            }
            flags.push(letter);
        }
    }
    Ok((flags, &[]))
}

/// An I/O error about a path, without the OS error number that Rust adds.
fn io_error(path: &Path, error: &io::Error) -> String {
    let message = error.to_string();
    let message = match message.rfind(" (os error ") {
        Some(i) if message.ends_with(')') => &message[..i],
        _ => &message,
    };
    format!("{}: {message}", path.display())
}
