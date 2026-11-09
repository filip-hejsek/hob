//! `prompt`'s and `prompts`' parts: asking for a prompt's arguments,
//! describing a prompt, and showing its messages.

use hob_core::Context;
use kettle_ui::widget::{cancel_button, column, label, row, submit_button, text_field};
use kettle_ui::{IslandBuilder, Outcome};
use rmcp::model::{GetPromptResult, JsonObject, Prompt, PromptArgument, Role};
use serde_json::Value;

use crate::{BoxError, content};

/// Asks for the arguments to `prompt`. `None` if the user cancelled. A
/// prompt without arguments gets none, without asking. Empty fields are
/// left out.
pub(crate) fn ask(ctx: &Context, prompt: &Prompt) -> Result<Option<JsonObject>, BoxError> {
    let params = prompt.arguments.as_deref().unwrap_or_default();
    if params.is_empty() {
        return Ok(Some(JsonObject::new()));
    }
    let mut children = vec![label(format!("Get prompt {}", prompt.name))];
    for (i, param) in params.iter().enumerate() {
        let mut widgets = vec![label(heading(param))];
        if let Some(description) = &param.description {
            widgets.push(label(description.as_str()));
        }
        widgets.push(text_field(&field_id(i), ""));
        children.push(column(widgets));
    }
    children.push(row([submit_button("Get"), cancel_button("Cancel")]));
    match ctx.show(IslandBuilder::new(column(children)))? {
        Outcome::Submitted(fields) => {
            let mut arguments = JsonObject::new();
            for (i, param) in params.iter().enumerate() {
                let value = fields.text(&field_id(i));
                if !value.is_empty() {
                    arguments.insert(param.name.clone(), Value::String(value.to_owned()));
                }
            }
            Ok(Some(arguments))
        }
        Outcome::Pressed(..) | Outcome::Cancelled => Ok(None),
    }
}

fn field_id(i: usize) -> String {
    format!("arg{i}")
}

/// An argument's name, title, and whether it's required.
fn heading(param: &PromptArgument) -> String {
    let mut heading = param.name.clone();
    if let Some(title) = &param.title {
        heading = format!("{heading}: {title}");
    }
    if param.required == Some(true) {
        heading.push_str(" (required)");
    }
    heading
}

/// Parses arguments given as `NAME=VALUE`.
pub(crate) fn parse_assignments(args: &[String]) -> Result<JsonObject, String> {
    let mut arguments = JsonObject::new();
    for arg in args {
        let (name, value) =
            (arg.split_once('=')).ok_or_else(|| format!("expected NAME=VALUE, not {arg:?}"))?;
        arguments.insert(name.to_owned(), Value::String(value.to_owned()));
    }
    Ok(arguments)
}

/// All about a prompt: title, description and arguments.
pub(crate) fn describe(prompt: &Prompt) -> String {
    let mut sections = Vec::new();
    sections.push(match &prompt.title {
        Some(title) => format!("{}: {title}", prompt.name),
        None => prompt.name.clone(),
    });
    if let Some(description) = &prompt.description {
        sections.push(description.clone());
    }
    let params = prompt.arguments.as_deref().unwrap_or_default();
    if params.is_empty() {
        sections.push("Arguments: none".to_owned());
    } else {
        let lines = params.iter().map(|param| match &param.description {
            Some(description) => format!("- {}: {description}", heading(param)),
            None => format!("- {}", heading(param)),
        });
        let lines: Vec<String> = lines.collect();
        sections.push(format!("Arguments:\n{}", lines.join("\n")));
    }
    sections.join("\n\n")
}

/// Shows prompt `name` as the server gave it: its description, then each
/// message under its role.
pub(crate) fn show(ctx: &Context, name: &str, result: &GetPromptResult) {
    if let Some(description) = &result.description {
        ctx.print(format!("Description: {description}"));
    }
    if result.messages.is_empty() {
        ctx.print("The prompt has no messages.");
    }
    for message in &result.messages {
        let role = match message.role {
            Role::User => "User:",
            Role::Assistant => "Assistant:",
        };
        content::show_block(ctx, name, Some(role), &message.content);
    }
}
