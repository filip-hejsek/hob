//! Tests, including end-to-end tests that run the shell in `iced_test`'s
//! [`Emulator`], with test-only commands.

use std::thread;
use std::time::{Duration, Instant};

use futures_channel::mpsc;
use hob_core::args::{Kind, Spec};
use iced::{Size, mouse};
use iced_test::emulator::{Emulator, Event, Mode};
use iced_test::instruction::{Expectation, Interaction, Key, Keyboard, Mouse, Target};
use iced_test::{Instruction, program::Program};
use kettle_ui::widget::{cancel_button, column, label, row, submit_button, text_field};
use kettle_ui::{IslandBuilder, Outcome};

use super::*;

#[test]
fn dependency_warnings_are_not_shown() {
    let shown = |level, target| shows_notice(&LogRecord::new(level, target, "m"), Level::Info);
    assert!(shown(Level::Warn, "hob_gui"));
    assert!(!shown(Level::Info, "hob_gui"));
    assert!(shown(Level::Error, "wgpu_core"));
    assert!(!shown(Level::Warn, "iced_winit::proxy"));
    let record = LogRecord::new(Level::Warn, "hob_gui", "m");
    assert!(!shows_notice(&record, Level::Error));
}

#[test]
fn running_a_command_shows_its_output() {
    let mut shell = start(None);
    shell.run("say hi");
    shell.expect("> say hi");
    shell.expect("hi");
}

#[test]
fn startup_lines_run_first() {
    let config = GuiConfig {
        startup: vec!["say one".into(), "say two".into()],
        ..gui_config()
    };
    let program = application(
        Some(Arc::new(interpreter())),
        None,
        Limits::default(),
        config,
        Level::Info,
        None,
    );
    let mut shell = Harness::new(program);
    shell.expect("> say one");
    shell.expect("two");
}

#[test]
fn unknown_command_shows_an_error() {
    let mut shell = start(None);
    shell.run("nonsense");
    shell.expect("> nonsense");
    let error = interpreter().run("nonsense").err().unwrap();
    shell.expect(&LogRecord::new(Level::Error, TARGET, error.to_string()).to_string());
}

#[test]
fn answering_an_island() {
    let mut shell = start(None);
    shell.run("greet");
    shell.expect("OK");
    shell.step(click("Your name"));
    shell.step(typewrite("Ann"));
    shell.step(click("OK"));
    shell.expect("Hello, Ann!");
    // The frozen island is still shown.
    shell.expect("Ann");
}

#[test]
fn a_new_island_gets_focus_and_gives_it_back() {
    let mut shell = start(None);
    shell.run("greet");
    shell.expect("OK");
    // No click: the name field has focus. Enter in the last field submits.
    shell.settle();
    shell.step(typewrite("Ann"));
    shell.step(press(Key::Enter));
    shell.expect("Hello, Ann!");
    // The prompt has focus again.
    shell.settle();
    shell.step(typewrite("say back"));
    shell.step(press(Key::Enter));
    shell.expect("back");
}

#[test]
fn clearing_keeps_the_focus_of_a_live_island() {
    let mut shell = start(None);
    shell.run("say before");
    shell.expect("before");
    shell.run("greet");
    shell.expect("OK");
    shell.settle();
    shell.step(typewrite("An"));
    // Ctrl+L removes the block before the island's.
    let ctrl_l = Message::Key(keyboard::Key::Character("l".into()), Modifiers::CTRL);
    shell.update(ctrl_l);
    shell.expect_absent("> say before");
    // The name field still has focus.
    shell.step(typewrite("n"));
    shell.step(press(Key::Enter));
    shell.expect("Hello, Ann!");
}

#[test]
fn typing_while_nothing_has_focus_types_into_the_prompt() {
    let mut shell = start(None);
    shell.run("say hi");
    shell.expect("hi");
    // A click on the output unfocuses the prompt.
    shell.step(click("> say hi"));
    shell.settle();
    // The emulator doesn't pass events to subscriptions, so this sends
    // what `key_message` makes of the first key.
    shell.update(Message::Typed("s".into()));
    // The prompt has focus now, and takes the rest.
    shell.step(typewrite("ay again"));
    shell.step(press(Key::Enter));
    shell.expect("again");
}

#[test]
fn only_printable_text_no_widget_took_is_typed() {
    let press = |key: keyboard::Key, modifiers, text: Option<&str>| {
        iced::Event::Keyboard(keyboard::Event::KeyPressed {
            modified_key: key.clone(),
            key,
            physical_key: keyboard::key::Physical::Unidentified(
                keyboard::key::NativeCode::Unidentified,
            ),
            location: keyboard::Location::Standard,
            modifiers,
            text: text.map(Into::into),
            repeat: false,
        })
    };
    let a = || keyboard::Key::Character("a".into());
    let ignored = event::Status::Ignored;
    let typed = key_message(press(a(), Modifiers::SHIFT, Some("A")), ignored);
    assert!(matches!(typed, Some(Message::Typed(t)) if t == "A"));
    // A widget took it.
    let captured = event::Status::Captured;
    assert!(key_message(press(a(), Modifiers::empty(), Some("a")), captured).is_none());
    // Not text, but maybe bound.
    let ctrl_a = key_message(press(a(), Modifiers::CTRL, Some("\u{1}")), ignored);
    assert!(matches!(ctrl_a, Some(Message::Key(..))));
    let tab = keyboard::Key::Named(keyboard::key::Named::Tab);
    let tab = key_message(press(tab, Modifiers::empty(), Some("\t")), ignored);
    assert!(matches!(tab, Some(Message::Key(..))));
}

