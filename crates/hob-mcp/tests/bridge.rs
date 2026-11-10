//! Drives the bridge over MCP. Hob is this test program itself, started
//! with `--hob` (the headless frontend with test commands) or
//! `--hob-stuck` (a process that never replies nor exits).
//!
//! It has its own `main` (`harness = false`), so that it can act as hob.
//! Arguments that don't start with `-` filter the tests by name.

use std::ffi::OsString;
use std::panic;
use std::process::ExitCode;
use std::thread;
use std::time::Duration;

use hob_core::Interpreter;
use hob_mcp::{Bridge, Launch, Limits};
use kettle_ui::widget::*;
use kettle_ui::{IslandBuilder, Outcome};
use serde_json::{Value, json};

const TESTS: &[(&str, fn())] = &[
    ("initializes_and_lists_tools", initializes_and_lists_tools),
    (
        "runs_commands_and_acts_on_islands",
        runs_commands_and_acts_on_islands,
    ),
    (
        "fill_presses_the_submit_button",
        fill_presses_the_submit_button,
    ),
    ("waits_for_commands", waits_for_commands),
    ("shows_changed_islands", shows_changed_islands),
    ("sends_images", sends_images),
    ("reports_errors_to_the_agent", reports_errors_to_the_agent),
    ("agent_starts_and_closes_hob", agent_starts_and_closes_hob),
    ("exit_closes_hob", exit_closes_hob),
    ("without_restart", without_restart),
    ("reports_crashes", reports_crashes),
    ("kills_hob_that_is_stuck", kills_hob_that_is_stuck),
    #[cfg(unix)]
    ("notices_when_hob_is_killed", notices_when_hob_is_killed),
];

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--hob") => {
            let stdin = std::io::stdin();
            let _ = hob_headless::serve(interpreter(), stdin, std::io::stdout());
            return ExitCode::SUCCESS;
        }
        Some("--hob-stuck") => loop {
            thread::sleep(Duration::from_secs(1));
        },
        _ => {}
    }
    let filters: Vec<_> = args.iter().filter(|a| !a.starts_with('-')).collect();
    let mut failed = 0;
    for (name, test) in TESTS {
        if !filters.is_empty() && !filters.iter().any(|f| name.contains(f.as_str())) {
            continue;
        }
        let ok = panic::catch_unwind(test).is_ok();
        println!("test {name} ... {}", if ok { "ok" } else { "FAILED" });
        failed += usize::from(!ok);
    }
    match failed {
        0 => ExitCode::SUCCESS,
        _ => ExitCode::FAILURE,
    }
}

fn interpreter() -> Interpreter {
    let mut i = Interpreter::new();
    hob_core::builtins::register(&mut i);
    i.register("echo", "Print the arguments", |ctx, args| {
        ctx.print(args.join(" "));
        Ok(())
    });
    i.register("slow", "Print after a while", |ctx, _| {
        thread::sleep(Duration::from_millis(300));
        ctx.print("slept");
        Ok(())
    });
    i.register(
        "progress",
        "Show an island that changes, until OK",
        |ctx, _| {
            let mut builder = IslandBuilder::new(column([progress("done"), submit_button("OK")]));
            builder.set_state("done", 0.0);
            let handle = builder.handle();
            thread::spawn(move || {
                for i in 1..=10 {
                    thread::sleep(Duration::from_millis(150));
                    handle.set("done", f64::from(i) / 10.0);
                }
            });
            ctx.show(builder)?;
            Ok(())
        },
    );
    i.register("ticks", "Print 3 ticks, slowly", |ctx, _| {
        for i in 1..=3 {
            thread::sleep(Duration::from_millis(200));
            ctx.print(format!("tick {i}"));
        }
        Ok(())
    });
    i.register("crash", "Exit with status 3", |ctx, _| {
        // The frontend handles this request only after it has replied to
        // `run`, so the bridge knows the block when hob exits.
        let _ = ctx.log_records();
        eprintln!("crashing on purpose");
        std::process::exit(3);
    });
    i.register(
        "picture",
        "Show a picture, and a caption to fill in",
        |ctx, _| {
            let dot = kettle_ui::Image::from_rgba(1, 1, &[255, 0, 0, 255], "Red dot")?;
            let mut builder = IslandBuilder::new(column([
                image("dot"),
                text_field("caption", "Caption"),
                submit_button("OK"),
            ]));
            builder.set_state("dot", dot);
            ctx.show(builder)?;
            Ok(())
        },
    );
    i.register("ask", "Ask for a name", |ctx, _| {
        let mut builder = IslandBuilder::new(column([
            text_field("name", "Name"),
            checkbox("loud", "Loud"),
            label(state("greeting")),
            submit_button("OK"),
        ]));
        builder.derive("greeting", |f| format!("Hi, {}!", f.text("name")).into());
        if let Outcome::Submitted(fields) = ctx.show(builder)? {
            ctx.print(format!(
                "got {} {}",
                fields.text("name"),
                fields.checked("loud")
            ));
        }
        Ok(())
    });
    i.register(
        "choose",
        "Ask for a choice, without a submit button",
        |ctx, _| {
            let builder = IslandBuilder::new(row([button("a", "A"), cancel_button("Cancel")]));
            if let Outcome::Pressed(id, _) = ctx.show(builder)? {
                ctx.print(format!("chose {id}"));
            }
            Ok(())
        },
    );
    i
}

