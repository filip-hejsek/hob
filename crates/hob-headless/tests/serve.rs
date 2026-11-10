//! Drives the headless frontend over pipes, like a client would.

use std::collections::VecDeque;
use std::io::{self, BufRead, BufReader, PipeReader, PipeWriter, Write};
use std::thread;
use std::time::Duration;

use hob_core::Interpreter;
use hob_headless::protocol::{self, Call, ErrorObject, Incoming, Notification, code};
use kettle_ui::widget::*;
use kettle_ui::{IslandBuilder, Outcome};
use kettle_ui_headless::outline;
use serde_json::{Value, json};

struct Client {
    input: PipeWriter,
    output: BufReader<PipeReader>,
    next_id: u64,
    /// Notifications that arrived while waiting for a reply.
    pending: VecDeque<Notification>,
}

impl Client {
    fn start(interpreter: Interpreter) -> Client {
        Client::start_with(interpreter, hob_headless::Options::default())
    }

    fn start_with(interpreter: Interpreter, options: hob_headless::Options) -> Client {
        let (input_reader, input) = io::pipe().unwrap();
        let (output, output_writer) = io::pipe().unwrap();
        thread::spawn(move || {
            hob_headless::serve_with(interpreter, input_reader, output_writer, options)
        });
        Client {
            input,
            output: BufReader::new(output),
            next_id: 0,
            pending: VecDeque::new(),
        }
    }

    fn read(&mut self) -> Incoming {
        let mut line = String::new();
        self.output.read_line(&mut line).unwrap();
        protocol::parse_incoming(&line).unwrap()
    }

    fn send_line(&mut self, line: &str) {
        writeln!(self.input, "{line}").unwrap();
    }

    /// Waits for the reply to a request line with this id.
    fn reply(&mut self, expected: Value) -> Result<Value, ErrorObject> {
        loop {
            match self.read() {
                Incoming::Response { id, result } => {
                    assert_eq!(id, expected);
                    return result;
                }
                Incoming::Notification(n) => self.pending.push_back(n),
            }
        }
    }

    fn call(&mut self, call: Call) -> Result<Value, ErrorObject> {
        self.next_id += 1;
        let line = protocol::request(self.next_id, &call);
        self.send_line(&line);
        self.reply(self.next_id.into())
    }

    fn run(&mut self, line: &str) -> u64 {
        let result = self.call(Call::Run { line: line.into() }).unwrap();
        result["block"].as_u64().unwrap()
    }

    /// The next notification, described as text.
    fn next(&mut self) -> String {
        let notification = match self.pending.pop_front() {
            Some(n) => n,
            None => match self.read() {
                Incoming::Notification(n) => n,
                Incoming::Response { .. } => panic!("unexpected response"),
            },
        };
        match notification {
            Notification::Printed { block, text } => format!("{block} printed {text}"),
            Notification::Logged { block, record } => match block {
                Some(block) => format!("{block} logged {record}"),
                None => format!("notice {record}"),
            },
            Notification::Island {
                block,
                island,
                live,
                contents,
            } => {
                let live = if live { "live" } else { "frozen" };
                format!("{block} island {island} {live}\n{}", outline(&contents))
            }
            Notification::Cleared { block } => format!("{block} cleared"),
            Notification::ExitRequested { block } => format!("{block} exit requested"),
            Notification::Finished { block } => format!("{block} finished"),
        }
    }
}

fn interpreter() -> Interpreter {
    let mut i = Interpreter::new();
    hob_core::builtins::register(&mut i);
    i.register("echo", "Print the arguments", |ctx, args| {
        ctx.print(args.join(" "));
        Ok(())
    });
    i.register("warn", "Log a warning", |ctx, args| {
        ctx.warn(args.join(" "));
        Ok(())
    });
    i.register("ask", "Ask for a name", |ctx, _| {
        let mut builder = IslandBuilder::new(column([
            text_field("name", "Name"),
            label(state("greeting")),
            label(state("error")),
            submit_button("OK"),
            cancel_button("Cancel"),
        ]));
        builder.derive("greeting", |f| format!("Hi, {}!", f.text("name")).into());
        builder.validate("error", |f| {
            f.text("name").is_empty().then(|| "Enter a name".into())
        });
        match ctx.show(builder)? {
            Outcome::Submitted(fields) => ctx.print(format!("got {}", fields.text("name"))),
            Outcome::Cancelled | Outcome::Pressed(..) => ctx.print("cancelled"),
        }
        Ok(())
    });
    i.register("picture", "Print an image file", |ctx, args| {
        let picture = kettle_ui::Image::from_file(&args[0], "pic")?;
        let mut builder = IslandBuilder::new(image("pic"));
        builder.set_state("pic", picture);
        ctx.print_island(builder.freeze()?);
        Ok(())
    });
    i.register("tick", "Update state from another thread", |ctx, _| {
        let mut builder = IslandBuilder::new(column([label(state("s")), cancel_button("Stop")]));
        builder.set_state("s", "0");
        let handle = builder.handle();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            handle.set("s", "1");
        });
        ctx.show(builder)?;
        Ok(())
    });
    i
}

#[test]
fn runs_commands() {
    let mut client = Client::start(interpreter());
    let block = client.run("echo hello  world");
    assert_eq!(client.next(), format!("{block} printed hello world"));
    assert_eq!(client.next(), format!("{block} finished"));
}