// The emulator doesn't pass keys to subscriptions, so the tests below send
// the shell's actions for Tab and Shift+Tab, and the key message for Escape.

#[test]
fn tab_again_picks_a_candidate_and_enter_runs_it() {
    let mut shell = start(None);
    shell.step(click(PLACEHOLDER));
    shell.step(typewrite("s"));
    shell.update(Message::Action(Action::Complete));
    shell.expect("sing");
    // Picks say, then sing, then back to say.
    shell.update(Message::Action(Action::Complete));
    shell.update(Message::Action(Action::Complete));
    shell.update(Message::Action(Action::PickPrevious));
    shell.step(press(Key::Enter));
    shell.expect("> say");
    // The offer is gone.
    shell.expect_absent("sing");
}

#[test]
fn the_offer_doesnt_scroll_a_short_scrollback() {
    // Enough candidates for several rows, so the view gets shorter by more
    // than a line.
    let dir = tempfile::tempdir().unwrap();
    for i in 0..24 {
        std::fs::write(dir.path().join(format!("a candidate {i:02}")), "").unwrap();
    }
    let dir = dir.path().display().to_string();
    let mut shell = start(None);
    shell.run("say hi");
    shell.expect("hi");
    shell.step(click(PLACEHOLDER));
    shell.step(typewrite(&format!("look '{dir}/")));
    shell.update(Message::Action(Action::Complete));
    shell.expect("a candidate 23");
    // The view got shorter, but the top line is still shown: clicking it
    // needs it in sight.
    shell.step(click("> say hi"));
}

#[test]
fn space_takes_the_picked_candidate() {
    let mut shell = start(None);
    shell.step(click(PLACEHOLDER));
    shell.step(typewrite("s"));
    shell.update(Message::Action(Action::Complete));
    // Going back from the start picks the last.
    shell.update(Message::Action(Action::PickPrevious));
    shell.step(typewrite(" loud"));
    shell.step(press(Key::Enter));
    shell.expect("> sing loud");
    shell.expect("la la");
}

#[test]
fn escape_restores_the_line_from_before_picking() {
    let mut shell = start(None);
    shell.step(click(PLACEHOLDER));
    shell.step(typewrite("s"));
    shell.update(Message::Action(Action::Complete));
    shell.update(Message::Action(Action::Complete));
    shell.update(Message::Action(Action::Complete));
    // The prompt keeps focus.
    shell.step(press(Key::Escape));
    // The candidates are still offered.
    shell.expect("sing");
    shell.step(typewrite("ay hi"));
    shell.step(press(Key::Enter));
    shell.expect("> say hi");
}

#[test]
fn typing_the_delimiter_goes_on_into_the_picked_directory() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("nested dir")).unwrap();
    std::fs::create_dir(dir.path().join("new")).unwrap();
    std::fs::write(dir.path().join("notes"), "").unwrap();
    let dir = dir.path().display().to_string();
    let mut shell = start(None);
    shell.step(click(PLACEHOLDER));
    shell.step(typewrite(&format!("look '{dir}/")));
    shell.update(Message::Action(Action::Complete));
    // Picks "nested dir", filled in with its quote closed.
    shell.update(Message::Action(Action::Complete));
    // The quote is open again after the slash, as if completed alone.
    shell.step(typewrite("/x'"));
    shell.step(press(Key::Enter));
    shell.expect(&format!("{dir}/nested dir/x"));
    // A slash after a file goes after it bare, quote still open.
    shell.step(typewrite(&format!("look '{dir}/")));
    shell.update(Message::Action(Action::Complete));
    shell.update(Message::Action(Action::PickPrevious));
    shell.step(typewrite("/' y"));
    shell.step(press(Key::Enter));
    shell.expect(&format!("{dir}/notes/ | y"));
}

/// A shell without a window, whose interpreter works in a temporary
/// directory with some files and directories.
fn shell_with_files() -> (Shell, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    for file in ["alpha", "alphabet", "beta", "gamma", "README"] {
        std::fs::write(dir.path().join(file), "").unwrap();
    }
    std::fs::create_dir(dir.path().join("delta")).unwrap();
    let interpreter = interpreter();
    interpreter.set_cwd(dir.path());
    let (shell, _) = Shell::new(
        Some(Arc::new(interpreter)),
        None,
        Limits::default(),
        Arc::new(gui_config()),
        Level::Info,
        None,
    );
    (shell, dir)
}

/// Types `text` into the shell's prompt.
fn type_into(shell: &mut Shell, text: &str) {
    for c in text.chars() {
        let insert = text_editor::Action::Edit(Edit::Insert(c));
        let _ = shell.update(Message::PromptEdited(insert));
    }
}

/// Presses Backspace in the shell's prompt.
fn backspace(shell: &mut Shell) {
    let backspace = text_editor::Action::Edit(Edit::Backspace);
    let _ = shell.update(Message::PromptEdited(backspace));
}

fn act(shell: &mut Shell, action: Action) {
    let _ = shell.update(Message::Action(action));
}

/// The candidates listed, and the picked one.
fn listed(shell: &Shell) -> (Vec<&str>, Option<&str>) {
    let offer = shell.offer.as_ref().expect("an offer");
    let shown = |index: usize| offer.choices[index].shown.as_str();
    let listed = offer.matches.iter().map(|m| shown(m.index)).collect();
    (listed, offer.picked.map(|i| shown(offer.matches[i].index)))
}

