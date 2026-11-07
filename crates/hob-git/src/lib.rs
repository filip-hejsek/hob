//! Git commands for hob, using gitoxide (`gix`) rather than the `git`
//! program: `git status`, `git log`, `git branch` and `git diff`.
//!
//! The repository is found from the interpreter's working directory
//! ([`Context::cwd`]), and paths are shown relative to it, as git does.

use std::fmt;
use std::path::{Component, Path, PathBuf};

use gix::bstr::{BStr, ByteSlice};
use gix::diff::blob::unified_diff::{ConsumeHunk, ContextSize, DiffLineKind, HunkHeader};
use gix::diff::blob::{Algorithm, InternedInput, UnifiedDiff};
use gix::status::index_worktree::Item as WorktreeItem;
use gix::status::index_worktree::iter::Summary;
use gix::status::{Item, UntrackedFiles};
use hob_core::args::{CompletionContext, Kind, Matches, Spec};
use hob_core::{CommandResult, Context, Interpreter};

/// Registers the `git` command.
pub fn register(interpreter: &mut Interpreter) {
    let spec = Spec::new()
        .subcommand("status", "Show the changes in the worktree", Spec::new())
        .subcommand(
            "log",
            "Show commits, newest first",
            Spec::new()
                .option(
                    "max-count",
                    "N",
                    Kind::Text,
                    "Show N commits (20 by default)",
                )
                .short('n')
                .arg("REV", Kind::custom(revisions))
                .optional(),
        )
        .subcommand("branch", "List the branches", Spec::new())
        .subcommand(
            "diff",
            "Show unstaged changes",
            Spec::new()
                .flag("cached", "Show staged changes instead")
                .flag("staged", "The same as --cached"),
        );
    interpreter.register_spec(
        "git",
        "Inspect a Git repository: git status, log, branch or diff",
        spec,
        git,
    );
}

/// `HEAD` and the names of the local branches and tags, for completion.
fn revisions(ctx: &CompletionContext) -> Vec<String> {
    let Ok(repo) = gix::discover(ctx.cwd()) else {
        return Vec::new();
    };
    let Ok(references) = repo.references() else {
        return Vec::new();
    };
    let mut names = vec!["HEAD".to_owned()];
    for iter in [references.local_branches(), references.tags()] {
        let Ok(iter) = iter else { continue };
        for reference in iter.flatten() {
            names.push(reference.name().shorten().to_str_lossy().into_owned());
        }
    }
    names
}

/// How many commits `git log` shows unless `-n` says otherwise. The
/// scrollback isn't made for a whole history, and it's saved with the session.
const LOG_LIMIT: usize = 20;

/// The most `git diff` prints, in bytes, like `cat`'s limit.
const DIFF_LIMIT: usize = 1024 * 1024;

fn git(ctx: &mut Context, args: &Matches) -> CommandResult {
    let (subcommand, args) = args.subcommand().expect("git has subcommands");
    let result = match subcommand {
        "status" => status(ctx),
        "log" => log(ctx, args),
        "branch" => branch(ctx),
        "diff" => diff(ctx, args),
        _ => unreachable!("subcommands are declared"),
    };
    Ok(result?)
}

/// A failed git command, with gix's chain of causes on one line.
#[derive(Debug)]
struct Failure(String);

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Failure {}

impl From<gix::Error> for Failure {
    fn from(error: gix::Error) -> Self {
        Failure(format!("{error:#}"))
    }
}

impl From<String> for Failure {
    fn from(message: String) -> Self {
        Failure(message)
    }
}

impl From<&str> for Failure {
    fn from(message: &str) -> Self {
        Failure(message.to_owned())
    }
}

type Result<T, E = Failure> = std::result::Result<T, E>;

/// The repository containing the working directory, and the working
/// directory relative to its worktree (empty at the top, `None` if it's
/// not inside the worktree, e.g. in a bare repository).
struct Repo {
    repo: gix::Repository,
    prefix: Option<PathBuf>,
}

impl Repo {
    fn discover(ctx: &Context) -> Result<Self> {
        let cwd = ctx.cwd();
        let repo = gix::discover(cwd).map_err(|_| {
            Failure(format!(
                "not a git repository (or any of its parents): {}",
                cwd.display()
            ))
        })?;
        // Compare real paths: the working directory may go through a symlink.
        let prefix = (repo.workdir())
            .and_then(|root| Some((root.canonicalize().ok()?, cwd.canonicalize().ok()?)))
            .and_then(|(root, cwd)| Some(cwd.strip_prefix(root).ok()?.to_owned()));
        Ok(Repo { repo, prefix })
    }

    /// A repository-relative path (with `/` separators) as git shows it:
    /// relative to the working directory.
    fn show(&self, path: &BStr) -> String {
        let path = path.to_str_lossy();
        match &self.prefix {
            Some(prefix) => relative(&path, prefix),
            None => path.into_owned(),
        }
    }

