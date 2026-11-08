//! Runs the note commands on a database in a temporary directory, answering
//! their islands headlessly.

use futures_executor::{BlockingStream, block_on_stream};
use hob_core::{Interpreter, Request, Requests};
use kettle_ui::Outcome;
use kettle_ui_headless::Island;
use tempfile::TempDir;

/// An interpreter with the note commands and an empty database.
fn setup() -> (Interpreter, TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let mut interpreter = Interpreter::new();
    hob_notes::register(&mut interpreter, Some(dir.path().join("notes.sqlite")));
    (interpreter, dir)
}

fn start(interpreter: &Interpreter, line: &str) -> BlockingStream<Requests> {
    block_on_stream(interpreter.run(line).unwrap())
}

/// Everything the command prints and logs until it finishes.
fn rest(requests: BlockingStream<Requests>) -> Vec<String> {
    requests
        .map(|request| match request {
            Request::Print(message) => message,
            Request::Log(record) => record.to_string(),
            _ => panic!("unexpected request"),
        })
        .collect()
}

fn run(interpreter: &Interpreter, line: &str) -> Vec<String> {
    rest(start(interpreter, line))
}

/// Waits for the command to show an island, and answers it with `answer`.
fn answer(requests: &mut BlockingStream<Requests>, answer: impl FnOnce(&mut Island) -> Outcome) {
    let Some(Request::Show(show)) = requests.next() else {
        panic!("expected Show");
    };
    let (builder, reply) = show.into_parts();
    let mut island = Island::new(builder).unwrap();
    reply.send(answer(&mut island));
}

#[test]
fn note_and_title_from_arguments() {
    let (hob, _dir) = setup();
    assert_eq!(run(&hob, "note buy  milk"), ["Saved note #1."]);
    assert_eq!(run(&hob, "note call Ada"), ["Saved note #2."]);
    assert_eq!(run(&hob, "title Phone"), ["Named note #2: Phone"]);
    assert_eq!(run(&hob, "title ' '"), ["error: the title is empty"]);
    assert_eq!(run(&hob, "note ''"), ["error: the note is empty"]);
}

#[test]
fn note_asks_for_text() {
    let (hob, _dir) = setup();
    let mut requests = start(&hob, "note");
    answer(&mut requests, |island| {
        island.type_text("text", "  first line\nsecond\n").unwrap();
        island.press("Save").unwrap()
    });
    assert_eq!(rest(requests), ["Saved note #1."]);

    let mut requests = start(&hob, "note");
    answer(&mut requests, |island| island.press("Cancel").unwrap());
    assert_eq!(rest(requests), ["Not saved."]);

    // The text was trimmed; the title island shows its first line.
    let mut requests = start(&hob, "title");
    answer(&mut requests, |island| {
        assert!(
            island
                .to_string()
                .contains(r#"label "Name #1: first line…""#),
            "{island}"
        );
        island.type_text("title", "Lines").unwrap();
        island.press("Save").unwrap()
    });
    assert_eq!(rest(requests), ["Named note #1: Lines"]);

    // Asking again shows the old title to edit.
    let mut requests = start(&hob, "title");
    answer(&mut requests, |island| {
        assert_eq!(island.fields().text("title"), "Lines");
        island.press("Cancel").unwrap()
    });
    assert_eq!(rest(requests), ["Not named."]);
}

#[test]
fn nothing_to_name_or_finish() {
    let (hob, _dir) = setup();
    assert_eq!(run(&hob, "title x"), ["error: there are no notes yet"]);
    assert_eq!(
        run(&hob, "done"),
        ["error: nothing is active; pick a TODO note with todo"]
    );
    assert_eq!(run(&hob, "todo"), ["Nothing to do."]);
}

/// The labels of the buttons `todo` offers, with their keys.
fn offered(island: &Island) -> Vec<String> {
    (island.to_string().lines())
        .filter_map(|line| line.trim().strip_prefix("button "))
        .map(str::to_owned)
        .collect()
}

#[test]
fn todo_takes_items_from_each_source_in_turn() {
    let (hob, _dir) = setup();
    for text in ["one", "two", "TODO: three", "todo four", "five"] {
        run(&hob, &format!("note '{text}'"));
    }
    run(&hob, "title Todo five");
    let mut requests = start(&hob, "todo");
    answer(&mut requests, |island| {
        assert_eq!(
            offered(island),
            [
                r#""Do #3: TODO: three" [a]"#,
                r#""Name #1: one" [b]"#,
                r#""Do #4: todo four" [c]"#,
                r#""Not now""#,
            ]
        );
        island.press("Not now").unwrap()
    });
    assert_eq!(rest(requests), Vec::<String>::new());
}

#[test]
fn todo_works_on_a_note_until_done() {
    let (hob, _dir) = setup();
    run(&hob, "note todo: water plants");
    run(&hob, "title Todo water plants");
    run(&hob, "note todo: feed cat");
    run(&hob, "title Todo feed cat");

    let mut requests = start(&hob, "todo");
    answer(&mut requests, |island| {
        island.press("Do #1: Todo water plants").unwrap()
    });
    assert_eq!(
        rest(requests),
        ["Working on #1: Todo water plants; run done when it's done."]
    );

    // The active note isn't offered again.
    let mut requests = start(&hob, "todo");
    answer(&mut requests, |island| {
        let outline = island.to_string();
        assert!(outline.contains(r#"label "Working on #1: Todo water plants""#));
        assert_eq!(
            offered(island),
            [r#""Do #2: Todo feed cat" [a]"#, r#""Not now""#]
        );
        island.press("Do #2: Todo feed cat").unwrap()
    });
    rest(requests);

    // Picking another one put the first back.
    assert_eq!(run(&hob, "done"), ["Done: #2: Todo feed cat"]);
    let mut requests = start(&hob, "todo");
    answer(&mut requests, |island| {
        assert_eq!(
            offered(island),
            [r#""Do #1: Todo water plants" [a]"#, r#""Not now""#]
        );
        island.press("Do #1: Todo water plants").unwrap()
    });
    rest(requests);
    assert_eq!(run(&hob, "done"), ["Done: #1: Todo water plants"]);
    assert_eq!(run(&hob, "todo"), ["Nothing to do."]);
}

#[test]
fn todo_asks_for_a_name() {
    let (hob, _dir) = setup();
    run(&hob, "note first");
    run(&hob, "note second");
    let mut requests = start(&hob, "todo");
    answer(&mut requests, |island| {
        island.press("Name #1: first").unwrap()
    });
    // It names the note picked, not the last one.
    answer(&mut requests, |island| {
        island.type_text("title", "First").unwrap();
        island.press("Save").unwrap()
    });
    assert_eq!(rest(requests), ["Named note #1: First"]);
}

#[test]
fn nothing_else_to_do() {
    let (hob, _dir) = setup();
    run(&hob, "note todo: rest");
    run(&hob, "title todo: rest");
    let mut requests = start(&hob, "todo");
    answer(&mut requests, |island| {
        island.press("Do #1: todo: rest").unwrap()
    });
    rest(requests);
    assert_eq!(
        run(&hob, "todo"),
        ["Nothing else to do; working on #1: todo: rest."]
    );
}

#[test]
fn without_a_database() {
    let mut hob = Interpreter::new();
    hob_notes::register(&mut hob, None);
    assert_eq!(
        run(&hob, "note x"),
        ["error: there's no place for the notes database"]
    );
}
