//! Commands that work with any frontend. Register them with [`register`].
//!
//! - `!COMMAND`: runs a shell command with `sh -c`, in the working directory,
//!   printing its output (stdout and stderr) as it comes.
//! - `clear`: clears the scrollback.
//! - `exit`: exits hob; if other commands are running, asks first, listing
//!   them.
//! - `help [COMMAND]`: lists all commands with their descriptions, or shows
//!   the description of one.
//! - `jobs`: lists the other commands still running, oldest first.
//! - `messages [LEVEL]`: lists the log records the frontend has, oldest
//!   first, from `LEVEL` up (`info` by default, so `messages debug` lists
//!   all).

use std::io::Read;
use std::process::Stdio;
use std::sync::mpsc;
use std::thread;

use jiff::tz::TimeZone;
use jiff::{Timestamp, Zoned};

use kettle_ui::widget::{button_danger, cancel_button, column, label, row};
use kettle_ui::{IslandBuilder, Outcome};

use crate::{CommandResult, Context, Interpreter, Level, LogRecord};

/// Registers all built-in commands.
pub fn register(interpreter: &mut Interpreter) {
    interpreter.register_prefix("!", "Run a shell command: !COMMAND", shell);
    interpreter.register("clear", "Clear the scrollback", clear);
    interpreter.register("exit", "Exit hob", exit);
    interpreter.register("help", "List the commands, or describe one", help);
    interpreter.register("jobs", "List the other running commands", jobs);
    interpreter.register(
        "messages",
        "List log messages, from LEVEL up (info by default)",
        messages,
    );
}

/// The most output of a shell command that's shown. The scrollback isn't
/// made for more, and it's saved with the session.
const SHELL_OUTPUT_LIMIT: usize = 1024 * 1024;

/// Runs `command` with `sh -c`, with pipes for stdio. Nothing is written to
/// its stdin, which is closed right away. Its stdout and stderr are printed
/// as they come, a chunk of whole lines at a time, up to
/// [`SHELL_OUTPUT_LIMIT`]; the rest is read but dropped. Exiting with a
/// failure is an error.
fn shell(ctx: &mut Context, command: &str) -> CommandResult {
    if command.trim().is_empty() {
        return Err("usage: !COMMAND".into());
    }
    let mut child = std::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(ctx.cwd())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("couldn't run sh: {e}"))?;
    drop(child.stdin.take());
    let pipes: [Box<dyn Read + Send>; 2] = [
        Box::new(child.stdout.take().expect("stdout is piped")),
        Box::new(child.stderr.take().expect("stderr is piped")),
    ];
    let (sender, receiver) = mpsc::channel();
    thread::scope(|scope| {
        for (index, mut pipe) in pipes.into_iter().enumerate() {
            let sender = sender.clone();
            scope.spawn(move || {
                let mut buffer = vec![0; 64 * 1024];
                // A read error is treated like the end of the output.
                while let Ok(n @ 1..) = pipe.read(&mut buffer) {
                    let _ = sender.send((index, buffer[..n].to_vec()));
                }
            });
        }
        drop(sender);
        let mut output = ShellOutput::default();
        for (index, chunk) in receiver {
            output.add(ctx, index, &chunk);
        }
        output.finish(ctx);
    });
    let status = child.wait()?;
    match status.code() {
        _ if status.success() => Ok(()),
        Some(code) => Err(format!("exited with status {code}").into()),
        None => Err("was killed by a signal".into()),
    }
}

/// Prints a shell command's output: whole lines, each stream's own.
#[derive(Default)]
struct ShellOutput {
    /// The incomplete last line of stdout and of stderr.
    partial: [Vec<u8>; 2],
    /// How many bytes were printed.
    printed: usize,
    /// Whether output was dropped over the limit.
    truncated: bool,
}

impl ShellOutput {
    /// Adds a chunk read from stream `index`, printing its whole lines.
    fn add(&mut self, ctx: &Context, index: usize, chunk: &[u8]) {
        if self.truncated {
            return;
        }
        let partial = &mut self.partial[index];
        partial.extend_from_slice(chunk);
        if partial.len() > SHELL_OUTPUT_LIMIT {
            // A line too long to show whole.
            let line = std::mem::take(partial);
            self.print(ctx, &line);
        } else if let Some(end) = partial.iter().rposition(|&b| b == b'\n') {
            let rest = partial.split_off(end + 1);
            let lines = std::mem::replace(partial, rest);
            self.print(ctx, &lines);
        }
    }

