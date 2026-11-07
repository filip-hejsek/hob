//! Commands that work with any frontend. Register them with [`register`].
//!
//! - `clear`: clears the scrollback.
//! - `exit`: exits hob; if other commands are running, asks first, listing
//!   them.
//! - `help [COMMAND]`: lists all commands with their descriptions, or shows
//!   the description of one.
//! - `messages [LEVEL]`: lists the log records the frontend has, oldest
//!   first, from `LEVEL` up (`info` by default, so `messages debug` lists
//!   all).

use jiff::tz::TimeZone;
use jiff::{Timestamp, Zoned};

use kettle_ui::widget::{button_danger, cancel_button, column, label, row};
use kettle_ui::{IslandBuilder, Outcome};

use crate::{CommandResult, Context, Interpreter, Level, LogRecord};

/// Registers all built-in commands.
pub fn register(interpreter: &mut Interpreter) {
    interpreter.register("clear", "Clear the scrollback", clear);
    interpreter.register("exit", "Exit hob", exit);
    interpreter.register("help", "List the commands, or describe one", help);
    interpreter.register(
        "messages",
        "List log messages, from LEVEL up (info by default)",
        messages,
    );
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
            ["clear — Clear the scrollback\n\
              exit — Exit hob\n\
              help — List the commands, or describe one\n\
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