#[test]
fn typing_while_picking_searches_the_candidates() {
    let (mut shell, _dir) = shell_with_files();
    type_into(&mut shell, "look ");
    act(&mut shell, Action::Complete);
    act(&mut shell, Action::Complete);
    assert_eq!(shell.prompt.text(), "look README");
    // Ranked: the adjacent match first.
    type_into(&mut shell, "lt");
    assert_eq!(listed(&shell), (vec!["delta/", "alphabet"], Some("delta/")));
    assert_eq!(shell.offer.as_ref().unwrap().matches[0].positions, [2, 3]);
    assert_eq!(shell.prompt.text(), "look delta");
    // Tab goes through the matches only.
    act(&mut shell, Action::Complete);
    assert_eq!(shell.prompt.text(), "look alphabet");
    act(&mut shell, Action::Complete);
    assert_eq!(shell.prompt.text(), "look delta");
    // Backspace takes letters back, picking the best match again.
    backspace(&mut shell);
    assert_eq!(listed(&shell).0, ["alpha", "alphabet", "delta/"]);
    assert_eq!(shell.prompt.text(), "look alpha");
    backspace(&mut shell);
    assert_eq!(listed(&shell).0.len(), 6);
    // Then it goes back to the line from before, like Escape.
    backspace(&mut shell);
    assert_eq!(shell.prompt.text(), "look ");
    assert!(!shell.offer.as_ref().unwrap().picking());
    // Then it edits the line, ending the offer.
    backspace(&mut shell);
    assert_eq!(shell.prompt.text(), "look");
    assert!(shell.offer.is_none());
}

#[test]
fn searching_ignores_case_in_lowercase() {
    let (mut shell, _dir) = shell_with_files();
    type_into(&mut shell, "look ");
    act(&mut shell, Action::Complete);
    act(&mut shell, Action::PickPrevious);
    type_into(&mut shell, "rdm");
    assert_eq!(listed(&shell), (vec!["README"], Some("README")));
}

#[test]
fn other_keys_take_the_searched_candidate() {
    let (mut shell, _dir) = shell_with_files();
    type_into(&mut shell, "look ");
    act(&mut shell, Action::Complete);
    act(&mut shell, Action::Complete);
    type_into(&mut shell, "lt ");
    assert_eq!(shell.prompt.text(), "look delta ");
    assert!(shell.offer.is_none());
    // The delimiter goes on into the directory.
    act(&mut shell, Action::Complete);
    act(&mut shell, Action::Complete);
    type_into(&mut shell, "lt/");
    assert_eq!(shell.prompt.text(), "look delta delta/");
    assert!(shell.offer.is_none());
}

#[test]
fn shift_space_takes_the_candidate_bare_and_cycles_its_endings() {
    let (mut shell, _dir) = shell_with_files();
    let shift_space = |shell: &mut Shell| {
        act(shell, Action::TakePick);
        shell.prompt.text()
    };
    type_into(&mut shell, "look ");
    act(&mut shell, Action::Complete);
    act(&mut shell, Action::Complete);
    type_into(&mut shell, "lt");
    // Bare, with the delimiter, finished with a space, and around again.
    assert_eq!(shift_space(&mut shell), "look delta");
    assert!(shell.offer.is_none());
    assert_eq!(shift_space(&mut shell), "look delta/");
    assert_eq!(shift_space(&mut shell), "look delta ");
    assert_eq!(shift_space(&mut shell), "look delta");
    // Without a delimiter, the quote is closed with the space.
    shell.set_prompt("look 'al");
    act(&mut shell, Action::Complete);
    act(&mut shell, Action::Complete);
    assert_eq!(shift_space(&mut shell), "look 'alpha");
    assert_eq!(shift_space(&mut shell), "look 'alpha' ");
    assert_eq!(shift_space(&mut shell), "look 'alpha");
    // Otherwise, it types a space.
    type_into(&mut shell, "x");
    assert_eq!(shift_space(&mut shell), "look 'alphax ");
}

#[test]
fn the_cursor_is_before_the_picked_candidates_terminator() {
    let (mut shell, _dir) = shell_with_files();
    let column = |shell: &Shell| shell.prompt.cursor().position.column;
    type_into(&mut shell, "look 'al");
    act(&mut shell, Action::Complete);
    act(&mut shell, Action::Complete);
    assert_eq!(shell.prompt.text(), "look 'alpha'");
    assert_eq!(column(&shell), "look 'alpha".len());
    act(&mut shell, Action::Complete);
    assert_eq!(column(&shell), "look 'alphabet".len());
    type_into(&mut shell, "a");
    assert_eq!(column(&shell), "look 'alpha".len());
    // Without a quote, it's at the end.
    shell.offer = None;
    shell.set_prompt("look al");
    act(&mut shell, Action::Complete);
    act(&mut shell, Action::Complete);
    assert_eq!(column(&shell), "look alpha".len());
}

#[test]
fn other_text_goes_after_the_picked_candidate_bare() {
    let (mut shell, _dir) = shell_with_files();
    type_into(&mut shell, "look 'al");
    act(&mut shell, Action::Complete);
    act(&mut shell, Action::Complete);
    type_into(&mut shell, ".");
    assert_eq!(shell.prompt.text(), "look 'alpha.");
    assert!(shell.offer.is_none());
    // No cycling.
    act(&mut shell, Action::TakePick);
    assert_eq!(shell.prompt.text(), "look 'alpha. ");
    // Pasting too.
    shell.set_prompt("look 'al");
    act(&mut shell, Action::Complete);
    act(&mut shell, Action::Complete);
    let paste = text_editor::Action::Edit(Edit::Paste(Arc::new("-x".to_owned())));
    let _ = shell.update(Message::PromptEdited(paste));
    assert_eq!(shell.prompt.text(), "look 'alpha-x");
}