    fn workdir(&self) -> Result<&Path> {
        (self.repo.workdir()).ok_or_else(|| "this operation must be run in a work tree".into())
    }
}

/// `path` (relative to the top of the worktree, with `/` separators) relative
/// to `dir` (also relative to the top), using `..` where needed.
fn relative(path: &str, dir: &Path) -> String {
    let dir: Vec<_> = (dir.components())
        .filter_map(|c| match c {
            Component::Normal(name) => Some(name.to_string_lossy()),
            _ => None,
        })
        .collect();
    let parts: Vec<&str> = path.split('/').collect();
    let common = (dir.iter())
        .zip(&parts)
        .take_while(|(a, b)| a == *b)
        .count();
    let mut result: Vec<&str> = vec![".."; dir.len() - common];
    result.extend(&parts[common..]);
    result.join("/")
}

/// Prints the current branch, then the changes staged for commit, those not
/// staged, and untracked files, like a short `git status`.
fn status(ctx: &mut Context) -> Result<()> {
    let repo = Repo::discover(ctx)?;
    repo.workdir()?;
    ctx.print(head_description(&repo.repo)?);

    let mut staged = Vec::new();
    let mut unstaged = Vec::new();
    let mut untracked = Vec::new();
    let mut conflicts = Vec::new();
    let items = (repo.repo.status(gix::progress::Discard)?)
        .untracked_files(UntrackedFiles::Collapsed)
        .into_iter(None)?;
    for item in items {
        match item? {
            Item::TreeIndex(change) => {
                use gix::diff::index::ChangeRef;
                let line = match &change {
                    ChangeRef::Addition { location, .. } => {
                        format!("new file:   {}", repo.show(location.as_ref()))
                    }
                    ChangeRef::Deletion { location, .. } => {
                        format!("deleted:    {}", repo.show(location.as_ref()))
                    }
                    ChangeRef::Modification { location, .. } => {
                        format!("modified:   {}", repo.show(location.as_ref()))
                    }
                    ChangeRef::Rewrite {
                        source_location,
                        location,
                        copy,
                        ..
                    } => format!(
                        "{}{} -> {}",
                        if *copy {
                            "copied:     "
                        } else {
                            "renamed:    "
                        },
                        repo.show(source_location.as_ref()),
                        repo.show(location.as_ref()),
                    ),
                };
                staged.push(line);
            }
            Item::IndexWorktree(item) => {
                let path = repo.show(item.rela_path());
                match item.summary() {
                    Some(Summary::Added) => {
                        let directory = matches!(
                            &item,
                            WorktreeItem::DirectoryContents { entry, .. }
                                if entry.disk_kind == Some(gix::dir::entry::Kind::Directory)
                        );
                        untracked.push(if directory { path + "/" } else { path });
                    }
                    Some(Summary::Conflict) => conflicts.push(path),
                    Some(Summary::Modified) => unstaged.push(format!("modified:   {path}")),
                    Some(Summary::Removed) => unstaged.push(format!("deleted:    {path}")),
                    Some(Summary::TypeChange) => unstaged.push(format!("typechange: {path}")),
                    Some(Summary::IntentToAdd) => unstaged.push(format!("new file:   {path}")),
                    Some(Summary::Renamed | Summary::Copied) | None => {}
                }
            }
        }
    }
    // Changes come from several threads, in no particular order.
    let sections = [
        ("Unmerged paths:", conflicts),
        ("Changes to be committed:", staged),
        ("Changes not staged for commit:", unstaged),
        ("Untracked files:", untracked),
    ];
    let mut clean = true;
    for (title, mut lines) in sections {
        if lines.is_empty() {
            continue;
        }
        clean = false;
        lines.sort_by(|a, b| sort_key(a).cmp(sort_key(b)));
        lines.dedup();
        let lines: Vec<_> = lines.iter().map(|line| format!("  {line}")).collect();
        ctx.print(format!("{title}\n{}", lines.join("\n")));
    }
    if clean {
        ctx.print("nothing to commit, working tree clean");
    }
    Ok(())
}

/// Sorts status lines by path, not by the kind of change before it.
fn sort_key(line: &str) -> &str {
    line.split_once(": ")
        .map_or(line, |(_, path)| path.trim_start())
}

/// The first line of `git status`: `On branch NAME`, or where a detached
/// `HEAD` is.
fn head_description(repo: &gix::Repository) -> Result<String> {
    if let Some(name) = repo.head_name()? {
        let branch = name.shorten();
        if repo.head()?.is_unborn() {
            return Ok(format!("On branch {branch} (no commits yet)"));
        }
        return Ok(format!("On branch {branch}"));
    }
    let id = repo.head_id()?;
    Ok(format!("HEAD detached at {}", id.shorten_or_id()))
}

