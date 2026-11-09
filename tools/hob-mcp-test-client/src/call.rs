//! `call`'s parts: asking for a tool's arguments through a form built from
//! its input schema (or as raw JSON), and showing what the tool returned.

use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use hob_core::{CommandResult, Context};
use kettle_ui::widget::{
    Widget, button, cancel_button, checkbox, column, label, row, state, submit_button, text_area,
    text_field,
};
use kettle_ui::{Fields, Image, Inputs, IslandBuilder, Outcome};
use rmcp::model::{CallToolResult, ContentBlock, ImageContent, JsonObject, ResourceContents, Tool};
use serde_json::Value;

use crate::{BoxError, pretty};

/// One argument in the form: a property of the input schema.
struct Param {
    name: String,
    kind: ParamKind,
    /// The type, as the schema gives it (e.g. "string", "array").
    type_name: String,
    required: bool,
    description: Option<String>,
    default: Option<Value>,
    /// The allowed values (`enum`), if listed.
    choices: Vec<Value>,
}

/// How an argument is entered.
#[derive(Clone, Copy, PartialEq)]
enum ParamKind {
    /// A text field, sent as a string.
    Text,
    /// A text field, sent as a number.
    Number,
    /// A text field, sent as an integer.
    Integer,
    /// A checkbox.
    Boolean,
    /// A text area holding JSON: objects, arrays, unions, or no type.
    Json,
}

/// Asks for the arguments to `tool`. `None` if the user cancelled. A tool
/// without parameters gets none, without asking.
pub(crate) fn ask(ctx: &Context, tool: &Tool) -> Result<Option<JsonObject>, BoxError> {
    let params = Arc::new(params(&tool.input_schema));
    if params.is_empty() {
        return Ok(Some(JsonObject::new()));
    }
    let mut fields = None;
    loop {
        match ctx.show(form(tool, &params, fields.as_ref()))? {
            Outcome::Submitted(submitted) => {
                let (arguments, errors) = arguments(&params, &submitted);
                if errors.is_empty() {
                    return Ok(Some(arguments));
                }
                ctx.error(errors.join("\n"));
                fields = Some(submitted);
            }
            Outcome::Pressed(_, submitted) => {
                let (arguments, _) = arguments(&params, &submitted);
                return ask_json(ctx, &pretty(&arguments));
            }
            Outcome::Cancelled => return Ok(None),
        }
    }
}

/// Asks for the arguments as a JSON object, starting with `text`.
fn ask_json(ctx: &Context, text: &str) -> Result<Option<JsonObject>, BoxError> {
    let mut text = text.to_owned();
    loop {
        let mut island = IslandBuilder::new(column([
            label("Arguments, as a JSON object:"),
            text_area("json", "{}"),
            label(state("problem")),
            row([submit_button("Call"), cancel_button("Cancel")]),
        ]));
        island.set_text("json", &text);
        island.derive("problem", |inputs| {
            let problem = parse_object(inputs.text("json")).err();
            problem.unwrap_or_default().into()
        });
        match ctx.show(island)? {
            Outcome::Submitted(fields) => match parse_object(fields.text("json")) {
                Ok(arguments) => return Ok(Some(arguments)),
                Err(e) => {
                    ctx.error(e);
                    text = fields.text("json").to_owned();
                }
            },
            Outcome::Pressed(..) | Outcome::Cancelled => return Ok(None),
        }
    }
}

/// Parses arguments given as JSON: an object (or nothing, for none).
pub(crate) fn parse_object(text: &str) -> Result<JsonObject, String> {
    if text.trim().is_empty() {
        return Ok(JsonObject::new());
    }
    match serde_json::from_str(text) {
        Ok(Value::Object(object)) => Ok(object),
        Ok(_) => Err("the arguments must be a JSON object".to_owned()),
        Err(e) => Err(format!("invalid JSON: {e}")),
    }
}

/// The parameters in an input schema, in the order of their names.
fn params(schema: &JsonObject) -> Vec<Param> {
    let required: Vec<&str> = (schema.get("required").and_then(Value::as_array))
        .map(|names| names.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return Vec::new();
    };
    let empty = JsonObject::new();
    let params = properties.iter().map(|(name, property)| {
        let property = property.as_object().unwrap_or(&empty);
        let (kind, type_name) = kind(property);
        let choices = property.get("enum").and_then(Value::as_array);
        Param {
            name: name.clone(),
            kind,
            type_name,
            required: required.contains(&name.as_str()),
            description: (property.get("description").and_then(Value::as_str)).map(str::to_owned),
            default: property.get("default").cloned(),
            choices: choices.cloned().unwrap_or_default(),
        }
    });
    params.collect()
}