#[test]
fn shift_space_cycles_the_endings_of_other_ways_of_taking_a_candidate() {
    let (mut shell, _dir) = shell_with_files();
    let shift_space = |shell: &mut Shell| {
        act(shell, Action::TakePick);
        shell.prompt.text()
    };
    // Space.
    type_into(&mut shell, "look 'al");
    act(&mut shell, Action::Complete);
    act(&mut shell, Action::Complete);
    type_into(&mut shell, " ");
    assert_eq!(shell.prompt.text(), "look 'alpha' ");
    assert_eq!(shift_space(&mut shell), "look 'alpha");
    // The delimiter.
    shell.set_prompt("look ");
    act(&mut shell, Action::Complete);
    act(&mut shell, Action::Complete);
    type_into(&mut shell, "lt/");
    assert_eq!(shift_space(&mut shell), "look delta ");
    // Tab, with only one candidate.
    shell.set_prompt("look 'gam");
    act(&mut shell, Action::Complete);
    assert_eq!(shell.prompt.text(), "look 'gamma' ");
    assert_eq!(shift_space(&mut shell), "look 'gamma");
    shell.set_prompt("look de");
    act(&mut shell, Action::Complete);
    assert_eq!(shell.prompt.text(), "look delta/");
    // Tab with nothing to complete (delta is empty) doesn't stop it.
    act(&mut shell, Action::Complete);
    assert_eq!(shell.prompt.text(), "look delta/");
    assert_eq!(shift_space(&mut shell), "look delta ");
    // Moving the cursor stops it.
    let left = text_editor::Action::Move(Motion::Left);
    let _ = shell.update(Message::PromptEdited(left));
    assert_eq!(shift_space(&mut shell), "look delta  ");
}

#[test]
fn a_search_without_matches_types_the_query() {
    let (mut shell, _dir) = shell_with_files();
    type_into(&mut shell, "look ");
    act(&mut shell, Action::Complete);
    act(&mut shell, Action::Complete);
    type_into(&mut shell, "lxq");
    assert_eq!(listed(&shell), (vec![], None));
    assert_eq!(shell.prompt.text(), "look lxq");
    // Tab has nothing to pick.
    act(&mut shell, Action::Complete);
    assert_eq!(shell.prompt.text(), "look lxq");
    // Backspace brings matches back.
    backspace(&mut shell);
    backspace(&mut shell);
    assert_eq!(listed(&shell).1, Some("alpha"));
    // Space keeps the query.
    type_into(&mut shell, "xq ");
    assert_eq!(shell.prompt.text(), "look lxq ");
    assert!(shell.offer.is_none());
}

#[test]
fn escape_after_searching_restores_the_line_and_the_list() {
    let (mut shell, _dir) = shell_with_files();
    type_into(&mut shell, "look ");
    act(&mut shell, Action::Complete);
    act(&mut shell, Action::Complete);
    type_into(&mut shell, "lt");
    act(&mut shell, Action::CancelPick);
    assert_eq!(shell.prompt.text(), "look ");
    assert_eq!(listed(&shell).0.len(), 6);
    assert_eq!(listed(&shell).1, None);
    // Letters are typed again.
    type_into(&mut shell, "b");
    assert_eq!(shell.prompt.text(), "look b");
    assert!(shell.offer.is_none());
}

#[test]
fn escape_restores_the_line_while_nothing_has_focus() {
    let mut shell = start(None);
    shell.run("say hi");
    shell.expect("hi");
    shell.step(click(PLACEHOLDER));
    shell.step(typewrite("s"));
    shell.update(Message::Action(Action::Complete));
    shell.update(Message::Action(Action::Complete));
    shell.step(click("> say hi"));
    shell.settle();
    // The emulator doesn't pass events to subscriptions.
    let escape = keyboard::Key::Named(keyboard::key::Named::Escape);
    shell.update(Message::Key(escape, Modifiers::empty()));
    // The prompt has focus again.
    shell.step(typewrite("ay again"));
    shell.step(press(Key::Enter));
    shell.expect("> say again");
}

#[test]
fn the_command_line_has_one_line() {
    let mut shell = start(None);
    shell.step(click(PLACEHOLDER));
    // Shift+Enter would insert a line break, but the prompt drops it.
    shell.update(Message::PromptEdited(text_editor::Action::Edit(
        Edit::Enter,
    )));
    let paste = "\nsay one\r\ntwo\nthree\n";
    shell.update(Message::PromptEdited(text_editor::Action::Edit(
        Edit::Paste(Arc::new(paste.to_owned())),
    )));
    shell.step(press(Key::Enter));
    // Not private: the leading line break was dropped.
    shell.expect("> say one two three");
}

#[test]
fn history_puts_the_cursor_at_the_end() {
    let mut shell = start(None);
    shell.run("say one");
    shell.expect("one");
    shell.update(Message::Action(Action::HistoryBack));
    shell.step(typewrite(" two"));
    shell.step(press(Key::Enter));
    shell.expect("one two");
}

