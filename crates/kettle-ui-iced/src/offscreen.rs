//! Offscreen rendering of an island to a PNG, with [`render_png`].

use iced::advanced::layout::Limits;
use iced::advanced::renderer::{Headless, Renderer as _, Style};
use iced::advanced::widget::Tree;
use iced::advanced::{Layout, Shell, clipboard};
use iced::time::Instant;
use iced::widget::container;
use iced::{Element, Event, Font, Pixels, Rectangle, Renderer, Size, Theme, mouse, window};
use kettle_ui::FrozenIsland;

use crate::{Message, render, spacing};

/// Renders an island offscreen to a PNG in `theme`, `width` logical pixels
/// wide and as tall as it needs, with padding around it. Without `frozen`,
/// widgets look enabled, as in a live island. Uses tiny-skia and the bundled Fira Sans,
/// so the output doesn't depend on the machine.
pub fn render_png(island: &FrozenIsland, theme: &Theme, width: u32, frozen: bool) -> Vec<u8> {
    let (size, rgba) = render_rgba(island, theme, width, frozen);
    let mut png = Vec::new();
    let mut encoder = png::Encoder::new(&mut png, size.width, size.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().expect("writing to a Vec can't fail");
    writer
        .write_image_data(&rgba)
        .expect("the image data fits the header");
    writer.finish().expect("writing to a Vec can't fail");
    png
}

/// Renders an island to RGBA pixels; see [`render_png`].
fn render_rgba(
    island: &FrozenIsland,
    theme: &Theme,
    width: u32,
    frozen: bool,
) -> (Size<u32>, Vec<u8>) {
    let mut element: Element<'_, Message> = container(render(island, island.root(), !frozen, None))
        .padding(spacing())
        .into();
    let mut renderer = futures_executor::block_on(<Renderer as Headless>::new(
        Font::with_name("Fira Sans"),
        Pixels(16.0),
        Some("tiny-skia"),
    ))
    .expect("the tiny-skia renderer is always available");

    let mut tree = Tree::new(&element);
    let limits = Limits::new(Size::ZERO, Size::new(width as f32, f32::INFINITY));
    let node = element
        .as_widget_mut()
        .layout(&mut tree, &renderer, &limits);
    let size = Size::new(width, (node.size().height.ceil() as u32).max(1));
    let viewport = Rectangle::with_size(Size::new(size.width as f32, size.height as f32));

    // Widgets such as buttons pick their status (e.g. disabled) only when
    // they see a redraw request, and draw as disabled until then.
    let mut messages = Vec::new();
    element.as_widget_mut().update(
        &mut tree,
        &Event::Window(window::Event::RedrawRequested(Instant::now())),
        Layout::new(&node),
        mouse::Cursor::Unavailable,
        &renderer,
        &mut clipboard::Null,
        &mut Shell::new(&mut messages),
        &viewport,
    );

    // As in iced_runtime's `UserInterface::draw` (there are no overlays).
    renderer.reset(viewport);
    let style = Style {
        text_color: theme.palette().text,
    };
    element.as_widget().draw(
        &tree,
        &mut renderer,
        theme,
        &style,
        Layout::new(&node),
        mouse::Cursor::Unavailable,
        &viewport,
    );
    let rgba = renderer.screenshot(size, 1.0, theme.palette().background);
    (size, rgba)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use kettle_ui::IslandBuilder;
    use kettle_ui::widget::*;

    use super::*;

    fn island() -> FrozenIsland {
        let mut b = IslandBuilder::new(column([
            text_field("name", "Your name"),
            label("Hello there"),
            checkbox("sub", "Subscribe"),
            progress("p"),
            row([submit_button("OK"), cancel_button("Cancel")]),
        ]));
        b.set_text("name", "Ada");
        b.set_checked("sub", true);
        b.set_state("p", 0.4);
        b.freeze().unwrap()
    }

    /// Compares the rendering with `tests/snapshots/NAME.png`, pixel by
    /// pixel. If the file is missing, writes it instead; delete it to
    /// regenerate it.
    fn check_snapshot(name: &str, frozen: bool) {
        let png = render_png(&island(), &Theme::Light, 300, frozen);
        let path: PathBuf = [env!("CARGO_MANIFEST_DIR"), "tests", "snapshots"]
            .iter()
            .collect::<PathBuf>()
            .join(format!("{name}.png"));
        let Ok(expected) = std::fs::read(&path) else {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &png).unwrap();
            return;
        };
        let (size, pixels) = decode(&png);
        let (expected_size, expected_pixels) = decode(&expected);
        assert_eq!(size, expected_size, "size differs from {}", path.display());
        assert!(
            pixels == expected_pixels,
            "pixels differ from {}",
            path.display()
        );
    }

    fn decode(png: &[u8]) -> ((u32, u32), Vec<u8>) {
        let mut reader = png::Decoder::new(std::io::Cursor::new(png))
            .read_info()
            .unwrap();
        let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut pixels).unwrap();
        pixels.truncate(info.buffer_size());
        ((info.width, info.height), pixels)
    }

    #[test]
    fn live_look_matches_snapshot() {
        check_snapshot("live", false);
    }

    #[test]
    fn frozen_look_matches_snapshot() {
        check_snapshot("frozen", true);
    }

    #[test]
    fn theme_sets_the_background() {
        let background = |theme: &Theme| render_rgba(&island(), theme, 300, false).1[..4].to_vec();
        assert_eq!(background(&Theme::Light), [255, 255, 255, 255]);
        assert_ne!(background(&Theme::Dark), background(&Theme::Light));
    }

    #[test]
    fn text_area_grows_with_its_text() {
        let height = |text: &str| {
            let mut b = IslandBuilder::new(text_area("t", "Notes"));
            b.set_text("t", text);
            render_rgba(&b.freeze().unwrap(), &Theme::Light, 300, true)
                .0
                .height
        };
        let empty = height("");
        assert!(empty > 80, "{empty}");
        assert_eq!(height("1\n2"), empty);
        assert!(height(&"line\n".repeat(10)) > empty + 80);
    }

    /// An island showing a solid red image.
    fn image_island(width: u32, height: u32, with_data: bool) -> FrozenIsland {
        let pixels = [255, 0, 0, 255].repeat((width * height) as usize);
        let red = kettle_ui::Image::from_rgba(width, height, &pixels, "A red box").unwrap();
        let red = if with_data { red } else { red.without_data() };
        let mut b = IslandBuilder::new(image("img"));
        b.set_state("img", red);
        b.freeze().unwrap()
    }

    fn is_red(rgba: &[u8], size: Size<u32>, x: u32, y: u32) -> bool {
        let i = ((y * size.width + x) * 4) as usize;
        rgba[i..i + 4] == [255, 0, 0, 255]
    }

    #[test]
    fn image_is_shown_at_its_size() {
        let (size, rgba) = render_rgba(&image_island(50, 40, true), &Theme::Light, 300, true);
        let pad = spacing() as u32;
        assert_eq!(size.height, 40 + 2 * pad);
        assert!(is_red(&rgba, size, pad, pad));
        assert!(is_red(&rgba, size, pad + 49, pad + 39));
        assert!(!is_red(&rgba, size, pad + 51, pad));
    }

    #[test]
    fn wide_image_is_scaled_to_fit() {
        let (size, rgba) = render_rgba(&image_island(560, 100, true), &Theme::Light, 300, true);
        let pad = spacing() as u32;
        // 280 pixels wide, so half the size.
        assert_eq!(size.height, 50 + 2 * pad);
        assert!(is_red(&rgba, size, pad + 279, pad + 49));
    }

    #[test]
    fn missing_image_shows_a_notice() {
        let (size, rgba) = render_rgba(&image_island(50, 400, false), &Theme::Light, 300, true);
        assert!(size.height < 100, "{size:?}");
        assert!(!rgba.chunks(4).any(|p| p == [255, 0, 0, 255]));
        assert!(rgba.chunks(4).any(|p| p != &rgba[..4]));
    }

    #[test]
    fn height_fits_content() {
        let (size, rgba) = render_rgba(&island(), &Theme::Light, 300, false);
        assert_eq!(size.width, 300);
        assert!(size.height > 100 && size.height < 400, "{size:?}");
        // Not blank: some pixels differ from the first one.
        assert!(rgba.chunks(4).any(|p| p != &rgba[..4]));
    }
}
