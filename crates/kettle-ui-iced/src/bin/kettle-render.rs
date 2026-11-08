//! Renders a frozen island, saved as JSON, to a PNG (see
//! [`kettle_ui_iced::render_png`]).
//!
//! Usage: `kettle-render [--frozen] [--width N] [--theme NAME] ISLAND.json OUT.png`
//!
//! `NAME` is one of iced's built-in themes, e.g. `dark` or `tokyo-night`
//! (case, spaces and hyphens don't matter); the default is `light`.

use std::process::ExitCode;

use iced::Theme;
use kettle_ui::FrozenIsland;

/// The program name that starts lines logged to stderr.
const LOG_PREFIX: &str = "kettle-render";

const USAGE: &str =
    "usage: kettle-render [--frozen] [--width N] [--theme NAME] ISLAND.json OUT.png";

/// About the width of an island in hob's default window.
const DEFAULT_WIDTH: u32 = 560;

fn main() -> ExitCode {
    let mut frozen = false;
    let mut width = DEFAULT_WIDTH;
    let mut theme = Theme::Light;
    let mut paths = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--frozen" => frozen = true,
            "--width" => match args.next().and_then(|w| w.parse().ok()) {
                Some(w) if w > 0 => width = w,
                _ => return usage(),
            },
            "--theme" => match args.next().map(|name| find_theme(&name)) {
                Some(Ok(t)) => theme = t,
                Some(Err(e)) => {
                    eprintln!("{LOG_PREFIX}: {e}");
                    return ExitCode::from(2);
                }
                None => return usage(),
            },
            _ if arg.starts_with('-') => return usage(),
            _ => paths.push(arg),
        }
    }
    let [input, output] = paths.as_slice() else {
        return usage();
    };
    match run(input, output, &theme, width, frozen) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{LOG_PREFIX}: {e}");
            ExitCode::FAILURE
        }
    }
}

fn usage() -> ExitCode {
    eprintln!("{USAGE}");
    ExitCode::from(2)
}

/// Finds a built-in theme by name, ignoring case, spaces and hyphens.
fn find_theme(name: &str) -> Result<Theme, String> {
    let key = |s: &str| {
        s.chars()
            .filter(|c| !matches!(c, ' ' | '-' | '_'))
            .flat_map(char::to_lowercase)
            .collect::<String>()
    };
    Theme::ALL
        .iter()
        .find(|t| key(&t.to_string()) == key(name))
        .cloned()
        .ok_or_else(|| {
            let names: Vec<_> = Theme::ALL.iter().map(Theme::to_string).collect();
            format!("unknown theme {name:?}; known: {}", names.join(", "))
        })
}

fn run(input: &str, output: &str, theme: &Theme, width: u32, frozen: bool) -> Result<(), String> {
    let json = std::fs::read_to_string(input).map_err(|e| format!("{input}: {e}"))?;
    let island: FrozenIsland = serde_json::from_str(&json).map_err(|e| format!("{input}: {e}"))?;
    let png = kettle_ui_iced::render_png(&island, theme, width, frozen);
    std::fs::write(output, png).map_err(|e| format!("{output}: {e}"))
}
