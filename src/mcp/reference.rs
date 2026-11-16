//! The public tool reference, `docs/reference/tools.md`: one section per tool, rendered from
//! the live tool list so the page cannot drift from what `tools/list` sends. Every tool's
//! description ends with a link to its heading here (see [`super::TOOL_REFERENCE_URL`]).
//!
//! A test renders the page and fails when the checked-in file differs. To refresh the file after
//! changing a tool, run `OKF_UPDATE_TOOL_REFERENCE=1 cargo test --lib mcp::reference`.

use std::fmt::Write as _;

use rmcp::model::Tool;
use serde_json::Value;

use crate::mcp::{OkfServer, TOOL_REFERENCE_URL};

/// The checked-in page, relative to the crate root.
const PAGE: &str = "docs/reference/tools.md";

/// The page's opening, up to the tool index.
const INTRO: &str = "# Telekin tool reference

<!-- Generated from the server's tool list by `src/mcp/reference.rs`. Do not edit by hand:
run `OKF_UPDATE_TOOL_REFERENCE=1 cargo test --lib mcp::reference` to refresh it. -->

Telekin serves Open Knowledge Format (OKF) catalogs over MCP: concepts (markdown files with YAML
frontmatter) about systems, projects, people and decisions. This page lists every tool the
server offers, with its parameters. Each tool's description, as a client sees it in `tools/list`,
ends with a link to its section here.

How a session usually goes:

- `search` for a concept, then `get_concept` to read it.
- A concept path looks like `/infra/time-server.md`; the leading slash and the `.md` are
  optional.
- Without `catalog`, a tool uses the catalogs your connection can see (`list_catalogs` names
  them).
- To write, call `open_changeset` first and pass the handle it returns as `changeset` to every
  edit, `publish`, `discard`, `revert` and `pending_changes`. Reads take it too, to see the
  drafts. Nothing is visible to others until `publish`.

A tool is one of four kinds:

- read-only: it changes nothing.
- staged: it adds an entry to a changeset, which `discard` drops and `publish` applies.
- maintenance: a step in a maintenance run (claim, renew, submit, dispose). It takes no
  changeset and is saved at once, so `discard` does not undo it; it never touches a concept.
- destructive: it deletes, or cannot be undone from inside the server.
";

/// The tool's description without the trailing reference link `OkfServer::tools` adds.
///
/// # Arguments
/// * `tool` - a tool from the router.
///
/// # Returns
/// The description as written on the tool.
fn own_description(tool: &Tool) -> String {
    let full = tool.description.as_deref().unwrap_or_default();
    let suffix = format!(" Reference: {TOOL_REFERENCE_URL}#{}", tool.name);
    full.strip_suffix(&suffix).unwrap_or(full).to_string()
}

/// A short type name for a property schema: its `type`, an enum's values, a `$ref`'s target, or
/// the alternatives of an `anyOf`. `null` is dropped (an optional parameter says so itself).
///
/// # Arguments
/// * `schema` - one property's JSON Schema.
///
/// # Returns
/// A phrase such as `string`, `array of string` or `one of: a, b`.
fn type_name(schema: &Value) -> String {
    if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        let listed: Vec<String> = values
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect();
        return format!("one of: {}", listed.join(", "));
    }
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        return reference
            .rsplit('/')
            .next()
            .unwrap_or(reference)
            .to_string();
    }
    for key in ["anyOf", "oneOf"] {
        if let Some(options) = schema.get(key).and_then(Value::as_array) {
            let names: Vec<String> = options
                .iter()
                .map(type_name)
                .filter(|name| name != "null")
                .collect();
            return names.join(" or ");
        }
    }
    let kinds: Vec<&str> = match schema.get("type") {
        Some(Value::String(kind)) => vec![kind.as_str()],
        Some(Value::Array(kinds)) => kinds.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    let names: Vec<String> = kinds
        .into_iter()
        .filter(|kind| *kind != "null")
        .map(|kind| match (kind, schema.get("items")) {
            ("array", Some(items)) => format!("array of {}", type_name(items)),
            _ => kind.to_string(),
        })
        .collect();
    if names.is_empty() {
        // Only `null` (an `anyOf` arm that says "or absent"), or no `type` at all.
        if schema.get("type").is_some() {
            "null"
        } else {
            "any"
        }
        .to_string()
    } else {
        names.join(" or ")
    }
}

