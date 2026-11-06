//! Drives the bridge over MCP, with the headless frontend in this process.

use std::io;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use hob_core::Interpreter;
use hob_headless::client::Client;
use hob_mcp::Bridge;
use kettle_ui::widget::*;
use kettle_ui::{IslandBuilder, Outcome};
use serde_json::{Value, json};

fn interpreter() -> Interpreter {
    let mut i = Interpreter::new();
    i.register("echo", "Print the arguments", |ctx, args| {
        ctx.print(args.join(" "));
        Ok(())
    });
    i.register("slow", "Print after a while", |ctx, _| {
        thread::sleep(Duration::from_millis(300));
        ctx.print("slept");
        Ok(())
    });
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
    i
}

fn bridge() -> Bridge {
    let (input_reader, input) = io::pipe().unwrap();
    let (output, output_writer) = io::pipe().unwrap();
    thread::spawn(move || hob_headless::serve(interpreter(), input_reader, output_writer));
    let (sender, notifications) = mpsc::channel();
    let client = Client::new(input, output, move |n| {
        let _ = sender.send(n);
    });
    Bridge::new(client, notifications)
}

/// Sends a request; returns the reply.
fn request(bridge: &mut Bridge, method: &str, params: Value) -> Value {
    let message = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    let reply = bridge.handle_message(&message.to_string()).unwrap();
    serde_json::from_str(&reply).unwrap()
}

/// Calls a tool; returns its text, and whether it's an error.
fn tool(bridge: &mut Bridge, name: &str, arguments: Value) -> (String, bool) {
    let reply = request(
        bridge,
        "tools/call",
        json!({"name": name, "arguments": arguments}),
    );
    let result = &reply["result"];
    let text = result["content"][0]["text"].as_str().unwrap().to_owned();
    (text, result["isError"].as_bool().unwrap())
}

#[test]
fn initializes_and_lists_tools() {
    let mut bridge = bridge();
    let params = json!({"protocolVersion": "2025-06-18", "capabilities": {}});
    let reply = request(&mut bridge, "initialize", params);
    assert_eq!(reply["result"]["protocolVersion"], "2025-06-18");
    assert!(reply["result"]["capabilities"]["tools"].is_object());

    let reply = request(&mut bridge, "tools/list", json!({}));
    let names: Vec<_> = (reply["result"]["tools"].as_array().unwrap().iter())
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["run", "fill", "press", "state", "wait"]);

    // Notifications get no reply.
    let notification = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
    assert_eq!(bridge.handle_message(notification), None);
    let reply = request(&mut bridge, "nope", json!({}));
    assert_eq!(reply["error"]["code"], -32601);
}

#[test]
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

#[test]
fn waits_for_commands() {
    let mut bridge = bridge();
    let (text, _) = tool(
        &mut bridge,
        "run",
        json!({"line": "slow", "timeout_seconds": 0}),
    );
    assert!(text.contains("is still running"), "{text}");
    let (text, _) = tool(&mut bridge, "state", json!({}));
    assert!(text.contains("Running commands:\n  [1] slow"), "{text}");
    let arguments = json!({"block": 1, "until": "finished"});
    let (text, error) = tool(&mut bridge, "wait", arguments);
    assert!(!error);
    assert!(text.contains("[1] slept\n  [1] finished"), "{text}");

    let (text, _) = tool(&mut bridge, "wait", json!({"timeout_seconds": 0.05}));
    assert!(text.contains("Nothing to wait for came"), "{text}");
}

#[test]
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

    let reply = request(&mut bridge, "tools/call", json!({"name": "fly"}));
    assert_eq!(reply["error"]["code"], -32602);
}
