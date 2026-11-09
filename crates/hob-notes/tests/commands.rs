//! Runs the note commands on a database in a temporary directory, answering
//! their islands headlessly.

use futures_executor::{BlockingStream, block_on_stream};
use hob_core::{Interpreter, Request, Requests};
use kettle_ui::Outcome;
use kettle_ui_headless::{Island, outline};
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

/// Everything the command prints and logs until it finishes; printed islands
/// as outlines.
fn rest(requests: BlockingStream<Requests>) -> Vec<String> {
    requests
        .map(|request| match request {
            Request::Print(message) => message,
            Request::PrintIsland(island) => outline(&island),
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

/// Runs `line`, which shows an island, and answers it with `answer`; returns
/// what's printed then.
fn run_answering(
    interpreter: &Interpreter,
    line: &str,
    answer_with: impl FnOnce(&mut Island) -> Outcome,
) -> Vec<String> {
    let mut requests = start(interpreter, line);
    answer(&mut requests, answer_with);
    rest(requests)
}

/// Notes as `notes list` shows them: `#ID state title` per row.
fn listed(interpreter: &Interpreter, line: &str) -> Vec<String> {
    let output = run(interpreter, line);
    let [table] = &output[..] else {
        panic!("expected one table: {output:?}");
    };
    if table == "No notes." {
        return Vec::new();
    }
    assert!(table.starts_with("table"), "{table}");
    let mut rows = Vec::new();
    for line in table.lines().skip(1) {
        let line = line.trim();
        if line == "row" {
            rows.push(Vec::new());
        } else if let Some(text) = line.strip_prefix("label ") {
            let text = text.trim_matches('"');
            rows.last_mut().unwrap().push(text.to_owned());
        }
    }
    rows.into_iter().map(|row| row.join(" ")).collect()
}

/// The first line of a printed note, and its text after the details line.
fn shown(output: &[String]) -> (String, String, String) {
    let [output] = output else {
        panic!("expected one message: {output:?}");
    };
    let mut parts = output.splitn(3, '\n');
    let heading = parts.next().unwrap().to_owned();
    let details = parts.next().unwrap().to_owned();
    let text = parts.next().unwrap().strip_prefix('\n').unwrap().to_owned();
    (heading, details, text)
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
fn note_text_is_free_form() {
    let (hob, _dir) = setup();
    assert_eq!(run(&hob, "note -x buy -- -2 --eggs"), ["Saved note #1."]);
    let (_, _, text) = shown(&run(&hob, "notes show 1"));
    assert_eq!(text, "-x buy -- -2 --eggs");
}

#[test]
fn every_command_has_a_usage_line() {
    let (hob, _dir) = setup();
    let mut hob = hob;
    hob_core::builtins::register(&mut hob);
    let usage = |command: &str| {
        let help = run(&hob, &format!("help {command}"));
        help[0].lines().nth(1).unwrap().to_owned()
    };
    assert_eq!(usage("note"), "usage: note [TEXT...]");
    assert_eq!(usage("title"), "usage: title [-n ID] [TITLE...]");
    assert_eq!(usage("todo"), "usage: todo");
    assert_eq!(usage("done"), "usage: done [ID]");
    assert_eq!(
        usage("notes"),
        "usage: notes [list [--all] [--todo] [-c NAME] | show ID | edit ID | delete [-y] ID \
         | reopen ID | categories [list | add NAME... | rename NAME NEW | delete [-y] NAME]]"
    );
}

#[test]
fn note_asks_for_text() {
    let (hob, _dir) = setup();
    let output = run_answering(&hob, "note", |island| {
        island.type_text("text", "  first line\nsecond\n").unwrap();
        island.press("Save").unwrap()
    });
    assert_eq!(output, ["Saved note #1."]);

    let output = run_answering(&hob, "note", |island| island.press("Cancel").unwrap());
    assert_eq!(output, ["Not saved."]);

    let output = run_answering(&hob, "note", |island| {
        island.type_text("title", "nothing").unwrap();
        island.press("Save").unwrap()
    });
    assert_eq!(output, ["error: the note is empty"]);

    // The text was trimmed; the title island shows its first line.
    let output = run_answering(&hob, "title", |island| {
        assert!(
            island
                .to_string()
                .contains(r#"label "Name #1: first line…""#),
            "{island}"
        );
        island.type_text("title", "Lines").unwrap();
        island.press("Save").unwrap()
    });
    assert_eq!(output, ["Named note #1: Lines"]);

    // Asking again shows the old title to edit.
    let output = run_answering(&hob, "title", |island| {
        assert_eq!(island.fields().text("title"), "Lines");
        island.press("Cancel").unwrap()
    });
    assert_eq!(output, ["Not named."]);
}

#[test]
fn note_island_takes_a_title() {
    let (hob, _dir) = setup();
    let output = run_answering(&hob, "note", |island| {
        island.type_text("title", "  Shopping ").unwrap();
        island.type_text("text", "milk\neggs").unwrap();
        island.press("Save").unwrap()
    });
    assert_eq!(output, ["Saved note #1."]);
    let (heading, _, text) = shown(&run(&hob, "notes show 1"));
    assert_eq!(
        (heading.as_str(), text.as_str()),
        ("#1: Shopping", "milk\neggs")
    );
    // An empty title leaves it unnamed.
    run_answering(&hob, "note", |island| {
        island.type_text("title", " ").unwrap();
        island.type_text("text", "plain").unwrap();
        island.press("Save").unwrap()
    });
    assert_eq!(shown(&run(&hob, "notes show 2")).0, "#2");
}

#[test]
fn title_names_the_note_given() {
    let (hob, _dir) = setup();
    run(&hob, "note first");
    run(&hob, "note second");
    assert_eq!(run(&hob, "title -n 1 First"), ["Named note #1: First"]);
    assert_eq!(run(&hob, "title -n '#2' Second"), ["Named note #2: Second"]);
    assert_eq!(run(&hob, "title --note=1 1st"), ["Named note #1: 1st"]);
    assert_eq!(run(&hob, "title -n 3 x"), ["error: there's no note #3"]);
    assert_eq!(run(&hob, "title -n x y"), [r#"error: invalid ID: "x""#]);
    // A title can start with a number.
    assert_eq!(run(&hob, "title 2 eggs"), ["Named note #2: 2 eggs"]);
    let output = run_answering(&hob, "title -n 1", |island| {
        assert_eq!(island.fields().text("title"), "1st");
        island.type_text("title", "One").unwrap();
        island.press("Save").unwrap()
    });
    assert_eq!(output, ["Named note #1: One"]);
    assert_eq!(listed(&hob, "notes"), ["#1  One", "#2  2 eggs"]);
}

#[test]
fn nothing_to_name_or_finish() {
    let (hob, _dir) = setup();
    assert_eq!(run(&hob, "title x"), ["error: there are no notes yet"]);
    assert_eq!(
        run(&hob, "done"),
        ["error: nothing is active; pick a TODO note with todo, or give its ID"]
    );
    assert_eq!(run(&hob, "todo"), ["Nothing to do."]);
    assert_eq!(run(&hob, "notes"), ["No notes."]);
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
    let output = run_answering(&hob, "todo", |island| {
        assert_eq!(
            offered(island),
            [
                r#""Do #3: three" [a]"#,
                r#""Name #1: one" [b]"#,
                r#""Do #4: four" [c]"#,
                r#""Not now""#,
            ]
        );
        island.press("Not now").unwrap()
    });
    assert_eq!(output, ["Nothing picked."]);
}

#[test]
fn a_tag_is_a_word() {
    let (hob, _dir) = setup();
    for text in ["todos are fun", "todoist", "TODO: x", "todo-x y", "todo y"] {
        run(&hob, &format!("note '{text}'"));
    }
    run(&hob, "title -n 1 Todos");
    // The tag is taken out, so nothing's left.
    assert_eq!(run(&hob, "note todo:"), ["error: the note is empty"]);
    // An unnamed TODO note is offered only to do, not also to name.
    let output = run_answering(&hob, "todo", |island| {
        assert_eq!(
            offered(island),
            [
                r#""Do #3: x" [a]"#,
                r#""Name #2: todoist" [b]"#,
                r#""Do #5: y" [c]"#,
                r#""Not now""#,
            ]
        );
        island.press("Not now").unwrap()
    });
    assert_eq!(output, ["Nothing picked."]);
    assert_eq!(
        listed(&hob, "notes list --todo"),
        ["#3 todo x", "#5 todo y"]
    );
}

#[test]
fn a_tag_in_the_text_counts_only_without_a_title() {
    let (hob, _dir) = setup();
    for title in ["Plain", "", "todo: Tagged"] {
        run_answering(&hob, "note", |island| {
            island.type_text("title", title).unwrap();
            island.type_text("text", "todo x").unwrap();
            island.press("Save").unwrap()
        });
    }
    assert_eq!(
        listed(&hob, "notes"),
        ["#1  Plain", "#2 todo x", "#3 todo Tagged"]
    );
    assert_eq!(shown(&run(&hob, "notes show 3")).2, "todo x");
    // `title` tags a note if the title has a tag, and keeps its tag if not.
    assert_eq!(run(&hob, "title -n 1 TODO later"), ["Named note #1: later"]);
    assert_eq!(run(&hob, "title -n 1 Later"), ["Named note #1: Later"]);
    assert_eq!(run(&hob, "title -n 1 todo"), ["error: the title is empty"]);
    assert_eq!(listed(&hob, "notes list --todo")[0], "#1 todo Later");
}

#[test]
fn todo_works_on_a_note_until_done() {
    let (hob, _dir) = setup();
    run(&hob, "note todo: water plants");
    run(&hob, "title Todo water plants");
    run(&hob, "note todo: feed cat");
    run(&hob, "title Todo feed cat");

    let output = run_answering(&hob, "todo", |island| {
        island.press("Do #1: water plants").unwrap()
    });
    assert_eq!(
        output,
        ["Working on #1: water plants; run done when it's done."]
    );

    // The active note isn't offered again.
    run_answering(&hob, "todo", |island| {
        let outline = island.to_string();
        assert!(outline.contains(r#"label "Working on #1: water plants""#));
        assert_eq!(
            offered(island),
            [r#""Do #2: feed cat" [a]"#, r#""Not now""#]
        );
        island.press("Do #2: feed cat").unwrap()
    });

    // Picking another one put the first back.
    assert_eq!(run(&hob, "done"), ["Done: #2: feed cat"]);
    run_answering(&hob, "todo", |island| {
        assert_eq!(
            offered(island),
            [r#""Do #1: water plants" [a]"#, r#""Not now""#]
        );
        island.press("Do #1: water plants").unwrap()
    });
    assert_eq!(run(&hob, "done"), ["Done: #1: water plants"]);
    assert_eq!(run(&hob, "todo"), ["Nothing to do."]);
}

#[test]
fn done_takes_an_id() {
    let (hob, _dir) = setup();
    run(&hob, "note todo: a");
    run(&hob, "note todo: b");
    run(&hob, "note plain");
    run_answering(&hob, "todo", |island| island.press("Do #1: a").unwrap());
    // An open one, without picking it first.
    assert_eq!(run(&hob, "done '#2'"), ["Done: #2: b"]);
    // The active one, by its ID.
    assert_eq!(run(&hob, "done 1"), ["Done: #1: a"]);
    assert_eq!(run(&hob, "done 1"), ["error: #1 is already done"]);
    assert_eq!(run(&hob, "done 3"), ["error: #3 isn't a TODO note"]);
    assert_eq!(run(&hob, "done 4"), ["error: there's no note #4"]);
    assert_eq!(run(&hob, "done x"), [r#"error: invalid ID: "x""#]);
    assert_eq!(
        run(&hob, "done 1 2"),
        [r#"error: unexpected argument "2"; usage: done [ID]"#]
    );
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
    run_answering(&hob, "todo", |island| island.press("Do #1: rest").unwrap());
    assert_eq!(
        run(&hob, "todo"),
        ["Nothing else to do; working on #1: rest."]
    );
}

#[test]
fn notes_lists_them() {
    let (hob, _dir) = setup();
    run(&hob, "note plain one");
    run(&hob, "note todo: open");
    run(&hob, "note todo: active");
    run(&hob, "note todo: finished");
    run(
        &hob,
        "note 'long line that goes on and on, much longer than fifty characters'",
    );
    run(&hob, "title -n 1 Plain");
    run_answering(&hob, "todo", |island| {
        island.press("Do #3: active").unwrap()
    });
    run(&hob, "done 4");
    let preview = "long line that goes on and on, much longer than fi…";
    let not_done = [
        "#1  Plain".to_owned(),
        "#2 todo open".to_owned(),
        "#3 active active".to_owned(),
        format!("#5  {preview}"),
    ];
    assert_eq!(listed(&hob, "notes"), not_done);
    assert_eq!(listed(&hob, "notes list"), not_done);
    assert_eq!(
        listed(&hob, "notes list --all"),
        [
            "#1  Plain",
            "#2 todo open",
            "#3 active active",
            "#4 done finished",
            &format!("#5  {preview}"),
        ]
    );
    assert_eq!(
        listed(&hob, "notes list --todo"),
        ["#2 todo open", "#3 active active"]
    );
    assert_eq!(
        listed(&hob, "notes list --todo --all"),
        ["#2 todo open", "#3 active active", "#4 done finished"]
    );
    // The table has horizontal borders.
    let output = run(&hob, "notes");
    assert!(
        output[0].starts_with("table (horizontal borders)\n"),
        "{output:?}"
    );
    assert_eq!(
        run(&hob, "notes list -x"),
        ["error: unknown option -x; usage: notes list [--all] [--todo] [-c NAME]"]
    );
    let error = run(&hob, "notes frob");
    assert!(
        error[0].starts_with("error: unknown subcommand \"frob\"; usage: notes [list "),
        "{error:?}"
    );
}

#[test]
fn notes_list_only_done() {
    let (hob, _dir) = setup();
    run(&hob, "note todo x");
    run(&hob, "done 1");
    assert_eq!(run(&hob, "notes"), ["No notes."]);
    assert_eq!(run(&hob, "notes list --todo"), ["No notes."]);
    assert_eq!(listed(&hob, "notes list --all"), ["#1 done x"]);
}

/// Whether `details` is the details line of a note: its state (if any), and
/// local times in the form `YYYY-MM-DD HH:MM`.
fn check_details(details: &str, state: &str, done: bool) {
    let mut parts = details.split(", ");
    if !state.is_empty() {
        assert_eq!(parts.next(), Some(state), "{details}");
    }
    let is_time = |s: &str| {
        s.len() == 16
            && s.chars().enumerate().all(|(i, c)| match i {
                4 | 7 => c == '-',
                10 => c == ' ',
                13 => c == ':',
                _ => c.is_ascii_digit(),
            })
    };
    let created = parts.next().unwrap().strip_prefix("created ").unwrap();
    assert!(is_time(created), "{details}");
    match parts.next() {
        Some(when) => {
            assert!(done, "{details}");
            assert!(is_time(when.strip_prefix("done ").unwrap()), "{details}");
        }
        None => assert!(!done, "{details}"),
    }
    assert_eq!(parts.next(), None);
}

#[test]
fn notes_show_prints_a_note() {
    let (hob, _dir) = setup();
    run(&hob, "note plain");
    run_answering(&hob, "note", |island| {
        island.type_text("title", "TODO: Shopping").unwrap();
        island.type_text("text", "buy\n\nmilk").unwrap();
        island.press("Save").unwrap()
    });
    let (heading, details, text) = shown(&run(&hob, "notes show 1"));
    assert_eq!((heading.as_str(), text.as_str()), ("#1", "plain"));
    check_details(&details, "", false);
    let (heading, details, text) = shown(&run(&hob, "notes show '#2'"));
    assert_eq!(
        (heading.as_str(), text.as_str()),
        ("#2: Shopping", "buy\n\nmilk")
    );
    check_details(&details, "todo", false);
    run(&hob, "done 2");
    let (_, details, _) = shown(&run(&hob, "notes show 2"));
    check_details(&details, "done", true);
    assert_eq!(run(&hob, "notes show 3"), ["error: there's no note #3"]);
    assert_eq!(
        run(&hob, "notes show"),
        ["error: missing ID; usage: notes show ID"]
    );
    assert_eq!(run(&hob, "notes show '#'"), [r##"error: invalid ID: "#""##]);
}

#[test]
fn notes_edit_changes_a_note() {
    let (hob, _dir) = setup();
    run(&hob, "note first");
    run(&hob, "title One");
    let output = run_answering(&hob, "notes edit 1", |island| {
        assert_eq!(island.fields().text("title"), "One");
        assert_eq!(island.fields().text("text"), "first");
        island.type_text("title", " todo: Uno ").unwrap();
        island.type_text("text", " todo: first\nreally ").unwrap();
        island.press("Save").unwrap()
    });
    assert_eq!(output, ["Saved note #1."]);
    let (heading, details, text) = shown(&run(&hob, "notes show 1"));
    assert_eq!(heading, "#1: Uno");
    // With a title, the text's tag is just text.
    assert_eq!(text, "todo: first\nreally");
    check_details(&details, "todo", false);

    // The tag is shown before the title. An empty title makes it unnamed
    // again, so the tag comes from the text.
    run_answering(&hob, "notes edit 1", |island| {
        assert_eq!(island.fields().text("title"), "todo: Uno");
        island.type_text("title", "").unwrap();
        island.press("Save").unwrap()
    });
    let (heading, details, text) = shown(&run(&hob, "notes show 1"));
    assert_eq!((heading.as_str(), text.as_str()), ("#1", "first\nreally"));
    check_details(&details, "todo", false);

    // Unnamed, the tag is shown alone; taking it out untags the note.
    run_answering(&hob, "notes edit 1", |island| {
        assert_eq!(island.fields().text("title"), "todo");
        island.type_text("title", "").unwrap();
        island.press("Save").unwrap()
    });
    check_details(&shown(&run(&hob, "notes show 1")).1, "", false);

    let output = run_answering(&hob, "notes edit 1", |island| {
        island.type_text("text", "  ").unwrap();
        island.press("Save").unwrap()
    });
    assert_eq!(output, ["error: the note is empty"]);
    let output = run_answering(&hob, "notes edit 1", |island| {
        island.type_text("text", "changed").unwrap();
        island.press("Cancel").unwrap()
    });
    assert_eq!(output, ["Not saved."]);
    assert_eq!(shown(&run(&hob, "notes show 1")).2, "first\nreally");
    assert_eq!(run(&hob, "notes edit 2"), ["error: there's no note #2"]);
}

#[test]
fn notes_delete_asks_first() {
    let (hob, _dir) = setup();
    run(&hob, "note todo: a");
    run(&hob, "note b");
    let output = run_answering(&hob, "notes delete 1", |island| {
        let outline = island.to_string();
        assert!(outline.contains(r#"label "Delete #1: a?""#), "{outline}");
        assert!(outline.contains(r#"label "todo, created "#), "{outline}");
        assert!(outline.contains(r#"button "Delete" (danger)"#), "{outline}");
        island.press("Cancel").unwrap()
    });
    assert_eq!(output, ["Not deleted."]);
    let output = run_answering(&hob, "notes delete 1", |island| {
        island.press("Delete").unwrap()
    });
    assert_eq!(output, ["Deleted note #1."]);
    assert_eq!(run(&hob, "notes delete -y '#2'"), ["Deleted note #2."]);
    assert_eq!(
        run(&hob, "notes delete -y 2"),
        ["error: there's no note #2"]
    );
    assert_eq!(run(&hob, "notes delete 2"), ["error: there's no note #2"]);
    assert_eq!(run(&hob, "notes"), ["No notes."]);
    // Ids aren't reused.
    assert_eq!(run(&hob, "note c"), ["Saved note #3."]);
}

#[test]
fn notes_reopen() {
    let (hob, _dir) = setup();
    run(&hob, "note todo: a");
    run(&hob, "note todo: b");
    run(&hob, "note plain");
    run(&hob, "done 1");
    assert_eq!(run(&hob, "notes reopen 1"), ["Reopened #1: a"]);
    let (_, details, _) = shown(&run(&hob, "notes show 1"));
    check_details(&details, "todo", false);
    assert_eq!(run(&hob, "notes reopen 1"), ["error: #1 is already open"]);
    run_answering(&hob, "todo", |island| island.press("Do #2: b").unwrap());
    assert_eq!(run(&hob, "notes reopen 2"), ["Reopened #2: b"]);
    assert_eq!(
        listed(&hob, "notes list --todo"),
        ["#1 todo a", "#2 todo b"]
    );
    assert_eq!(run(&hob, "notes reopen 3"), ["error: #3 isn't a TODO note"]);
    assert_eq!(run(&hob, "notes reopen 4"), ["error: there's no note #4"]);
}

#[test]
fn picking_rechecks_the_note() {
    let (hob, _dir) = setup();
    run(&hob, "note todo: a");
    run(&hob, "note todo: b");
    run(&hob, "note todo: c");
    let output = run_answering(&hob, "todo", |island| {
        assert_eq!(run(&hob, "done 1"), ["Done: #1: a"]);
        island.press("Do #1: a").unwrap()
    });
    assert_eq!(output, ["error: note #1 was marked done meanwhile"]);
    let output = run_answering(&hob, "todo", |island| {
        assert_eq!(run(&hob, "notes delete -y 2"), ["Deleted note #2."]);
        island.press("Do #2: b").unwrap()
    });
    assert_eq!(output, ["error: note #2 was deleted meanwhile"]);
    let output = run_answering(&hob, "todo", |island| {
        let edited = run_answering(&hob, "notes edit 3", |island| {
            island.type_text("title", "").unwrap();
            island.press("Save").unwrap()
        });
        assert_eq!(edited, ["Saved note #3."]);
        island.press("Do #3: c").unwrap()
    });
    assert_eq!(output, ["error: note #3 isn't a TODO note any more"]);
    assert!(
        listed(&hob, "notes")
            .iter()
            .all(|row| !row.contains("active"))
    );
}

#[test]
fn saving_rechecks_the_note() {
    let (hob, _dir) = setup();
    for text in ["a", "b", "c", "d"] {
        run(&hob, &format!("note {text}"));
    }
    let output = run_answering(&hob, "notes edit 1", |island| {
        run(&hob, "notes delete -y 1");
        island.type_text("text", "lost").unwrap();
        island.press("Save").unwrap()
    });
    assert_eq!(output, ["error: note #1 was deleted meanwhile"]);
    let output = run_answering(&hob, "title -n 2", |island| {
        run(&hob, "notes delete -y 2");
        island.type_text("title", "B").unwrap();
        island.press("Save").unwrap()
    });
    assert_eq!(output, ["error: note #2 was deleted meanwhile"]);
    let mut requests = start(&hob, "todo");
    answer(&mut requests, |island| island.press("Name #3: c").unwrap());
    answer(&mut requests, |island| {
        run(&hob, "notes delete -y 3");
        island.type_text("title", "C").unwrap();
        island.press("Save").unwrap()
    });
    assert_eq!(rest(requests), ["error: note #3 was deleted meanwhile"]);
    let output = run_answering(&hob, "notes delete 4", |island| {
        run(&hob, "notes delete -y 4");
        island.press("Delete").unwrap()
    });
    assert_eq!(output, ["error: note #4 was deleted meanwhile"]);
    assert_eq!(run(&hob, "notes"), ["No notes."]);
}

#[test]
fn last_edit_wins() {
    let (hob, _dir) = setup();
    run(&hob, "note a");
    let output = run_answering(&hob, "notes edit 1", |island| {
        let other = run_answering(&hob, "notes edit 1", |island| {
            island.type_text("text", "first").unwrap();
            island.press("Save").unwrap()
        });
        assert_eq!(other, ["Saved note #1."]);
        island.type_text("text", "second").unwrap();
        island.press("Save").unwrap()
    });
    assert_eq!(output, ["Saved note #1."]);
    assert_eq!(shown(&run(&hob, "notes show 1")).2, "second");
}

/// Categories as `notes categories` shows them: `name count` per row.
fn categories(interpreter: &Interpreter) -> Vec<String> {
    listed(interpreter, "notes categories")
}

/// The checkboxes of an island, as `name [x]` or `name [ ]`.
fn boxes(island: &Island) -> Vec<String> {
    (island.to_string().lines())
        .filter_map(|line| line.trim().strip_prefix("checkbox "))
        .map(|line| {
            let (_, rest) = line.split_once(": ").unwrap();
            let (mark, name) = rest.split_once("] ").unwrap();
            format!("{} {mark}]", name.trim_matches('"'))
        })
        .collect()
}

#[test]
fn categories_are_defined() {
    let (hob, _dir) = setup();
    assert_eq!(run(&hob, "notes categories"), ["No categories."]);
    assert_eq!(
        run(&hob, "notes categories add work 'at home'"),
        [r#"Added category "work"."#, r#"Added category "at home"."#]
    );
    assert_eq!(categories(&hob), ["at home no notes", "work no notes"]);
    assert_eq!(listed(&hob, "notes categories list"), categories(&hob));
    assert_eq!(
        run(&hob, "notes categories add new WORK"),
        [r#"error: there's already a category "work""#]
    );
    assert_eq!(
        run(&hob, "notes categories add x X"),
        [r#"error: category "X" is given twice"#]
    );
    assert_eq!(
        run(&hob, "notes categories add ' '"),
        ["error: the category name is empty"]
    );
    assert_eq!(
        run(&hob, "notes categories add 'a\tb'"),
        [r#"error: invalid category name "a\tb""#]
    );
    // Nothing was added by the failed commands.
    assert_eq!(categories(&hob).len(), 2);
    assert_eq!(
        run(&hob, "notes categories add"),
        ["error: missing NAME; usage: notes categories add NAME..."]
    );

    assert_eq!(
        run(&hob, "notes categories rename Work job"),
        [r#"Renamed category "work" to "job"."#]
    );
    assert_eq!(
        run(&hob, "notes categories rename job Job"),
        [r#"Renamed category "job" to "Job"."#]
    );
    assert_eq!(
        run(&hob, "notes categories rename job 'AT HOME'"),
        [r#"error: there's already a category "at home""#]
    );
    assert_eq!(
        run(&hob, "notes categories rename x y"),
        [r#"error: there's no category "x""#]
    );
    assert_eq!(categories(&hob), ["at home no notes", "Job no notes"]);
}

#[test]
fn categories_are_deleted() {
    let (hob, _dir) = setup();
    run(&hob, "notes categories add a b");
    run(&hob, "note x");
    run_answering(&hob, "notes edit 1", |island| {
        island.set_checked("category-1", true).unwrap();
        island.set_checked("category-2", true).unwrap();
        island.press("Save").unwrap()
    });
    let output = run_answering(&hob, "notes categories delete A", |island| {
        let outline = island.to_string();
        assert!(
            outline.contains(r#"label "Delete category \"a\"?""#),
            "{outline}"
        );
        assert!(
            outline.contains(r#"label "It's taken off 1 note.""#),
            "{outline}"
        );
        island.press("Cancel").unwrap()
    });
    assert_eq!(output, ["Not deleted."]);
    let output = run_answering(&hob, "notes categories delete a", |island| {
        island.press("Delete").unwrap()
    });
    assert_eq!(output, [r#"Deleted category "a"."#]);
    assert_eq!(listed(&hob, "notes"), ["#1  x b"]);
    assert_eq!(
        run(&hob, "notes categories delete -y b"),
        [r#"Deleted category "b"."#]
    );
    assert_eq!(listed(&hob, "notes"), ["#1  x"]);
    assert_eq!(
        run(&hob, "notes categories delete -y b"),
        [r#"error: there's no category "b""#]
    );
    assert_eq!(run(&hob, "notes categories"), ["No categories."]);
}

#[test]
fn notes_edit_has_a_checkbox_per_category() {
    let (hob, _dir) = setup();
    run(&hob, "note shopping");
    // Without categories, there are no checkboxes.
    run_answering(&hob, "notes edit 1", |island| {
        let outline = island.to_string();
        assert!(!outline.contains("Categories"), "{outline}");
        assert_eq!(boxes(island), Vec::<String>::new());
        island.press("Cancel").unwrap()
    });

    run(&hob, "notes categories add work home");
    let output = run_answering(&hob, "notes edit 1", |island| {
        assert!(island.to_string().contains(r#"label "Categories:""#));
        // Sorted by name.
        assert_eq!(boxes(island), ["home [ ]", "work [ ]"]);
        island.set_checked("category-2", true).unwrap();
        island.press("Save").unwrap()
    });
    assert_eq!(output, ["Saved note #1."]);
    let [shown] = &run(&hob, "notes show 1")[..] else {
        panic!()
    };
    let lines: Vec<_> = shown.lines().collect();
    assert_eq!(lines[0], "#1");
    assert_eq!(lines[2..], ["Categories: home", "", "shopping"]);
    assert_eq!(categories(&hob), ["home 1 note", "work no notes"]);

    // The edit island shows the note's categories checked.
    run_answering(&hob, "notes edit 1", |island| {
        assert_eq!(boxes(island), ["home [x]", "work [ ]"]);
        island.set_checked("category-1", true).unwrap();
        island.press("Save").unwrap()
    });
    assert_eq!(listed(&hob, "notes"), ["#1  shopping home, work"]);
    run_answering(&hob, "notes edit 1", |island| {
        assert_eq!(boxes(island), ["home [x]", "work [x]"]);
        island.set_checked("category-2", false).unwrap();
        island.press("Save").unwrap()
    });
    assert_eq!(listed(&hob, "notes"), ["#1  shopping work"]);
    // Cancelling changes nothing.
    run_answering(&hob, "notes edit 1", |island| {
        island.set_checked("category-1", false).unwrap();
        island.press("Cancel").unwrap()
    });
    assert_eq!(listed(&hob, "notes"), ["#1  shopping work"]);
}

#[test]
fn note_island_has_a_checkbox_per_category() {
    let (hob, _dir) = setup();
    run(&hob, "notes categories add home");
    let output = run_answering(&hob, "note", |island| {
        assert_eq!(boxes(island), ["home [ ]"]);
        island.type_text("text", "milk").unwrap();
        island.set_checked("category-1", true).unwrap();
        island.press("Save").unwrap()
    });
    assert_eq!(output, ["Saved note #1."]);
    // From arguments, it's in none.
    run(&hob, "note eggs");
    assert_eq!(listed(&hob, "notes"), ["#1  milk home", "#2  eggs "]);
}

#[test]
fn a_category_deleted_while_editing_is_left_out() {
    let (hob, _dir) = setup();
    run(&hob, "notes categories add a b");
    run(&hob, "note x");
    let output = run_answering(&hob, "notes edit 1", |island| {
        island.set_checked("category-1", true).unwrap();
        island.set_checked("category-2", true).unwrap();
        run(&hob, "notes categories delete -y a");
        island.press("Save").unwrap()
    });
    assert_eq!(output, ["Saved note #1."]);
    assert_eq!(listed(&hob, "notes"), ["#1  x b"]);
}

#[test]
fn notes_list_by_category() {
    let (hob, _dir) = setup();
    run(&hob, "notes categories add work home");
    for text in ["todo report", "todo dishes", "plain", "todo old report"] {
        run(&hob, &format!("note '{text}'"));
    }
    // Ids: work is 1, home 2.
    for (id, category) in [(1, 1), (2, 2), (4, 1)] {
        run_answering(&hob, &format!("notes edit {id}"), |island| {
            island
                .set_checked(&format!("category-{category}"), true)
                .unwrap();
            island.press("Save").unwrap()
        });
    }
    run(&hob, "done 4");
    assert_eq!(listed(&hob, "notes list -c WORK"), ["#1 todo report work"]);
    assert_eq!(
        listed(&hob, "notes list --all --category=work"),
        ["#1 todo report work", "#4 done old report work"]
    );
    assert_eq!(
        listed(&hob, "notes list --todo -c home"),
        ["#2 todo dishes home"]
    );
    assert_eq!(
        run(&hob, "notes list -c nope"),
        [r#"error: there's no category "nope""#]
    );
    // Notes without categories, listed alone, get no column for them.
    let output = run(&hob, "notes list --all");
    assert_eq!(output[0].matches("row").count(), 4);
    run(&hob, "notes categories delete -y work");
    run(&hob, "notes categories delete -y home");
    let table = &run(&hob, "notes")[0];
    let labels = table.matches("label").count();
    assert_eq!(labels, 3 * 3, "{table}");
}

#[test]
fn deleting_a_note_takes_it_out_of_its_categories() {
    let (hob, _dir) = setup();
    run(&hob, "notes categories add a");
    run_answering(&hob, "note", |island| {
        island.type_text("text", "x").unwrap();
        island.set_checked("category-1", true).unwrap();
        island.press("Save").unwrap()
    });
    assert_eq!(categories(&hob), ["a 1 note"]);
    run(&hob, "notes delete -y 1");
    assert_eq!(categories(&hob), ["a no notes"]);
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