/// Prints commits reachable from `REV` (by default `HEAD`), newest first, one
/// line each like `git log --oneline`: at most `N` (by default
/// [`LOG_LIMIT`]).
fn log(ctx: &mut Context, args: &Matches) -> Result<()> {
    let limit = args.parse("max-count")?.unwrap_or(LOG_LIMIT);
    let rev = args.value("REV");
    let repo = Repo::discover(ctx)?;
    let rev = rev.unwrap_or("HEAD");
    let tip = match repo.repo.rev_parse_single(rev) {
        Ok(id) => id.object()?.peel_to_commit()?.id,
        Err(_) if rev == "HEAD" && repo.repo.head()?.is_unborn() => {
            return Err("there are no commits yet".into());
        }
        Err(e) => return Err(format!("{rev}: {e:#}").into()),
    };
    use gix::revision::walk::Sorting;
    use gix::traverse::commit::simple::CommitTimeOrder;
    let walk = (repo.repo.rev_walk([tip]))
        .sorting(Sorting::ByCommitTime(CommitTimeOrder::NewestFirst))
        .all()?;
    let mut lines = Vec::new();
    let mut more = false;
    for info in walk {
        if lines.len() == limit {
            more = true;
            break;
        }
        let commit = info?.object()?;
        let summary = commit.message()?.summary().to_str_lossy().into_owned();
        lines.push(format!("{} {summary}", commit.id().shorten_or_id()));
    }
    if !lines.is_empty() {
        ctx.print(lines.join("\n"));
    }
    if more {
        ctx.info(format!(
            "showing the first {limit} commits; use -n to see more"
        ));
    }
    Ok(())
}

/// Lists local branches, marking the current one with `*`.
fn branch(ctx: &mut Context) -> Result<()> {
    let repo = Repo::discover(ctx)?;
    let head = repo.repo.head_name()?;
    let mut lines = Vec::new();
    if head.is_none() {
        let id = repo.repo.head_id()?;
        lines.push(format!("* (HEAD detached at {})", id.shorten_or_id()));
    }
    let references = repo.repo.references()?;
    let mut branches = Vec::new();
    for reference in references.local_branches()? {
        let reference = reference.map_err(|e| Failure(e.to_string()))?;
        let current = head.as_ref().map(|head| head.as_ref()) == Some(reference.name());
        branches.push((reference.name().shorten().to_string(), current));
    }
    // An unborn branch has no reference yet, but git lists it anyway.
    if let Some(head) = &head
        && repo.repo.head()?.is_unborn()
    {
        branches.push((head.shorten().to_string(), true));
    }
    branches.sort();
    lines.extend(
        (branches.into_iter())
            .map(|(name, current)| format!("{} {name}", if current { '*' } else { ' ' })),
    );
    if !lines.is_empty() {
        ctx.print(lines.join("\n"));
    }
    Ok(())
}

/// One side of a file's change, for `git diff`.
enum Side {
    /// The file doesn't exist on this side.
    Absent,
    /// A blob in the object database.
    Blob(gix::ObjectId),
    /// A file in the worktree.
    File(PathBuf),
}

/// Prints the changes not staged for commit (the worktree against the index),
/// or with `--cached`, the staged ones (the index against `HEAD`), as unified
/// diffs, one message per file.
///
/// Files are compared as they are stored, without git's filters (such as
/// line ending conversion) or diff drivers.
fn diff(ctx: &mut Context, args: &Matches) -> Result<()> {
    let cached = args.flag("cached") || args.flag("staged");
    let repo = Repo::discover(ctx)?;
    let workdir = repo.workdir()?.to_owned();
    let mut changes = Vec::new();
    let status = (repo.repo.status(gix::progress::Discard)?).untracked_files(UntrackedFiles::None);
    if cached {
        for item in status.into_iter(None)? {
            let Item::TreeIndex(change) = item? else {
                continue;
            };
            use gix::diff::index::ChangeRef;
            let change = match change {
                ChangeRef::Addition { location, id, .. } => (
                    location.into_owned(),
                    Side::Absent,
                    Side::Blob(id.into_owned()),
                ),
                ChangeRef::Deletion { location, id, .. } => (
                    location.into_owned(),
                    Side::Blob(id.into_owned()),
                    Side::Absent,
                ),
                ChangeRef::Modification {
                    location,
                    previous_id,
                    id,
                    ..
                }
                | ChangeRef::Rewrite {
                    location,
                    source_id: previous_id,
                    id,
                    ..
                } => (
                    location.into_owned(),
                    Side::Blob(previous_id.into_owned()),
                    Side::Blob(id.into_owned()),
                ),
            };
            changes.push(change);
        }
    } else {
        for item in status.into_index_worktree_iter(None)? {
            let item = item?;
            let after = match item.summary() {
                Some(Summary::Modified | Summary::TypeChange) => {
                    Side::File(workdir.join(gix::path::from_bstr(item.rela_path())?))
                }
                Some(Summary::Removed) => Side::Absent,
                _ => continue,
            };
            let WorktreeItem::Modification { entry, .. } = &item else {
                continue;
            };
            // Submodules are left out, as their content isn't a file.
            if entry.mode == gix::index::entry::Mode::COMMIT {
                continue;
            }
            changes.push((item.rela_path().to_owned(), Side::Blob(entry.id), after));
        }
    }
    changes.sort_by(|a, b| a.0.cmp(&b.0));

    let mut printed = 0;
    for (path, before, after) in changes {
        let text = file_diff(&repo, path.as_ref(), &before, &after)?;
        printed += text.len();
        if printed > DIFF_LIMIT {
            ctx.warn("the diff is too big to show in full (over 1 MiB)");
            break;
        }
        ctx.print(text);
    }
    Ok(())
}

