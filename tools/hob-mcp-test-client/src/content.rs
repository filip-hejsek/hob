//! Showing what servers return: content blocks (in tool results and prompt
//! messages) and resource contents.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use hob_core::Context;
use kettle_ui::{Image, IslandBuilder};
use rmcp::model::{ContentBlock, ResourceContents};

/// Shows a content block, after `heading` (e.g. "User:") if given. `source`
/// names where it came from, for images' alt text (e.g. "echo").
pub(crate) fn show_block(ctx: &Context, source: &str, heading: Option<&str>, block: &ContentBlock) {
    match block {
        ContentBlock::Text(text) => ctx.print(headed(heading, &text.text)),
        ContentBlock::Image(content) => {
            let alt = format!("{} image from {source}", content.mime_type);
            show_image(ctx, heading, &content.data, &content.mime_type, alt);
        }
        ContentBlock::Audio(audio) => {
            let audio = format!("Audio: {}, {}", audio.mime_type, size(&audio.data));
            ctx.print(headed(heading, &audio));
        }
        ContentBlock::Resource(embedded) => {
            let resource = format!("Resource {}:", uri(&embedded.resource));
            let heading = headed(heading, &resource);
            show_resource(ctx, Some(&heading), &embedded.resource);
        }
        ContentBlock::ResourceLink(resource) => {
            let link = format!("Link to resource {} ({})", resource.uri, resource.name);
            ctx.print(headed(heading, &link));
        }
        _ => ctx.print(headed(heading, "Content of an unknown kind")),
    }
}

/// Shows a resource's contents, after `heading` if given: text as it is,
/// images in an island, and other binary data described.
pub(crate) fn show_resource(ctx: &Context, heading: Option<&str>, contents: &ResourceContents) {
    match contents {
        ResourceContents::TextResourceContents { text, .. } => ctx.print(headed(heading, text)),
        ResourceContents::BlobResourceContents {
            uri,
            mime_type: Some(mime_type),
            blob,
            ..
        } if mime_type.starts_with("image/") => {
            let alt = format!("{mime_type} image {uri}");
            show_image(ctx, heading, blob, mime_type, alt);
        }
        ResourceContents::BlobResourceContents {
            mime_type, blob, ..
        } => {
            let mime_type = mime_type.as_deref().unwrap_or("binary data");
            ctx.print(headed(heading, &format!("{mime_type}, {}", size(blob))));
        }
        _ => ctx.print(headed(heading, "Contents of an unknown kind")),
    }
}

/// The URI of a resource's contents.
pub(crate) fn uri(contents: &ResourceContents) -> &str {
    match contents {
        ResourceContents::TextResourceContents { uri, .. }
        | ResourceContents::BlobResourceContents { uri, .. } => uri,
        _ => "(unknown)",
    }
}

/// Shows base64 image `data` in an island, after `heading` if given.
fn show_image(ctx: &Context, heading: Option<&str>, data: &str, mime_type: &str, alt: String) {
    let image = BASE64.decode(data).map_err(|e| e.to_string());
    let image = image.and_then(|bytes| Image::from_bytes(bytes, alt).map_err(|e| e.to_string()));
    let island = image.and_then(|image| {
        let mut island = IslandBuilder::new(kettle_ui::widget::image("image"));
        island.set_state("image", image);
        island.freeze().map_err(|e| e.to_string())
    });
    match island {
        Ok(island) => {
            if let Some(heading) = heading {
                ctx.print(heading);
            }
            ctx.print_island(island);
        }
        Err(e) => {
            let message = format!("Image ({mime_type}), which can't be shown: {e}");
            ctx.print(headed(heading, &message));
        }
    }
}

/// `text` under `heading`, if given.
fn headed(heading: Option<&str>, text: &str) -> String {
    match heading {
        Some(heading) => format!("{heading}\n{text}"),
        None => text.to_owned(),
    }
}

/// The size of base64 `data`, in bytes.
fn size(data: &str) -> String {
    match BASE64.decode(data) {
        Ok(bytes) => format!("{} bytes", bytes.len()),
        Err(e) => format!("invalid base64 ({e})"),
    }
}
