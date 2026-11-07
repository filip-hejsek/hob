//! Runs the demo commands, answering their islands headlessly.

use futures_executor::{BlockingStream, block_on_stream};
use hob_core::{Interpreter, Request, Requests};
use kettle_ui::Outcome;
use kettle_ui_headless::{Island, outline};

fn run(line: &str) -> BlockingStream<Requests> {
    let mut interpreter = Interpreter::new();
    hob_demo::register(&mut interpreter);
    block_on_stream(interpreter.run(line).unwrap())
}

/// Everything the command prints until it finishes; islands as outlines.
fn rest(requests: BlockingStream<Requests>) -> Vec<String> {
    requests
        .map(|request| match request {
            Request::Print(message) => message,
            Request::Log(record) => record.to_string(),
            Request::PrintIsland(island) => outline(&island),
            Request::Show(_) => panic!("unexpected Show"),
            Request::LogRecords(_) => panic!("unexpected LogRecords"),
            Request::Clear => panic!("unexpected Clear"),
        })
        .collect()
}

/// Waits for the command to show an island, and answers it with `answer`.
fn answer(requests: &mut BlockingStream<Requests>, answer: impl FnOnce(&mut Island) -> Outcome) {
    let Some(Request::Show(show)) = requests.next() else {
        panic!("expected Show");
    };
    let (builder, reply) = show.into_parts();
    match Island::new(builder) {
        Ok(mut island) => reply.send(answer(&mut island)),
        Err(e) => reply.fail(e),
    }
}

#[test]
fn echo() {
    assert_eq!(rest(run("echo a  b")), ["a b"]);
}

#[test]
fn count() {
    assert_eq!(rest(run("count 3")), ["1", "2", "3"]);
    assert_eq!(rest(run("count x")), [r#"error: not a number: "x""#]);
    assert_eq!(rest(run("count 1 2")), ["error: usage: count [N]"]);
}

#[test]
fn errors() {
    assert_eq!(rest(run("fail oh no")), ["About to fail…", "error: oh no"]);
    assert_eq!(rest(run("panic")), ["error: panicked: the demo panicked"]);
    assert_eq!(rest(run("contact x")), ["error: takes no arguments"]);
}

#[test]
fn levels() {
    assert_eq!(
        rest(run("levels")),
        [
            "error: an error",
            "warning: a warning",
            "info: some information",
            "debug: a debug message (hidden unless HOB_LOG=debug)"
        ]
    );
}

#[test]
fn invalid() {
    let mut requests = run("invalid");
    answer(&mut requests, |_| unreachable!());
    let printed = rest(requests);
    assert_eq!(printed.len(), 1);
    assert!(
        printed[0].starts_with("error: invalid island: "),
        "{printed:?}"
    );
}

#[test]
fn pizza() {
    let mut requests = run("pizza Ada Lovelace");
    answer(&mut requests, |island| {
        island.set_checked("cheese", true).unwrap();
        island.set_checked("ham", true).unwrap();
        assert!(island.to_string().contains(r#"label "Price: €11.00""#));
        island.press("Next").unwrap()
    });
    // The address is missing, so it asks again, keeping the name.
    answer(&mut requests, |island| island.press("Order").unwrap());
    assert_eq!(
        requests.next().map(|r| match r {
            Request::Print(m) => m,
            _ => panic!("expected Print"),
        }),
        Some("Please fill in both your name and address.".to_owned())
    );
    answer(&mut requests, |island| {
        assert_eq!(island.fields().text("name"), "Ada Lovelace");
        island.type_text("address", "12 St James's Square").unwrap();
        island.press("Order").unwrap()
    });
    assert_eq!(
        rest(requests),
        [r#"column
  label "Receipt for Ada Lovelace"
  list
    - "Pizza  €8.00"
    - "Extra cheese  €1.00"
    - "Ham  €2.00"
  label "Total: €11.00"
  label "Delivering to 12 St James's Square"
"#]
    );
}

#[test]
fn pizza_can_be_cancelled() {
    let mut requests = run("pizza");
    answer(&mut requests, |island| island.press("Next").unwrap());
    answer(&mut requests, |island| island.press("Cancel").unwrap());
    assert_eq!(rest(requests), ["Order cancelled."]);
}