/// The unified diff of one file, with a git-style header, without the final
/// line break.
fn file_diff(repo: &Repo, path: &BStr, before: &Side, after: &Side) -> Result<String> {
    let read = |side: &Side| -> Result<Vec<u8>> {
        Ok(match side {
            Side::Absent => Vec::new(),
            Side::Blob(id) => repo.repo.find_blob(*id)?.take_data(),
            // A symlink's content is its target, as git stores it.
            Side::File(path) => match std::fs::symlink_metadata(path) {
                Ok(m) if m.is_symlink() => {
                    let target = std::fs::read_link(path).map_err(|e| io_error(path, &e))?;
                    gix::path::into_bstr(target)?.into_owned().into()
                }
                _ => std::fs::read(path).map_err(|e| io_error(path, &e))?,
            },
        })
    };
    let (old, new) = (read(before)?, read(after)?);
    // Like git, diffs name files from the top of the worktree.
    let shown = path.to_str_lossy();
    let mut text = format!("diff --git a/{shown} b/{shown}\n");
    let (old_name, new_name) = match (before, after) {
        (Side::Absent, _) => {
            text.push_str("new file\n");
            ("/dev/null".to_owned(), format!("b/{shown}"))
        }
        (_, Side::Absent) => {
            text.push_str("deleted file\n");
            (format!("a/{shown}"), "/dev/null".to_owned())
        }
        _ => (format!("a/{shown}"), format!("b/{shown}")),
    };
    if old.contains(&0) || new.contains(&0) {
        text.push_str(&format!("Binary files {old_name} and {new_name} differ"));
        return Ok(text);
    }
    if old != new {
        text.push_str(&format!("--- {old_name}\n+++ {new_name}\n"));
        let input = InternedInput::new(old.as_slice(), new.as_slice());
        let mut diff = gix::diff::blob::Diff::compute(Algorithm::Histogram, &input);
        diff.postprocess_lines(&input);
        let hunks = UnifiedDiff::new(&diff, &input, Hunks(text), ContextSize::symmetrical(3));
        text = hunks.consume().map_err(|e| Failure(e.to_string()))?;
    }
    Ok(text.trim_end_matches('\n').to_owned())
}

/// Appends hunks to a diff's text, formatted as git does.
struct Hunks(String);

impl ConsumeHunk for Hunks {
    type Out = String;

    fn consume_hunk(
        &mut self,
        header: HunkHeader,
        lines: &[(DiffLineKind, &[u8])],
    ) -> std::io::Result<()> {
        // An empty range starts at the line before it, and a length of 1 is
        // left out.
        let range = |start: u32, len: u32| match len {
            0 => format!("{},0", start.saturating_sub(1)),
            1 => start.to_string(),
            _ => format!("{start},{len}"),
        };
        let text = &mut self.0;
        text.push_str(&format!(
            "@@ -{} +{} @@\n",
            range(header.before_hunk_start, header.before_hunk_len),
            range(header.after_hunk_start, header.after_hunk_len),
        ));
        for (kind, line) in lines {
            text.push(kind.to_prefix());
            text.push_str(&line.to_str_lossy());
            if !line.ends_with(b"\n") {
                text.push_str("\n\\ No newline at end of file\n");
            }
        }
        Ok(())
    }

    fn finish(self) -> String {
        self.0
    }
}

/// An I/O error about a path, without the OS error number that Rust adds.
fn io_error(path: &Path, error: &std::io::Error) -> Failure {
    let message = error.to_string();
    let message = match message.rfind(" (os error ") {
        Some(i) if message.ends_with(')') => &message[..i],
        _ => &message,
    };
    Failure(format!("{}: {message}", path.display()))
}
