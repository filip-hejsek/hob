//! Tests, including end-to-end tests that run the shell in `iced_test`'s
//! [`Emulator`], with test-only commands.

use std::thread;
use std::time::{Duration, Instant};

use futures_channel::mpsc;
use iced::{Size, mouse};
use iced_test::emulator::{Emulator, Event, Mode};
use iced_test::instruction::{Expectation, Interaction, Key, Keyboard, Mouse, Target};
use iced_test::{Instruction, program::Program};
use kettle_ui::widget::{column, label, submit_button, text_field};
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

/// Shown in the empty prompt.
const PLACEHOLDER: &str = "Type a command";

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
    interpreter.register("greet", "Ask for a name and greet", |ctx, _| {
        let island = IslandBuilder::new(column([
            text_field("name", "Your name"),
            submit_button("OK"),
        ]));
        if let Outcome::Submitted(fields) = ctx.show(island)? {
            ctx.print(format!("Hello, {}!", fields.text("name")));
        }
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
fn start(session: Option<PathBuf>) -> Harness<impl Program + 'static> {
    let program = application(
        Arc::new(interpreter()),
        session,
        PLACEHOLDER.into(),
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
