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
            Request::RunningCommands(_) => panic!("unexpected RunningCommands"),
            Request::Clear => panic!("unexpected Clear"),
            Request::Exit => panic!("unexpected Exit"),
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
    assert_eq!(rest(run("count x")), [r#"error: invalid N: "x""#]);
    assert_eq!(
        rest(run("count 1 2")),
        [r#"error: unexpected argument "2"; usage: count [N]"#]
    );
}

#[test]
fn errors() {
    assert_eq!(rest(run("fail oh no")), ["About to fail…", "error: oh no"]);
    assert_eq!(rest(run("panic")), ["error: panicked: the demo panicked"]);
    assert_eq!(
        rest(run("contact x")),
        [r#"error: unexpected argument "x"; usage: contact"#]
    );
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
    answer(&mut requests, |island| {
        assert_eq!(island.fields().text("name"), "Ada Lovelace");
        // The address is missing, so it isn't submitted.
        assert!(!island.to_string().contains("Enter"));
        let error = island.press("Order").unwrap_err();
        assert_eq!(
            error.to_string(),
            "not submitted: Enter the delivery address."
        );
        assert!((island.to_string()).contains(r#"label "Enter the delivery address.""#));
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

#[test]
fn view_saves_images_as_asked() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("red.png");
    let red = kettle_ui::Image::from_rgba(1, 1, &[255, 0, 0, 255], "").unwrap();
    std::fs::write(&path, red.data().unwrap().bytes()).unwrap();
    let image = |flags: &str| {
        let line = format!("view {flags} {}", path.display());
        let Some(Request::PrintIsland(island)) = run(&line).next() else {
            panic!("expected an island");
        };
        match island.state("image") {
            Some(kettle_ui::Value::Image(image)) => image.clone(),
            other => panic!("not an image: {other:?}"),
        }
    };
    let copied = image("");
    assert!(!copied.is_linked() && !copied.is_ephemeral());
    assert!(image("-l").is_linked());
    assert!(image("-e").is_ephemeral());
    assert_eq!(
        rest(run("view -e -l x")),
        ["error: -e and -l can't be used together"]
    );
}

#[test]
fn table() {
    let lines = rest(run("table"));
    let [outline] = lines.as_slice() else {
        panic!("expected one island: {lines:?}");
    };
    let expected_start = "\
column
  label \"All borders\"
  table
    row
      label \"Planet\"
      label \"Moons\"
      label \"Distance (AU)\"
    row
      label \"Mercury\"
";
    assert!(outline.starts_with(expected_start), "{outline}");
    assert!(outline.contains("  table (horizontal borders)\n    row\n"));
}