#[test]
fn prompt_keys() {
    use keyboard::key::{Code, Named, Physical};
    use text_editor::Binding as B;

    let bindings = keys::defaults();
    let press = |named, code, modifiers, focused| {
        let status = match focused {
            true => text_editor::Status::Focused { is_hovered: false },
            false => text_editor::Status::Active,
        };
        prompt_binding(
            &bindings,
            KeyPress {
                key: keyboard::Key::Named(named),
                modified_key: keyboard::Key::Named(named),
                physical_key: Physical::Code(code),
                modifiers,
                text: None,
                status,
            },
        )
    };
    let key = |named, code, modifiers| press(named, code, modifiers, true);
    let none = Modifiers::empty();

    let enter = key(Named::Enter, Code::Enter, none);
    assert!(matches!(enter, Some(B::Custom(Message::PromptSubmitted))));
    let shift_enter = key(Named::Enter, Code::Enter, Modifiers::SHIFT);
    assert!(shift_enter.is_none());
    // Up and Down browse the history.
    let up = key(Named::ArrowUp, Code::ArrowUp, none);
    assert!(matches!(
        up,
        Some(B::Custom(Message::Action(Action::HistoryBack)))
    ));
    let down = key(Named::ArrowDown, Code::ArrowDown, none);
    assert!(matches!(
        down,
        Some(B::Custom(Message::Action(Action::HistoryForward)))
    ));
    let tab = key(Named::Tab, Code::Tab, none);
    assert!(matches!(
        tab,
        Some(B::Custom(Message::Action(Action::Complete)))
    ));
    // The shell decides what Escape does.
    let escape = key(Named::Escape, Code::Escape, none);
    assert!(matches!(
        escape,
        Some(B::Custom(Message::Action(Action::CancelPick)))
    ));
    // And Shift+Space (it types a space unless a candidate is picked).
    let shift_space = key(Named::Space, Code::Space, Modifiers::SHIFT);
    assert!(matches!(
        shift_space,
        Some(B::Custom(Message::Action(Action::TakePick)))
    ));
    // The shell scrolls.
    assert!(key(Named::PageUp, Code::PageUp, none).is_none());
    // Nothing while unfocused.
    assert!(press(Named::Enter, Code::Enter, none, false).is_none());
}

#[test]
fn escape_unfocuses_and_a_click_on_the_icon_focuses() {
    let mut shell = start(None);
    shell.step(click(PLACEHOLDER));
    shell.step(press(Key::Escape));
    shell.settle();
    // Typing while nothing has focus goes through a subscription, which the
    // emulator doesn't run, so this would be lost if the prompt didn't have
    // focus.
    shell.step(click(">"));
    shell.step(typewrite("say hi"));
    shell.step(press(Key::Enter));
    shell.expect("> say hi");
}

#[test]
fn ctrl_d_works_while_nothing_has_focus() {
    let mut shell = start(None);
    shell.run("greet");
    shell.expect("OK");
    shell.settle();
    shell.step(click("> greet"));
    shell.settle();
    let ctrl_d = Message::Key(keyboard::Key::Character("d".into()), Modifiers::CTRL);
    shell.update(ctrl_d);
    shell.expect("warning: Not exiting: greet is running");
}

#[test]
fn escape_cancels_an_island() {
    let mut shell = start(None);
    shell.run("greet");
    shell.expect("OK");
    shell.settle();
    shell.step(typewrite("Ann"));
    shell.step(press(Key::Escape));
    shell.expect("Not greeting.");
}

#[test]
fn keys_press_buttons() {
    let mut shell = start(None);
    shell.run("ask");
    shell.expect("No [n]");
    shell.settle();
    shell.step(typewrite("n"));
    shell.expect("Answer: no");
}

#[test]
fn ctrl_l_clears_the_scrollback_but_not_running_commands() {
    let (mut shell, _) = Shell::new(
        Some(Arc::new(interpreter())),
        None,
        Limits::default(),
        Arc::new(gui_config()),
        Level::Info,
        None,
    );
    for (line, running) in [("done", false), ("running", true)] {
        let id = shell.next_id();
        shell.scrollback.push(Item::Block(Block {
            id,
            line: line.into(),
            entries: Vec::new(),
            running,
            interrupted: false,
        }));
    }
    let ctrl_l = Message::Key(keyboard::Key::Character("l".into()), Modifiers::CTRL);
    let _ = shell.update(ctrl_l);
    let lines: Vec<_> = (shell.scrollback.iter())
        .map(|item| match item {
            Item::Block(block) => block.line.as_str(),
            Item::Notice(_) => "notice",
        })
        .collect();
    assert_eq!(lines, ["running"]);
}

#[test]
fn ctrl_d_does_not_exit_while_commands_are_running() {
    let (mut shell, _) = Shell::new(
        Some(Arc::new(interpreter())),
        None,
        Limits::default(),
        Arc::new(gui_config()),
        Level::Info,
        None,
    );
    let mut notices = Vec::new();
    for line in ["one", "two", "three"] {
        let id = shell.next_id();
        shell.scrollback.push(Item::Block(Block {
            id,
            line: line.into(),
            entries: Vec::new(),
            running: true,
            interrupted: false,
        }));
        let _ = shell.update(Message::Action(Action::Exit));
        let Some(Item::Notice(record)) = shell.scrollback.last() else {
            panic!("no notice");
        };
        notices.push(record.message.clone());
    }
    assert_eq!(
        notices,
        [
            "Not exiting: one is running",
            "Not exiting: two and 1 other command are running",
            "Not exiting: three and 2 other commands are running",
        ]
    );
}

#[test]
fn exit_asks_first_if_commands_are_running() {
    let mut shell = start(None);
    shell.run("greet");
    shell.expect("OK");
    // Let the greeting take focus first, so it doesn't take "exit".
    shell.settle();
    shell.run("exit");
    shell.expect("These commands are still running:");
    shell.expect("greet");
    shell.settle();
    shell.step(typewrite("n"));
    shell.expect("Not exiting.");
}

