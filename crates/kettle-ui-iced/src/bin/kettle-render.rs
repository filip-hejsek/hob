//! Renders a frozen island, saved as JSON, to a PNG (see
//! [`kettle_ui_iced::render_png`]).
//!
//! Usage: `kettle-render [--frozen] [--width N] ISLAND.json OUT.png`

use std::process::ExitCode;

use kettle_ui::FrozenIsland;

const USAGE: &str = "usage: kettle-render [--frozen] [--width N] ISLAND.json OUT.png";

/// About the width of an island in hob's default window.
const DEFAULT_WIDTH: u32 = 560;

fn main() -> ExitCode {
    let mut frozen = false;
    let mut width = DEFAULT_WIDTH;
    let mut paths = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--frozen" => frozen = true,
            "--width" => match args.next().and_then(|w| w.parse().ok()) {
                Some(w) if w > 0 => width = w,
                _ => return usage(),
            },
            _ if arg.starts_with('-') => return usage(),
            _ => paths.push(arg),
        }
    }
    let [input, output] = paths.as_slice() else {
        return usage();
    };
    match run(input, output, width, frozen) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("kettle-render: {e}");
            ExitCode::FAILURE
        }
    }
}

fn usage() -> ExitCode {
    eprintln!("{USAGE}");
    ExitCode::from(2)
}

fn run(input: &str, output: &str, width: u32, frozen: bool) -> Result<(), String> {
    let json = std::fs::read_to_string(input).map_err(|e| format!("{input}: {e}"))?;
    let island: FrozenIsland = serde_json::from_str(&json).map_err(|e| format!("{input}: {e}"))?;
    let png = kettle_ui_iced::render_png(&island, width, frozen);
    std::fs::write(output, png).map_err(|e| format!("{output}: {e}"))
}