/// How a property is entered, and its type's name.
fn kind(property: &JsonObject) -> (ParamKind, String) {
    let types: Vec<&str> = match property.get("type") {
        Some(Value::String(name)) => vec![name],
        Some(Value::Array(names)) => names.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    let type_name = match types.is_empty() {
        true => "any".to_owned(),
        false => types.join(" or "),
    };
    // A nullable type is entered like the type: leaving it empty sends
    // nothing, rather than null.
    let not_null: Vec<&str> = types.into_iter().filter(|&t| t != "null").collect();
    let kind = match not_null.as_slice() {
        ["string"] => ParamKind::Text,
        ["number"] => ParamKind::Number,
        ["integer"] => ParamKind::Integer,
        ["boolean"] => ParamKind::Boolean,
        _ => ParamKind::Json,
    };
    (kind, type_name)
}

/// The form for `params`, filled in with `fields` if given.
fn form(tool: &Tool, params: &Arc<Vec<Param>>, fields: Option<&Fields>) -> IslandBuilder {
    let mut children = vec![label(format!("Call {}", tool.name))];
    for (i, param) in params.iter().enumerate() {
        children.push(param_widgets(&field_id(i), param));
    }
    children.push(label(state("arguments")));
    children.push(row([
        submit_button("Call"),
        button("json", "Edit as JSON"),
        cancel_button("Cancel"),
    ]));
    let mut island = IslandBuilder::new(column(children));
    for (i, param) in params.iter().enumerate() {
        let id = field_id(i);
        match (param.kind, fields) {
            (ParamKind::Boolean, Some(fields)) => island.set_checked(&id, fields.checked(&id)),
            (ParamKind::Boolean, None) => island.set_checked(&id, default_bool(param)),
            (_, Some(fields)) => island.set_text(&id, fields.text(&id)),
            (_, None) => {}
        }
    }
    let params = params.clone();
    island.derive("arguments", move |inputs| {
        let (arguments, errors) = arguments(&params, inputs);
        match errors.is_empty() {
            true => format!("Arguments: {}", Value::Object(arguments)).into(),
            false => errors.join("\n").into(),
        }
    });
    island
}

/// A parameter's heading, description and field.
fn param_widgets(id: &str, param: &Param) -> Widget {
    let mut heading = format!("{} ({}", param.name, param.type_name);
    if param.kind == ParamKind::Json {
        heading.push_str(", as JSON");
    }
    if param.required {
        heading.push_str(", required");
    }
    heading.push(')');
    let mut widgets = vec![match param.kind {
        ParamKind::Boolean => checkbox(id, &heading),
        _ => label(heading),
    }];
    if let Some(description) = &param.description {
        widgets.push(label(description.as_str()));
    }
    let hint = hint(param);
    match param.kind {
        ParamKind::Boolean => {}
        ParamKind::Json => widgets.push(text_area(id, &hint)),
        _ => widgets.push(text_field(id, &hint)),
    }
    column(widgets)
}

/// The placeholder of a parameter's field: its choices or its default.
fn hint(param: &Param) -> String {
    let show = |value: &Value| match (param.kind, value) {
        (ParamKind::Text, Value::String(s)) => s.clone(),
        _ => value.to_string(),
    };
    let mut parts = Vec::new();
    if !param.choices.is_empty() {
        let choices: Vec<String> = param.choices.iter().map(show).collect();
        parts.push(format!("one of: {}", choices.join(", ")));
    }
    if let Some(default) = &param.default {
        parts.push(format!("default: {}", show(default)));
    }
    parts.join("; ")
}

fn field_id(i: usize) -> String {
    format!("param{i}")
}

fn default_bool(param: &Param) -> bool {
    param
        .default
        .as_ref()
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// What a form's fields are read from: the submitted fields, or the inputs
/// of derived state.
trait Read {
    fn text(&self, id: &str) -> &str;
    fn checked(&self, id: &str) -> bool;
}

impl Read for Fields {
    fn text(&self, id: &str) -> &str {
        Fields::text(self, id)
    }

    fn checked(&self, id: &str) -> bool {
        Fields::checked(self, id)
    }
}

impl Read for Inputs<'_> {
    fn text(&self, id: &str) -> &str {
        Inputs::text(self, id)
    }

    fn checked(&self, id: &str) -> bool {
        Inputs::checked(self, id)
    }
}

/// The arguments the form's fields make, and what's wrong with them (as
/// "name: problem"; those arguments are left out).
///
/// Empty fields are left out, so the tool uses its default. A checkbox is
/// sent if it's required, or differs from the default (false if none).
fn arguments(params: &[Param], fields: &impl Read) -> (JsonObject, Vec<String>) {
    let mut arguments = JsonObject::new();
    let mut errors = Vec::new();
    for (i, param) in params.iter().enumerate() {
        let id = field_id(i);
        let value = match param.kind {
            ParamKind::Boolean => {
                let checked = fields.checked(&id);
                if !param.required && checked == default_bool(param) {
                    continue;
                }
                Ok(Value::Bool(checked))
            }
            kind => {
                let text = fields.text(&id);
                if text.trim().is_empty() {
                    continue;
                }
                parse(kind, text)
            }
        };
        match value {
            Ok(value) => {
                arguments.insert(param.name.clone(), value);
            }
            Err(e) => errors.push(format!("{}: {e}", param.name)),
        }
    }
    (arguments, errors)
}

/// The value of a text field of `kind`.
fn parse(kind: ParamKind, text: &str) -> Result<Value, String> {
    if kind == ParamKind::Text {
        return Ok(Value::String(text.to_owned()));
    }
    let value: Value = serde_json::from_str(text).map_err(|e| match kind {
        ParamKind::Json => format!("invalid JSON: {e}"),
        _ => "not a number".to_owned(),
    })?;
    match kind {
        ParamKind::Number if !value.is_number() => Err("not a number".to_owned()),
        ParamKind::Integer if !(value.is_i64() || value.is_u64()) => {
            Err("not an integer".to_owned())
        }
        _ => Ok(value),
    }
}

/// Shows what tool `name` returned: its content, then its structured
/// content. Fails if the tool reported an error.
pub(crate) fn show_result(ctx: &Context, name: &str, result: &CallToolResult) -> CommandResult {
    for content in &result.content {
        match content {
            ContentBlock::Text(text) => ctx.print(text.text.as_str()),
            ContentBlock::Image(content) => show_image(ctx, name, content),
            ContentBlock::Audio(audio) => {
                ctx.print(format!("Audio: {}, {}", audio.mime_type, size(&audio.data)));
            }
            ContentBlock::Resource(embedded) => match &embedded.resource {
                ResourceContents::TextResourceContents { uri, text, .. } => {
                    ctx.print(format!("Resource {uri}:\n{text}"));
                }
                ResourceContents::BlobResourceContents {
                    uri,
                    mime_type,
                    blob,
                    ..
                } => {
                    let mime_type = mime_type.as_deref().unwrap_or("binary data");
                    ctx.print(format!("Resource {uri}: {mime_type}, {}", size(blob)));
                }
                _ => ctx.print("A resource of an unknown kind"),
            },
            ContentBlock::ResourceLink(resource) => {
                ctx.print(format!(
                    "Link to resource {} ({})",
                    resource.uri, resource.name
                ));
            }
            _ => ctx.print("Content of an unknown kind"),
        }
    }
    if let Some(structured) = &result.structured_content {
        ctx.print(format!("Structured content:\n{}", pretty(structured)));
    }
    if result.content.is_empty() && result.structured_content.is_none() {
        ctx.print("The tool returned nothing.");
    }
    if result.is_error == Some(true) {
        return Err("the tool reported an error".into());
    }
    Ok(())
}

fn show_image(ctx: &Context, name: &str, content: &ImageContent) {
    let mime_type = &content.mime_type;
    let alt = format!("{mime_type} image from {name}");
    let image = BASE64.decode(&content.data).map_err(|e| e.to_string());
    let image = image.and_then(|bytes| Image::from_bytes(bytes, alt).map_err(|e| e.to_string()));
    let island = image.and_then(|image| {
        let mut island = IslandBuilder::new(kettle_ui::widget::image("image"));
        island.set_state("image", image);
        island.freeze().map_err(|e| e.to_string())
    });
    match island {
        Ok(island) => ctx.print_island(island),
        Err(e) => ctx.print(format!("Image ({mime_type}), which can't be shown: {e}")),
    }
}

/// The size of base64 `data`, in bytes.
fn size(data: &str) -> String {
    match BASE64.decode(data) {
        Ok(bytes) => format!("{} bytes", bytes.len()),
        Err(e) => format!("invalid base64 ({e})"),
    }
}