#[test]
fn clear_clears_the_scrollback() {
    let mut shell = start(None);
    shell.run("say hi");
    shell.expect("hi");
    shell.run("clear");
    shell.expect_absent("hi");
    shell.expect_absent("> clear");
}

#[test]
fn session_is_restored_and_saved() {
    let file = ScratchFile::new("session");
    let island = IslandBuilder::new(label("A frozen label"))
        .freeze()
        .unwrap();
    let session = Session {
        name: Some("hob".into()),
        history: vec!["old".into()],
        scrollback: vec![hob_core::Item::Block(hob_core::Block {
            line: "old".into(),
            entries: vec![
                hob_core::Entry::Text("Old output".into()),
                hob_core::Entry::Island(island),
            ],
            interrupted: false,
        })],
    };
    session.save(&file.0).unwrap();

    let mut shell = start(Some(file.0.clone()));
    shell.expect("> old");
    shell.expect("Old output");
    shell.expect("A frozen label");

    // The trailing space is trimmed.
    shell.run("say new ");
    let saved = Session::load(&file.0).unwrap().unwrap();
    assert_eq!(saved.history, ["old", "say new"]);
    let lines: Vec<_> = (saved.scrollback.iter())
        .filter_map(|item| match item {
            hob_core::Item::Block(block) => Some(block.line.as_str()),
            hob_core::Item::Notice(_) => None,
        })
        .collect();
    assert_eq!(lines, ["old", "say new"]);
}

#[test]
fn a_session_from_another_program_is_warned_about_on_the_next_save() {
    let file = ScratchFile::new("other-name");
    let session = Session {
        name: Some("kettle".into()),
        history: vec!["old".into()],
        ..Session::default()
    };
    session.save(&file.0).unwrap();
    let before = std::fs::read(&file.0).unwrap();

    let mut shell = start(Some(file.0.clone()));
    shell.expect("warning: a session from kettle was loaded into hob");
    shell.settle();
    // Loading alone doesn't save it.
    assert_eq!(std::fs::read(&file.0).unwrap(), before);

    shell.run("say new");
    let saved = Session::load(&file.0).unwrap().unwrap();
    assert_eq!(saved.name.as_deref(), Some("hob"));
    let warned = saved.scrollback.iter().any(|item| {
        matches!(item, hob_core::Item::Notice(r)
            if r.level == Level::Warn && r.message.contains("from kettle"))
    });
    assert!(warned);
}

#[test]
fn a_session_without_a_name_is_not_warned_about() {
    let file = ScratchFile::new("no-name");
    let session = Session {
        history: vec!["old".into()],
        ..Session::default()
    };
    session.save(&file.0).unwrap();
    let (shell, _) = Shell::new(
        Some(Arc::new(interpreter())),
        Some(file.0.clone()),
        Limits::default(),
        Arc::new(gui_config()),
        Level::Info,
        None,
    );
    assert!(shell.scrollback.is_empty());
}

#[test]
fn private_lines_are_not_saved() {
    let file = ScratchFile::new("private");
    let mut shell = start(Some(file.0.clone()));
    shell.run("say public");
    shell.run(" say secret");
    shell.expect("secret");
    shell.expect(PRIVATE_NOTE);
    let saved = Session::load(&file.0).unwrap().unwrap();
    assert_eq!(saved.history, ["say public"]);
    assert_eq!(saved.scrollback.len(), 1);
}

#[test]
fn saving_trims_the_scrollback_and_history() {
    let file = ScratchFile::new("trim");
    let picture = |shade: u8| {
        let pixels = [shade, 0, 0, 255].repeat(16);
        kettle_ui::Image::from_rgba(4, 4, &pixels, "picture").unwrap()
    };
    let (a, b) = (picture(10), picture(20));
    let limits = Limits {
        history: 4,
        scrollback: 5000,
        images: b.data().unwrap().bytes().len() * 2,
    };
    let (mut shell, _) = Shell::new(
        Some(Arc::new(interpreter())),
        Some(file.0.clone()),
        limits,
        Arc::new(gui_config()),
        Level::Info,
        None,
    );
    let big = Entry::Text("x".repeat(10_000));
    let image_entry = |image: &kettle_ui::Image| {
        let mut builder = IslandBuilder::new(kettle_ui::widget::image("x"));
        builder.set_state("x", image.clone());
        Entry::Frozen(builder.freeze().unwrap())
    };
    let blocks = [
        ("running", true, vec![Entry::Text("x".repeat(10_000))]),
        ("old", false, vec![big]),
        (" secret", false, Vec::new()),
        ("pic a", false, vec![image_entry(&a)]),
        ("pic b", false, vec![image_entry(&b)]),
        ("new", false, Vec::new()),
    ];
    for (line, running, entries) in blocks {
        shell.history.push(line);
        let id = shell.next_id();
        shell.scrollback.push(Item::Block(Block {
            id,
            line: line.into(),
            entries,
            running,
            interrupted: false,
        }));
    }
    shell.save();

    // Too big: "old", and the private block after it. The running block
    // stays, since its command still uses it.
    let lines: Vec<_> = (shell.scrollback.iter())
        .map(|item| match item {
            Item::Block(block) => block.line.as_str(),
            Item::Notice(_) => "notice",
        })
        .collect();
    assert_eq!(lines, ["running", "pic a", "pic b", "new"]);
    // The history lost its oldest lines, and the private line with its
    // block.
    assert_eq!(shell.history.saved_lines(), ["pic a", "pic b", "new"]);
    assert_eq!(shell.history.back(""), Some("new"));
    assert_eq!(shell.history.back(""), Some("pic b"));
    assert_eq!(shell.history.back(""), Some("pic a"));
    assert_eq!(shell.history.back(""), None);
    // Only one image fits.
    let image_data = |item: &Item| match item {
        Item::Block(Block { entries, .. }) => match &entries[..] {
            [Entry::Frozen(island)] => island.images().next().unwrap().data().cloned(),
            _ => panic!("expected an island"),
        },
        Item::Notice(_) => panic!("expected a block"),
    };
    assert_eq!(image_data(&shell.scrollback[1]), None);
    assert_eq!(image_data(&shell.scrollback[2]).as_ref(), b.data());

    let saved = Session::load(&file.0).unwrap().unwrap();
    assert_eq!(saved.history, ["pic a", "pic b", "new"]);
    assert_eq!(saved.scrollback.len(), 4);
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&file.0).unwrap()).unwrap();
    let table = json["images"].as_object().unwrap();
    assert_eq!(table.keys().collect::<Vec<_>>(), [&b.key().to_string()]);
}