    /// Prints the incomplete last lines.
    fn finish(mut self, ctx: &Context) {
        for partial in std::mem::take(&mut self.partial) {
            self.print(ctx, &partial);
        }
    }

    /// Prints `bytes` (without a final line break), as far as the limit
    /// allows; warns once when it's reached.
    fn print(&mut self, ctx: &Context, bytes: &[u8]) {
        if bytes.is_empty() || self.truncated {
            return;
        }
        let left = SHELL_OUTPUT_LIMIT - self.printed;
        let shown = &bytes[..bytes.len().min(left)];
        self.printed += shown.len();
        let shown = shown.strip_suffix(b"\n").unwrap_or(shown);
        if !shown.is_empty() {
            ctx.print(String::from_utf8_lossy(shown));
        }
        if bytes.len() > left {
            self.truncated = true;
            ctx.warn("output truncated at 1 MiB");
        }
    }
}

fn clear(ctx: &mut Context, args: &[String]) -> CommandResult {
    if !args.is_empty() {
        return Err("usage: clear".into());
    }
    ctx.clear();
    Ok(())
}

fn exit(ctx: &mut Context, args: &[String]) -> CommandResult {
    if !args.is_empty() {
        return Err("usage: exit".into());
    }
    let running = ctx.running_commands()?;
    if !running.is_empty() {
        // "Yes" isn't a submit button, so Enter can't confirm by accident.
        let island = IslandBuilder::new(column(
            [label("These commands are still running:")]
                .into_iter()
                .chain(running.into_iter().map(label))
                .chain([
                    label("Exit anyway? They will be interrupted."),
                    row([button_danger("exit", "Yes [y]"), cancel_button("No [n]")]),
                ]),
        ));
        if !matches!(ctx.show(island)?, Outcome::Pressed(..)) {
            ctx.print("Not exiting.");
            return Ok(());
        }
    }
    ctx.exit();
    Ok(())
}

fn help(ctx: &mut Context, args: &[String]) -> CommandResult {
    let line = |(name, description)| format!("{name} — {description}");
    match args {
        [] => {
            let lines: Vec<String> = ctx.commands().map(line).collect();
            ctx.print(lines.join("\n"));
        }
        [name] => match ctx.commands().find(|(n, _)| n == name) {
            Some(command) => ctx.print(line(command)),
            None => return Err(format!("no command {name:?}").into()),
        },
        _ => return Err("usage: help [COMMAND]".into()),
    }
    Ok(())
}

fn jobs(ctx: &mut Context, args: &[String]) -> CommandResult {
    if !args.is_empty() {
        return Err("usage: jobs".into());
    }
    let running = ctx.running_commands()?;
    match running.is_empty() {
        true => ctx.print("No other commands are running."),
        false => ctx.print(running.join("\n")),
    }
    Ok(())
}

fn messages(ctx: &mut Context, args: &[String]) -> CommandResult {
    let level = match args {
        [] => Level::Info,
        [level] => level.parse()?,
        _ => return Err("usage: messages [LEVEL]".into()),
    };
    let now = Timestamp::now().to_zoned(TimeZone::system());
    let lines: Vec<String> = (ctx.log_records()?.iter())
        .filter(|record| record.level <= level)
        .map(|record| format_record(record, &now))
        .collect();
    // One print, rather than one per record.
    match lines.is_empty() {
        true => ctx.print("No messages."),
        false => ctx.print(lines.join("\n")),
    }
    Ok(())
}