/// The command that starts this program as hob, in `mode` (`--hob` or
/// `--hob-stuck`).
fn hob_command(mode: &str) -> Vec<String> {
    let exe = std::env::current_exe().unwrap();
    vec![exe.to_str().unwrap().to_owned(), mode.to_owned()]
}

fn limits() -> Limits {
    Limits {
        call: Duration::from_millis(500),
        grace: Duration::from_millis(500),
    }
}

/// A bridge with a fixed command that may be restarted, and hob started.
fn bridge() -> Bridge {
    fixed_bridge(true)
}

/// A bridge with a fixed command, and hob started.
fn fixed_bridge(restart: bool) -> Bridge {
    let command = hob_command("--hob").into_iter().map(OsString::from);
    let launch = Launch::Command {
        command: command.collect(),
        restart,
    };
    let mut bridge = Bridge::new(launch, limits());
    let report = bridge.start(None).unwrap();
    assert!(report.contains("hob started (PID "), "{report}");
    bridge
}

/// Sends a request; returns the reply.
fn request(bridge: &mut Bridge, method: &str, params: Value) -> Value {
    let message = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    let reply = bridge.handle_message(&message.to_string()).unwrap();
    serde_json::from_str(&reply).unwrap()
}

/// Calls a tool; returns its text, and whether it's an error.
fn tool(bridge: &mut Bridge, name: &str, arguments: Value) -> (String, bool) {
    let result = tool_result(bridge, name, arguments);
    let text = result["content"][0]["text"].as_str().unwrap().to_owned();
    (text, result["isError"].as_bool().unwrap())
}

/// Calls a tool; returns its result.
fn tool_result(bridge: &mut Bridge, name: &str, arguments: Value) -> Value {
    let reply = request(
        bridge,
        "tools/call",
        json!({"name": name, "arguments": arguments}),
    );
    reply["result"].clone()
}

/// The PID in "hob started (PID 123)".
fn pid(report: &str) -> u32 {
    let (_, rest) = report.split_once("hob started (PID ").unwrap();
    rest.split(')').next().unwrap().parse().unwrap()
}

fn initializes_and_lists_tools() {
    let mut bridge = bridge();
    let params = json!({"protocolVersion": "2025-06-18", "capabilities": {}});
    let reply = request(&mut bridge, "initialize", params);
    assert_eq!(reply["result"]["protocolVersion"], "2025-06-18");
    assert!(reply["result"]["capabilities"]["tools"].is_object());

    let reply = request(&mut bridge, "tools/list", json!({}));
    let tools = reply["result"]["tools"].as_array().unwrap();
    let names: Vec<_> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        ["run", "fill", "press", "state", "wait", "start", "close"]
    );
    assert_eq!(tools[5]["inputSchema"]["properties"], json!({}));

    // Notifications get no reply.
    let notification = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
    assert_eq!(bridge.handle_message(notification), None);
    let reply = request(&mut bridge, "nope", json!({}));
    assert_eq!(reply["error"]["code"], -32601);

    // When the agent starts hob, it gives the command.
    let mut bridge = Bridge::new(Launch::ByAgent, limits());
    let reply = request(&mut bridge, "tools/list", json!({}));
    let start = &reply["result"]["tools"][5];
    assert_eq!(start["inputSchema"]["required"], json!(["command"]));
    let reply = request(&mut bridge, "initialize", json!({}));
    let instructions = reply["result"]["instructions"].as_str().unwrap();
    assert!(instructions.contains("Hob isn't running at first"));
}