/// Make text safe for one table cell: one line, no pipe characters.
///
/// # Arguments
/// * `text` - a description.
///
/// # Returns
/// The text on one line with `|` escaped.
fn cell(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace('|', "\\|")
}

/// The behaviour class a tool's hints name.
///
/// # Arguments
/// * `tool` - a tool from the router.
///
/// # Returns
/// `read-only`, `staged`, `maintenance` or `destructive`.
fn class(tool: &Tool) -> &'static str {
    let hints = tool.annotations.as_ref();
    if hints.and_then(|h| h.read_only_hint) == Some(true) {
        "read-only"
    } else if hints.and_then(|h| h.destructive_hint) == Some(true) {
        "destructive"
    } else if tool.name.starts_with("maintenance_") {
        // Run steps persist at once and take no changeset handle, so they are not "staged".
        "maintenance"
    } else {
        "staged"
    }
}

/// Append one tool's section.
///
/// # Arguments
/// * `page` - the page being built.
/// * `tool` - the tool to describe.
fn render_tool(page: &mut String, tool: &Tool) {
    let title = tool.title.as_deref().unwrap_or_default();
    // A heading that is only the tool's name makes `#<name>` the anchor on GitHub.
    let _ = write!(
        page,
        "## `{}`\n\n**{title}** ({})\n\n{}\n\n",
        tool.name,
        class(tool),
        own_description(tool)
    );
    let schema = Value::Object((*tool.input_schema).clone());
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|names| names.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        page.push_str("No parameters.\n\n");
        return;
    };
    page.push_str("| Parameter | Type | Required | Description |\n|---|---|---|---|\n");
    for (name, property) in properties {
        let description = property
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let _ = writeln!(
            page,
            "| `{name}` | {} | {} | {} |",
            cell(&type_name(property)),
            if required.contains(&name.as_str()) {
                "yes"
            } else {
                "no"
            },
            cell(description)
        );
    }
    page.push('\n');
}

/// The whole page for the live tool list.
///
/// # Returns
/// The markdown, tools in name order.
fn render() -> String {
    let mut tools = OkfServer::tools().list_all();
    tools.sort_by(|a, b| a.name.cmp(&b.name));
    let mut page = String::from(INTRO);
    page.push_str("| Tool | Title | Kind |\n|---|---|---|\n");
    for tool in &tools {
        let _ = writeln!(
            page,
            "| [`{0}`](#{0}) | {1} | {2} |",
            tool.name,
            tool.title.as_deref().unwrap_or_default(),
            class(tool)
        );
    }
    page.push('\n');
    for tool in &tools {
        render_tool(&mut page, tool);
    }
    page.trim_end().to_string() + "\n"
}

#[test]
fn the_checked_in_tool_reference_matches_the_tool_list() {
    let rendered = render();
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(PAGE);

    if std::env::var_os("OKF_UPDATE_TOOL_REFERENCE").is_some() {
        std::fs::write(&path, &rendered).expect("the reference page is writable");
    }

    let checked_in = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        checked_in == rendered,
        "{PAGE} is out of date: run `OKF_UPDATE_TOOL_REFERENCE=1 cargo test --lib mcp::reference`"
    );
}

#[test]
fn the_reference_lists_every_tool_with_a_heading_its_link_points_at() {
    let page = render();
    let tools = OkfServer::tools().list_all();

    assert_eq!(tools.len(), 42);
    for tool in &tools {
        assert!(
            page.contains(&format!("\n## `{}`\n", tool.name)),
            "{}",
            tool.name
        );
        let description = tool.description.as_deref().unwrap_or_default();
        assert!(
            description.ends_with(&format!(" Reference: {TOOL_REFERENCE_URL}#{}", tool.name)),
            "{} does not end with its reference link",
            tool.name
        );
    }
}

#[test]
fn the_reference_has_no_private_details() {
    let page = render();

    assert!(!page.contains("okf.py"));
    assert!(!page.contains("vsix"));
}