#[test]
fn islands_are_live_until_submitted() {
    let mut client = Client::start(interpreter());
    let block = client.run("ask");
    let shown = client.next();
    assert!(shown.starts_with(&format!("{block} island ")), "{shown}");
    assert!(shown.contains(r#"label "Hi, !""#), "{shown}");
    let island = shown.split(' ').nth(2).unwrap().parse().unwrap();
    assert!(shown.contains("label \"\"\n"), "{shown}");

    // Not submitted while empty; the island shows why.
    let error = client
        .call(Call::Press {
            island,
            label: "OK".into(),
        })
        .unwrap_err();
    assert_eq!(error.code, code::ACTION_FAILED);
    assert_eq!(error.message, "not submitted: Enter a name");
    let changed = client.next();
    assert!(
        changed.contains(&format!("island {island} live")),
        "{changed}"
    );
    assert!(changed.contains(r#"label "Enter a name""#), "{changed}");

    let typed = client.call(Call::TypeText {
        island,
        field: "name".into(),
        text: "Ada".into(),
    });
    assert_eq!(typed, Ok(Value::Null));
    let changed = client.next();
    assert!(
        changed.contains(&format!("island {island} live")),
        "{changed}"
    );
    assert!(changed.contains(r#"label "Hi, Ada!""#), "{changed}");

    let pressed = client.call(Call::Press {
        island,
        label: "OK".into(),
    });
    assert_eq!(pressed, Ok(Value::Null));
    assert!(client.next().contains(&format!("island {island} frozen")));
    assert_eq!(client.next(), format!("{block} printed got Ada"));
    assert_eq!(client.next(), format!("{block} finished"));

    // It's frozen now.
    let error = client.call(Call::Press {
        island,
        label: "OK".into(),
    });
    assert_eq!(error.unwrap_err().code, code::NO_LIVE_ISLAND);
}

#[test]
fn state_updates_from_other_threads_are_sent() {
    let mut client = Client::start(interpreter());
    client.run("tick");
    let shown = client.next();
    assert!(shown.contains(r#"label "0""#), "{shown}");
    let island = shown.split(' ').nth(2).unwrap().parse().unwrap();
    let updated = client.next();
    assert!(updated.contains(r#"label "1""#), "{updated}");
    client
        .call(Call::Press {
            island,
            label: "Stop".into(),
        })
        .unwrap();
}

#[test]
fn errors_are_replied() {
    let mut client = Client::start(interpreter());
    let error = client
        .call(Call::Run {
            line: "nope".into(),
        })
        .unwrap_err();
    assert_eq!(error.code, code::NOT_STARTED);
    assert!(error.message.contains("unknown command"), "{error}");

    let block = client.run("ask");
    let island = client.next().split(' ').nth(2).unwrap().parse().unwrap();
    let error = client
        .call(Call::Press {
            island,
            label: "Nope".into(),
        })
        .unwrap_err();
    assert_eq!(error.code, code::ACTION_FAILED);
    client
        .call(Call::Press {
            island,
            label: "Cancel".into(),
        })
        .unwrap();
    client.next();
    assert_eq!(client.next(), format!("{block} printed cancelled"));

    client.send_line("not json");
    assert_eq!(
        client.reply(Value::Null).unwrap_err().code,
        code::PARSE_ERROR
    );
    client.send_line(&json!({"jsonrpc": "2.0", "id": 9, "method": "fly"}).to_string());
    let error = client.reply(9.into()).unwrap_err();
    assert_eq!(error.code, code::METHOD_NOT_FOUND);
}

#[test]
fn commands_can_read_log_records() {
    let mut client = Client::start(interpreter());
    let block = client.run("warn careful");
    assert_eq!(client.next(), format!("{block} logged warning: careful"));
    client.next();
    let block = client.run("messages");
    let listed = client.next();
    assert!(
        listed.starts_with(&format!("{block} printed "))
            && listed.ends_with("warning warn: careful"),
        "{listed}"
    );
}

#[test]
fn clear_is_passed_on() {
    let mut client = Client::start(interpreter());
    let block = client.run("clear");
    assert_eq!(client.next(), format!("{block} cleared"));
    assert_eq!(client.next(), format!("{block} finished"));
}

#[test]
fn exit_is_passed_on() {
    let mut client = Client::start(interpreter());
    let block = client.run("exit");
    assert_eq!(client.next(), format!("{block} exit requested"));
    assert_eq!(client.next(), format!("{block} finished"));
}

#[test]
fn exit_asks_first_if_commands_are_running() {
    let mut client = Client::start(interpreter());
    let ask = client.run("ask");
    assert!(client.next().starts_with(&format!("{ask} island")));
    let exit = client.run("exit");
    let shown = client.next();
    let prefix = format!("{exit} island ");
    let island: u64 = shown
        .strip_prefix(&prefix)
        .unwrap()
        .split(' ')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    assert!(shown.contains("ask"), "{shown}");
    let press = Call::Press {
        island,
        label: "Yes".into(),
    };
    client.call(press).unwrap();
    assert!(client.next().contains("frozen"));
    assert_eq!(client.next(), format!("{exit} exit requested"));
    assert_eq!(client.next(), format!("{exit} finished"));
}

#[test]
fn image_paths_are_sent_only_if_asked() {
    let dir = std::env::temp_dir().join(format!("hob-headless-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("pic.img");
    let red = kettle_ui::Image::from_rgba(1, 1, &[255, 0, 0, 255], "").unwrap();
    std::fs::write(&file, red.data().unwrap().bytes()).unwrap();
    let line = format!("picture {}", file.display());

    let mut client = Client::start(interpreter());
    client.run(&line);
    assert_eq!(client.next(), "1 island 2 frozen\nimage \"pic\" (1x1)\n");

    let mut options = hob_headless::Options::default();
    options.image_paths = true;
    let mut client = Client::start_with(interpreter(), options);
    client.run(&line);
    assert_eq!(
        client.next(),
        format!(
            "1 island 2 frozen\nimage \"pic\" (1x1, {})\n",
            file.display()
        )
    );
    std::fs::remove_dir_all(&dir).unwrap();
}