#[test]
fn preview_shows_the_session_without_changing_it() {
    let file = ScratchFile::new("preview");
    let session = Session {
        name: Some("kettle".into()),
        history: vec!["old".into()],
        scrollback: vec![hob_core::Item::Block(hob_core::Block {
            line: "old".into(),
            entries: vec![hob_core::Entry::Text("Old output".into())],
            interrupted: true,
        })],
    };
    session.save(&file.0).unwrap();
    let before = std::fs::read(&file.0).unwrap();

    let program = application(
        None,
        Some(file.0.clone()),
        Limits::default(),
        gui_config(),
        Level::Info,
        None,
    );
    let mut shell = Harness::new(program);
    shell.expect("> old");
    shell.expect("Old output");
    shell.expect("Interrupted: kettle exited while this command was running.");
    shell.expect(&format!("Read-only preview of {}", file.0.display()));
    // There's no prompt to type into.
    shell.settle();
    shell.step(typewrite("say new"));
    shell.step(press(Key::Enter));
    shell.settle();
    shell.expect_absent("> say new");
    assert_eq!(std::fs::read(&file.0).unwrap(), before);

    // Ctrl+L doesn't clear, and closing doesn't save.
    let (mut shell, _) = Shell::new(
        None,
        Some(file.0.clone()),
        Limits::default(),
        Arc::new(gui_config()),
        Level::Info,
        None,
    );
    let ctrl_l = Message::Key(keyboard::Key::Character("l".into()), Modifiers::CTRL);
    let _ = shell.update(ctrl_l);
    assert_eq!(shell.scrollback.len(), 1);
    let _ = shell.update(Message::CloseRequested);
    assert_eq!(std::fs::read(&file.0).unwrap(), before);
}

#[test]
fn preview_leaves_a_bad_file_alone() {
    let file = ScratchFile::new("preview-bad");
    std::fs::write(&file.0, "not json").unwrap();
    let (shell, _) = Shell::new(
        None,
        Some(file.0.clone()),
        Limits::default(),
        Arc::new(gui_config()),
        Level::Info,
        None,
    );
    assert!(matches!(&shell.scrollback[..], [Item::Notice(r)] if r.level == Level::Error));
    assert_eq!(std::fs::read(&file.0).unwrap(), b"not json");
}

/// Shown in the empty prompt.
const PLACEHOLDER: &str = "Type a command";

/// Shell config for the tests.
fn gui_config() -> GuiConfig {
    GuiConfig {
        title: "test".into(),
        app_id: "org.example.test".into(),
        command_placeholder: PLACEHOLDER.into(),
        log_prefix: "test".into(),
        startup: Vec::new(),
    }
}

/// How long [`Harness::expect`] waits.
const TIMEOUT: Duration = Duration::from_secs(5);

/// An interpreter with test-only commands, and the builtins.
fn interpreter() -> Interpreter {
    let mut interpreter = Interpreter::new();
    hob_core::builtins::register(&mut interpreter);
    interpreter.register("say", "Print the arguments", |ctx, args| {
        ctx.print(args.join(" "));
        Ok(())
    });
    let paths = Spec::new().arg("PATH", Kind::Path).optional().repeated();
    interpreter.register_spec("look", "Print the paths", paths, |ctx, args| {
        ctx.print(args.values("PATH").join(" | "));
        Ok(())
    });
    interpreter.register("sing", "Print a song", |ctx, _| {
        ctx.print("la la");
        Ok(())
    });
    interpreter.register("greet", "Ask for a name and greet", |ctx, _| {
        let island = IslandBuilder::new(column([
            text_field("name", "Your name"),
            row([submit_button("OK"), cancel_button("Cancel")]),
        ]));
        match ctx.show(island)? {
            Outcome::Submitted(fields) => ctx.print(format!("Hello, {}!", fields.text("name"))),
            Outcome::Cancelled | Outcome::Pressed(..) => ctx.print("Not greeting."),
        }
        Ok(())
    });
    interpreter.register("ask", "Ask a yes/no question", |ctx, _| {
        let island = IslandBuilder::new(row([submit_button("Yes [y]"), cancel_button("No [n]")]));
        let answer = match ctx.show(island)? {
            Outcome::Submitted(_) => "yes",
            Outcome::Cancelled | Outcome::Pressed(..) => "no",
        };
        ctx.print(format!("Answer: {answer}"));
        Ok(())
    });
    interpreter
}

/// Runs the shell in an [`Emulator`]. The emulator doesn't wait for tasks
/// ([`Mode::Immediate`]), since commands waiting for an island never finish
/// by themselves; instead, expectations are retried until they hold.
struct Harness<P: Program + 'static> {
    program: P,
    emulator: Emulator<P>,
    events: mpsc::Receiver<Event<P>>,
}