fn runs_commands_and_acts_on_islands() {
    let mut bridge = bridge();
    let (text, error) = tool(&mut bridge, "run", json!({"line": "echo hi"}));
    assert!(!error);
    assert_eq!(
        text,
        "What happened:\n  [1] hi\n  [1] finished\n[1] has finished.\n"
    );

    let (text, _) = tool(&mut bridge, "run", json!({"line": "ask"}));
    assert!(text.contains("[2] showed island #3"), "{text}");
    assert!(text.contains(r#"label "Hi, !""#), "{text}");
    assert!(text.ends_with("[2] is waiting for island #3.\n"), "{text}");

    let fields = json!({"name": "Ada", "loud": true});
    let (text, error) = tool(&mut bridge, "fill", json!({"island": 3, "fields": fields}));
    assert!(!error, "{text}");
    assert!(text.contains(r#"label "Hi, Ada!""#), "{text}");
    assert!(text.contains(r#"checkbox loud: [x]"#), "{text}");

    let (text, _) = tool(&mut bridge, "press", json!({"island": 3, "label": "OK"}));
    assert!(text.contains("[2] got Ada true"), "{text}");
    assert!(text.ends_with("[2] has finished.\n"), "{text}");
}

fn fill_presses_the_submit_button() {
    let mut bridge = bridge();
    let (text, _) = tool(&mut bridge, "run", json!({"line": "ask"}));
    assert!(text.contains(r#"button "OK" (submit)"#), "{text}");
    let arguments = json!({"island": 2, "fields": {"name": "Ada"}, "submit": true});
    let (text, error) = tool(&mut bridge, "fill", arguments);
    assert!(!error, "{text}");
    assert!(text.contains("[1] got Ada false"), "{text}");
    assert!(text.ends_with("[1] has finished.\n"), "{text}");

    // Without a submit button, it's an error; the fields are filled anyway.
    let (text, _) = tool(&mut bridge, "run", json!({"line": "choose"}));
    assert!(text.contains("[3] showed island #4"), "{text}");
    let arguments = json!({"island": 4, "fields": {}, "submit": true});
    let (text, error) = tool(&mut bridge, "fill", arguments);
    assert!(error, "{text}");
    assert!(text.contains("island #4 has no submit button"), "{text}");
    let (text, _) = tool(&mut bridge, "press", json!({"island": 4, "label": "A"}));
    assert!(text.contains("[3] chose a"), "{text}");
}

fn waits_for_commands() {
    let mut bridge = bridge();
    let (text, _) = tool(
        &mut bridge,
        "run",
        json!({"line": "slow", "timeout_seconds": 0}),
    );
    assert!(text.contains("is still running"), "{text}");
    let (text, _) = tool(&mut bridge, "state", json!({}));
    assert!(text.contains("hob is running (PID "), "{text}");
    assert!(text.contains("Running commands:\n  [1] slow"), "{text}");
    let arguments = json!({"block": 1, "until": "finished"});
    let (text, error) = tool(&mut bridge, "wait", arguments);
    assert!(!error);
    assert!(text.contains("[1] slept\n  [1] finished"), "{text}");
    assert!(text.ends_with("[1] has finished.\n"), "{text}");

    let (text, _) = tool(&mut bridge, "wait", json!({"timeout_seconds": 0.05}));
    assert!(text.contains("Nothing to wait for came"), "{text}");
}

/// A changed island is shown in results about its command, not in others.
fn shows_changed_islands() {
    let mut bridge = bridge();
    let (text, _) = tool(&mut bridge, "run", json!({"line": "progress"}));
    assert!(text.contains("Island #2 of [1], live:"), "{text}");
    // It changes while [3] prints; the changes are listed once.
    let (text, _) = tool(&mut bridge, "run", json!({"line": "ticks"}));
    assert!(
        text.contains("[3] tick 1\n  [3] tick 2\n  [3] tick 3"),
        "{text}"
    );
    assert_eq!(text.matches("[1] island #2 changed").count(), 1, "{text}");
    assert!(!text.contains("Island #2"), "{text}");
    thread::sleep(Duration::from_millis(1600));
    // Still to be shown, now by a result about [1].
    let (text, _) = tool(&mut bridge, "fill", json!({"island": 2, "fields": {}}));
    assert!(text.contains("Island #2 of [1], live:"), "{text}");
    assert!(text.contains("progress 100%"), "{text}");
}

fn reports_errors_to_the_agent() {
    let mut bridge = bridge();
    let (text, error) = tool(&mut bridge, "run", json!({"line": "nope"}));
    assert!(error && text.contains("unknown command"), "{text}");
    let (text, error) = tool(&mut bridge, "press", json!({"island": 9, "label": "OK"}));
    assert!(error && text.contains("no live island 9"), "{text}");
    let (text, error) = tool(&mut bridge, "run", json!({"lines": "echo"}));
    assert!(error && text.contains("bad arguments"), "{text}");
    let (text, error) = tool(
        &mut bridge,
        "fill",
        json!({"island": 1, "fields": {"a": 1}}),
    );
    assert!(error && text.contains("must be a string"), "{text}");
    // The command is fixed.
    let (text, error) = tool(&mut bridge, "start", json!({"command": ["sh"]}));
    assert!(error && text.contains("fixed command"), "{text}");

    let reply = request(&mut bridge, "tools/call", json!({"name": "fly"}));
    assert_eq!(reply["error"]["code"], -32602);
}

fn agent_starts_and_closes_hob() {
    let mut bridge = Bridge::new(Launch::ByAgent, limits());
    let (text, error) = tool(&mut bridge, "run", json!({"line": "echo hi"}));
    assert!(error && text.contains("hob isn't running"), "{text}");
    let (text, _) = tool(&mut bridge, "state", json!({}));
    assert_eq!(text, "hob isn't running.\n");
    let (text, error) = tool(&mut bridge, "wait", json!({}));
    assert!(error && text.contains("hob isn't running"), "{text}");
    let (text, error) = tool(&mut bridge, "start", json!({}));
    assert!(error && text.contains("give the command"), "{text}");
    let (text, error) = tool(&mut bridge, "start", json!({"command": ["/no/such/hob"]}));
    assert!(
        error && text.contains("couldn't start /no/such/hob"),
        "{text}"
    );
    let (text, error) = tool(&mut bridge, "start", json!({"command": ["false"]}));
    assert!(
        error && text.contains("exited unexpectedly (exit status: 1)"),
        "{text}"
    );

    let command = hob_command("--hob");
    let (text, error) = tool(&mut bridge, "start", json!({"command": command}));
    assert!(!error, "{text}");
    let first = pid(&text);
    let (text, error) = tool(&mut bridge, "start", json!({"command": command}));
    assert!(error && text.contains("already running"), "{text}");
    let (text, _) = tool(&mut bridge, "run", json!({"line": "ask"}));
    assert!(text.contains("[1] showed island #2"), "{text}");

    // Closing interrupts the command waiting for its island.
    let (text, error) = tool(&mut bridge, "close", json!({}));
    assert!(!error, "{text}");
    assert!(text.contains("[1] was interrupted"), "{text}");
    let exited = format!("hob (PID {first}) exited (exit status: 0)");
    assert!(text.contains(&exited), "{text}");
    let (text, error) = tool(&mut bridge, "close", json!({}));
    assert!(error && text.contains("hob isn't running"), "{text}");
    let (text, error) = tool(&mut bridge, "fill", json!({"island": 2, "fields": {}}));
    assert!(error && text.contains("hob isn't running"), "{text}");

    // A new hob starts over.
    let (text, _) = tool(&mut bridge, "start", json!({"command": command}));
    assert_ne!(pid(&text), first);
    let (text, _) = tool(&mut bridge, "run", json!({"line": "echo again"}));
    assert!(text.contains("[1] again"), "{text}");
}

fn exit_closes_hob() {
    let mut bridge = bridge();
    let (text, _) = tool(&mut bridge, "run", json!({"line": "exit"}));
    assert!(text.contains("[1] asked hob to exit"), "{text}");
    assert!(text.contains("exited (exit status: 0)"), "{text}");
    assert!(!text.contains("interrupted"), "{text}");
    assert!(text.ends_with("[1] has finished.\n"), "{text}");
    let (text, error) = tool(&mut bridge, "run", json!({"line": "echo"}));
    assert!(error && text.contains("hob isn't running"), "{text}");
    // It starts again with the fixed command.
    let (text, error) = tool(&mut bridge, "start", json!({}));
    assert!(!error && text.contains("hob started"), "{text}");
}

fn reports_crashes() {
    let mut bridge = bridge();
    let (text, _) = tool(
        &mut bridge,
        "run",
        json!({"line": "slow", "timeout_seconds": 0}),
    );
    assert!(text.contains("still running"), "{text}");
    let (text, _) = tool(&mut bridge, "run", json!({"line": "crash"}));
    assert!(text.contains("[1] was interrupted"), "{text}");
    assert!(text.contains("[2] was interrupted"), "{text}");
    assert!(
        text.contains("exited unexpectedly (exit status: 3)"),
        "{text}"
    );
    assert!(text.contains("\n    crashing on purpose"), "{text}");
    assert!(text.ends_with("[2] was interrupted.\n"), "{text}");
    let (text, _) = tool(&mut bridge, "state", json!({}));
    assert_eq!(text, "hob isn't running.\n");
    let (text, error) = tool(&mut bridge, "start", json!({}));
    assert!(!error && text.contains("hob started"), "{text}");
}

fn kills_hob_that_is_stuck() {
    let mut bridge = Bridge::new(Launch::ByAgent, limits());
    let command = hob_command("--hob-stuck");
    let (text, _) = tool(&mut bridge, "start", json!({"command": command}));
    let pid = pid(&text);
    let (text, error) = tool(&mut bridge, "run", json!({"line": "echo"}));
    assert!(
        error && text.contains("didn't reply within 0.5 s"),
        "{text}"
    );
    let (text, error) = tool(&mut bridge, "close", json!({}));
    assert!(!error, "{text}");
    let killed = format!("hob (PID {pid}) didn't exit within 0.5 s, so it was killed");
    assert!(text.contains(&killed), "{text}");
}

#[cfg(unix)]
fn notices_when_hob_is_killed() {
    let mut bridge = bridge();
    let (text, _) = tool(&mut bridge, "state", json!({}));
    let pid = text
        .split("(PID ")
        .nth(1)
        .unwrap()
        .split(')')
        .next()
        .unwrap();
    let status = std::process::Command::new("kill")
        .args(["-KILL", pid])
        .status();
    assert!(status.unwrap().success());
    let (text, _) = tool(&mut bridge, "wait", json!({}));
    assert!(text.contains("exited unexpectedly (signal: 9"), "{text}");
    let (text, error) = tool(&mut bridge, "start", json!({}));
    assert!(!error && text.contains("hob started"), "{text}");
}

fn without_restart() {
    let mut bridge = fixed_bridge(false);
    let reply = request(&mut bridge, "tools/list", json!({}));
    let tools = reply["result"]["tools"].as_array().unwrap();
    let names: Vec<_> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["run", "fill", "press", "state", "wait"]);
    let reply = request(&mut bridge, "initialize", json!({}));
    let instructions = reply["result"]["instructions"].as_str().unwrap();
    assert!(!instructions.contains("start starts it"), "{instructions}");
    for name in ["start", "close"] {
        let reply = request(&mut bridge, "tools/call", json!({"name": name}));
        assert_eq!(reply["error"]["code"], -32602);
    }

    // Exit does nothing.
    let (text, _) = tool(&mut bridge, "run", json!({"line": "exit"}));
    assert!(
        text.contains("[1] asked to exit, which does nothing here"),
        "{text}"
    );
    let (text, _) = tool(&mut bridge, "run", json!({"line": "echo still here"}));
    assert!(text.contains("[2] still here"), "{text}");

    // After a crash, the server reports it, then ends.
    let call = |id: u32, line: &str| {
        let arguments = json!({"line": line});
        let params = json!({"name": "run", "arguments": arguments});
        json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": params})
    };
    let input = format!("{}\n{}\n", call(1, "crash"), call(2, "echo"));
    let mut output = Vec::new();
    hob_mcp::mcp::serve(&mut bridge, input.as_bytes(), &mut output).unwrap();
    assert!(bridge.is_over());
    let output = String::from_utf8(output).unwrap();
    let replies: Vec<Value> = output
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(replies.len(), 1, "{output}");
    let text = replies[0]["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("exited unexpectedly (exit status: 3)"),
        "{text}"
    );
    assert!(
        text.contains("hob-mcp exits, since it can't start hob again"),
        "{text}"
    );
}

fn sends_images() {
    let mut bridge = bridge();
    let result = tool_result(&mut bridge, "run", json!({"line": "picture"}));
    let content = result["content"].as_array().unwrap();
    let text = content[0]["text"].as_str().unwrap();
    assert!(text.contains("image \"Red dot\" (1x1)"), "{text}");
    assert_eq!(content[1]["text"], "Island #2, image \"Red dot\" (1x1):");
    assert_eq!(content[2]["type"], "image");
    assert_eq!(content[2]["mimeType"], "image/png");
    let data = content[2]["data"].as_str().unwrap();
    assert!(data.starts_with("iVBORw0KGgo"), "not a PNG: {data}");
    assert_eq!(content.len(), 3);

    // Changing the island doesn't send the image again; state does.
    let fields = json!({"island": 2, "fields": {"caption": "A dot"}});
    let result = tool_result(&mut bridge, "fill", fields);
    assert_eq!(result["content"].as_array().unwrap().len(), 1, "{result}");
    let result = tool_result(&mut bridge, "state", json!({}));
    let content = result["content"].as_array().unwrap();
    assert_eq!(content.len(), 3, "{result}");
    assert_eq!(content[2]["mimeType"], "image/png");
}