/// `time level target: message`. The time is in the time zone of `now`, with
/// the date only if it isn't today.
fn format_record(record: &LogRecord, now: &Zoned) -> String {
    let time = match Timestamp::try_from(record.time) {
        Ok(time) => {
            let time = time.to_zoned(now.time_zone().clone());
            let format = match time.date() == now.date() {
                true => "%H:%M:%S",
                false => "%Y-%m-%d %H:%M:%S",
            };
            time.strftime(format).to_string()
        }
        Err(_) => "?".to_owned(),
    };
    let LogRecord {
        level,
        target,
        message,
        ..
    } = record;
    format!("{time} {level} {target}: {message}")
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use futures_core::Stream;

    use super::*;
    use crate::Request;

    fn record(level: Level, target: &str, message: &str, secs: u64) -> LogRecord {
        LogRecord {
            time: SystemTime::UNIX_EPOCH + Duration::from_secs(secs),
            ..LogRecord::new(level, target, message)
        }
    }

    #[test]
    fn formats_records_with_the_date_only_if_not_today() {
        let now = Timestamp::from_second(86_400 + 3_600)
            .unwrap()
            .to_zoned(TimeZone::UTC);
        let today = record(Level::Warn, "iced_winit", "slow", 86_400 + 61);
        assert_eq!(
            format_record(&today, &now),
            "00:01:01 warning iced_winit: slow"
        );
        let yesterday = record(Level::Error, "fail", "it broke", 3_599);
        assert_eq!(
            format_record(&yesterday, &now),
            "1970-01-01 00:59:59 error fail: it broke"
        );
    }

    /// Runs `line`, answering its request for log records with `records`;
    /// returns what it prints or logs.
    fn run(line: &str, records: Vec<LogRecord>) -> Vec<String> {
        run_with(line, records, vec![], None)
    }

    /// Like [`run`], but also answers the request for running commands with
    /// `running`, and an island with `outcome` (shown as `<island>`).
    fn run_with(
        line: &str,
        records: Vec<LogRecord>,
        running: Vec<String>,
        outcome: Option<Outcome>,
    ) -> Vec<String> {
        let mut interpreter = Interpreter::new();
        register(&mut interpreter);
        let mut requests = interpreter.run(line).unwrap();
        let mut records = Some(records);
        let (mut running, mut outcome) = (Some(running), outcome);
        let mut output = Vec::new();
        let waker = std::task::Waker::noop();
        let mut cx = std::task::Context::from_waker(waker);
        loop {
            match std::pin::Pin::new(&mut requests).poll_next(&mut cx) {
                std::task::Poll::Ready(Some(request)) => match request {
                    Request::LogRecords(reply) => reply.send(records.take().unwrap()),
                    Request::RunningCommands(reply) => reply.send(running.take().unwrap()),
                    Request::Show(show) => {
                        output.push("<island>".to_owned());
                        show.into_parts().1.send(outcome.take().unwrap());
                    }
                    Request::Print(message) => output.push(message),
                    Request::Log(record) => output.push(record.to_string()),
                    Request::Clear => output.push("<clear>".to_owned()),
                    Request::Exit => output.push("<exit>".to_owned()),
                    _ => panic!("unexpected request"),
                },
                std::task::Poll::Ready(None) => return output,
                std::task::Poll::Pending => std::thread::sleep(Duration::from_millis(1)),
            }
        }
    }

    /// The message part of printed lines.
    fn messages(output: &[String]) -> Vec<&str> {
        let lines = output.iter().flat_map(|o| o.lines());
        lines.map(|l| l.split_once(": ").unwrap().1).collect()
    }

    #[test]
    fn lists_records_from_a_level_up() {
        let records = || {
            vec![
                record(Level::Debug, "a", "d", 0),
                record(Level::Info, "a", "i", 0),
                record(Level::Error, "b", "e", 0),
            ]
        };
        let output = run("messages", records());
        assert_eq!(output.len(), 1, "printed at once");
        assert_eq!(messages(&output), ["i", "e"]);
        assert_eq!(messages(&run("messages debug", records())), ["d", "i", "e"]);
        assert_eq!(messages(&run("messages error", records())), ["e"]);
        assert_eq!(run("messages", vec![]), ["No messages."]);
    }

    #[test]
    fn help_lists_commands() {
        assert_eq!(
            run("help", vec![]),
            ["! — Run a shell command: !COMMAND\n\
              clear — Clear the scrollback\n\
              exit — Exit hob\n\
              help — List the commands, or describe one\n\
              jobs — List the other running commands\n\
              messages — List log messages, from LEVEL up (info by default)"]
        );
        assert_eq!(
            run("help help", vec![]),
            ["help — List the commands, or describe one"]
        );
        assert_eq!(run("help nope", vec![]), [r#"error: no command "nope""#]);
    }

    #[test]
    fn clear_asks_to_clear() {
        assert_eq!(run("clear", vec![]), ["<clear>"]);
        assert_eq!(run("clear x", vec![]), ["error: usage: clear"]);
    }

    #[test]
    fn exit_asks_to_exit() {
        assert_eq!(run("exit", vec![]), ["<exit>"]);
        assert_eq!(run("exit now", vec![]), ["error: usage: exit"]);
    }

    #[test]
    fn jobs_lists_running_commands() {
        let running = vec!["count 9".to_owned(), "confirm".to_owned()];
        assert_eq!(
            run_with("jobs", vec![], running, None),
            ["count 9\nconfirm"]
        );
        assert_eq!(run("jobs", vec![]), ["No other commands are running."]);
        assert_eq!(run("jobs x", vec![]), ["error: usage: jobs"]);
    }

    #[test]
    fn exit_asks_first_if_commands_are_running() {
        let running = || vec!["count 9".to_owned()];
        let pressed = Outcome::Pressed("exit".into(), Default::default());
        assert_eq!(
            run_with("exit", vec![], running(), Some(pressed)),
            ["<island>", "<exit>"]
        );
        assert_eq!(
            run_with("exit", vec![], running(), Some(Outcome::Cancelled)),
            ["<island>", "Not exiting."]
        );
    }

    /// What a shell command prints or logs, as one string.
    fn shell(line: &str) -> String {
        run(line, vec![]).join("\n")
    }

    #[test]
    fn shell_commands_print_their_output() {
        assert_eq!(shell("!echo a; echo b"), "a\nb");
        assert_eq!(shell("! printf 'no newline'"), "no newline");
        assert_eq!(shell("!echo oops >&2"), "oops");
        assert_eq!(shell("!true"), "");
    }

    #[test]
    fn shell_commands_run_in_the_working_directory() {
        let mut interpreter = Interpreter::new();
        register(&mut interpreter);
        interpreter.set_cwd("/");
        let mut requests = interpreter.run("!pwd").unwrap();
        let waker = std::task::Waker::noop();
        let mut cx = std::task::Context::from_waker(waker);
        let message = loop {
            match std::pin::Pin::new(&mut requests).poll_next(&mut cx) {
                std::task::Poll::Ready(Some(Request::Print(message))) => break message,
                std::task::Poll::Ready(_) => panic!("expected Print"),
                std::task::Poll::Pending => std::thread::sleep(Duration::from_millis(1)),
            }
        };
        assert_eq!(message, "/");
    }

    #[test]
    fn shell_commands_get_an_empty_stdin() {
        assert_eq!(shell("!cat; echo done"), "done");
    }

    #[test]
    fn failing_shell_commands_are_errors() {
        assert_eq!(shell("!exit 3"), "error: exited with status 3");
        assert_eq!(shell("!kill -9 $$"), "error: was killed by a signal");
        assert_eq!(shell("!"), "error: usage: !COMMAND");
    }

    #[test]
    fn shell_output_is_limited() {
        let limit = SHELL_OUTPUT_LIMIT;
        // Lines are printed without their line breaks.
        let lines = |output: &[String]| output.iter().map(|p| p.len() + 1).sum::<usize>();
        let output = run(&format!("!yes | head -c {limit}"), vec![]);
        assert_eq!(lines(&output), limit, "all of it, without a warning");
        let output = run(&format!("!yes | head -c {}", 3 * limit), vec![]);
        let (warning, printed) = output.split_last().unwrap();
        assert_eq!(warning, "warning: output truncated at 1 MiB");
        assert_eq!(lines(printed), limit);
        let output = run(
            &format!("!yes | tr -d '\\n' | head -c {}", 3 * limit),
            vec![],
        );
        assert_eq!(output.len(), 2);
        assert_eq!(output[0].len(), limit, "one long line");
    }

    #[test]
    fn rejects_bad_arguments() {
        let output = run("messages loud", vec![]);
        assert!(
            output[0].starts_with("error: unknown log level"),
            "{output:?}"
        );
        assert_eq!(
            run("messages a b", vec![]),
            ["error: usage: messages [LEVEL]"]
        );
    }
}