/// Starts the shell in a [`Harness`], with [`interpreter`]'s commands and
/// the given session file.
fn start(session: Option<PathBuf>) -> Harness<impl Program<Message = Message> + 'static> {
    start_with(session, Limits::default())
}

/// Like [`start`], with the session trimmed to `limits`.
fn start_with(
    session: Option<PathBuf>,
    limits: Limits,
) -> Harness<impl Program<Message = Message> + 'static> {
    let program = application(
        Some(Arc::new(interpreter())),
        session,
        limits,
        gui_config(),
        Level::Info,
        None,
    );
    Harness::new(program)
}

impl<P: Program + 'static> Harness<P> {
    fn new(program: P) -> Self {
        let (sender, events) = mpsc::channel(64);
        let emulator = Emulator::new(sender, &program, Mode::Immediate, Size::new(640.0, 560.0));
        let mut harness = Harness {
            program,
            emulator,
            events,
        };
        harness.wait().expect("the emulator failed to boot");
        harness
    }

    /// Types a command line into the empty prompt, and submits it.
    fn run(&mut self, line: &str) {
        // The prompt is focused on start, but the focus task may not have
        // run yet, since the emulator doesn't wait for tasks.
        self.step(click(PLACEHOLDER));
        self.step(typewrite(line));
        self.step(Instruction::Interact(Interaction::Keyboard(
            Keyboard::Type(Key::Enter),
        )));
    }

    /// Sends a message to the program, as if a widget had produced it.
    fn update(&mut self, message: P::Message) {
        self.emulator.update(&self.program, message);
        self.settle();
    }

    /// Runs an instruction, and panics if it fails.
    fn step(&mut self, instruction: Instruction) {
        self.emulator.run(&self.program, instruction);
        if let Err(failed) = self.wait() {
            panic!("failed: {failed}");
        }
    }

    /// Waits until `text` is shown.
    fn expect(&mut self, text: &str) {
        self.poll(text, true);
    }

    /// Waits until `text` isn't shown.
    fn expect_absent(&mut self, text: &str) {
        self.poll(text, false);
    }

    /// Checks whether `text` is shown, until it's `shown`, or panics after
    /// [`TIMEOUT`].
    fn poll(&mut self, text: &str, shown: bool) {
        let start = Instant::now();
        let instruction = Instruction::Expect(Expectation::Text(text.into()));
        loop {
            self.emulator.run(&self.program, instruction.clone());
            if self.wait().is_ok() == shown {
                return;
            }
            if start.elapsed() > TIMEOUT {
                let not = if shown { "" } else { "not " };
                panic!("timed out waiting for {text:?} to be {not}shown");
            }
            // Let commands and tasks make progress.
            self.pump(Duration::from_millis(5));
        }
    }

    /// Performs actions until the emulator is ready, or an instruction
    /// failed.
    fn wait(&mut self) -> Result<(), Instruction> {
        let start = Instant::now();
        loop {
            match self.next_event() {
                Some(Event::Action(action)) => self.emulator.perform(&self.program, action),
                Some(Event::Ready) => return Ok(()),
                Some(Event::Failed(instruction)) => return Err(instruction),
                None if start.elapsed() > TIMEOUT => panic!("the emulator is stuck"),
                None => thread::sleep(Duration::from_millis(1)),
            }
        }
    }

    /// Performs actions until there have been none for a while, e.g. so
    /// that focus has moved.
    fn settle(&mut self) {
        let start = Instant::now();
        let mut last = Instant::now();
        while last.elapsed() < Duration::from_millis(50) {
            assert!(start.elapsed() < TIMEOUT, "never settled");
            match self.next_event() {
                Some(Event::Action(action)) => {
                    self.emulator.perform(&self.program, action);
                    last = Instant::now();
                }
                Some(Event::Ready | Event::Failed(_)) => {}
                None => thread::sleep(Duration::from_millis(1)),
            }
        }
    }

    /// Performs actions for `duration`.
    fn pump(&mut self, duration: Duration) {
        let start = Instant::now();
        while start.elapsed() < duration {
            match self.next_event() {
                Some(Event::Action(action)) => self.emulator.perform(&self.program, action),
                Some(Event::Ready | Event::Failed(_)) => {}
                None => thread::sleep(Duration::from_millis(1)),
            }
        }
    }

    /// The next event, if there is one already.
    fn next_event(&mut self) -> Option<Event<P>> {
        match self.events.try_recv() {
            Ok(event) => Some(event),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Closed) => panic!("the emulator stopped"),
        }
    }
}

/// A left click on the element showing `text`.
fn click(text: &str) -> Instruction {
    Instruction::Interact(Interaction::Mouse(Mouse::Click {
        button: mouse::Button::Left,
        target: Some(Target::Text(text.into())),
    }))
}

/// Pressing and releasing `key`.
fn press(key: Key) -> Instruction {
    Instruction::Interact(Interaction::Keyboard(Keyboard::Type(key)))
}

/// Typing `text` into the focused element.
fn typewrite(text: &str) -> Instruction {
    Instruction::Interact(Interaction::Keyboard(Keyboard::Typewrite(text.into())))
}

/// A path for a temporary file, removed on drop.
struct ScratchFile(PathBuf);

impl ScratchFile {
    fn new(name: &str) -> Self {
        let name = format!("hob-gui-test-{}-{name}.json", std::process::id());
        ScratchFile(std::env::temp_dir().join(name))
    }
}

impl Drop for ScratchFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
