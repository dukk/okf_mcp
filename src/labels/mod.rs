//! Human labels for users, teams and catalogs (spec
//! 2026-11-17-okf-display-names-design.md). A slug (`name`) is a permanent,
//! opaque id; a label is what a person reads. This module is the one place
//! the fallback rule lives: every surface calls these functions (or reads
//! the name-to-label directory built with them), so no page spells it again.

use std::borrow::Cow;

use crate::error::OkfError;

/// The longest display name, in characters (Unicode scalar values, which
/// is what Postgres `char_length` counts).
pub const MAX_DISPLAY_NAME_CHARS: usize = 80;
/// A user with no display name and no usable email.
pub const UNNAMED_USER: &str = "Unnamed user";
/// A shared team with no display name.
pub const UNNAMED_TEAM: &str = "Unnamed team";
/// A catalog with neither display name nor description.
pub const UNTITLED_CATALOG: &str = "Untitled catalog";

/// A validated display name: trimmed, at most [`MAX_DISPLAY_NAME_CHARS`],
/// no control characters or line breaks. Empty means "unset". Not unique.
// A "newtype": a struct wrapping one `String`, so the compiler tells a
// checked display name apart from any other string (like a Python
// `NewType`, but enforced).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DisplayName(String);

impl DisplayName {
    /// Validates and normalises a proposed display name.
    ///
    /// # Arguments
    /// * `text` - what the caller sent.
    ///
    /// # Returns
    /// The trimmed name (possibly empty).
    ///
    /// # Errors
    /// [`OkfError::LimitExceeded`] (`limit: "display_name"`, 422) for one
    /// over the length limit or holding a control character or line break.
    #[tracing::instrument(level = "trace", skip_all)]
    pub fn parse(text: &str) -> Result<Self, OkfError> {
        let trimmed = text.trim();
        // U+2028/U+2029 are line and paragraph separators: not `is_control`,
        // but they break a line in an email subject all the same.
        let breaks = |c: char| c.is_control() || c == '\u{2028}' || c == '\u{2029}';
        if trimmed.chars().any(breaks) {
            return Err(refusal("it holds a control character or a line break"));
        }
        if trimmed.chars().count() > MAX_DISPLAY_NAME_CHARS {
            return Err(refusal(&format!(
                "it is longer than {MAX_DISPLAY_NAME_CHARS} characters"
            )));
        }
        Ok(Self(trimmed.to_string()))
    }

    /// The text.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "trivial accessor or conversion: returns or maps a value, a span would record nothing new"
        )
    )]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether it is unset.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "trivial accessor or conversion: returns or maps a value, a span would record nothing new"
        )
    )]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// The error for a refused display name.
#[cfg_attr(
    dylint_lib = "hyp_tracing",
    allow(
        uninstrumented_fn,
        reason = "trivial conversion: builds an error value, no I/O"
    )
)]
fn refusal(why: &str) -> OkfError {
    OkfError::LimitExceeded {
        limit: "display_name",
        reason: format!("display name refused: {why}"),
    }
}

/// Cuts `text` to at most [`MAX_DISPLAY_NAME_CHARS`] characters, trimmed.
// `Cow` ("clone on write") is either a borrowed `&str` or an owned
// `String`: no allocation when the text is already short enough.
#[tracing::instrument(level = "trace", skip_all)]
fn bounded(text: &str) -> Cow<'_, str> {
    let text = text.trim();
    match text.char_indices().nth(MAX_DISPLAY_NAME_CHARS) {
        None => Cow::Borrowed(text),
        Some((cut, _)) => Cow::Owned(text[..cut].trim_end().to_string()),
    }
}

/// A user's label: its display name, else its email's local part, else
/// [`UNNAMED_USER`]. Never the slug.
///
/// # Arguments
/// * `display_name` - `users.display_name`.
/// * `email` - `users.email`, if set.
///
/// # Returns
/// The label.
#[tracing::instrument(level = "trace", skip_all)]
pub fn user_label(display_name: &str, email: Option<&str>) -> String {
    let display = bounded(display_name);
    if !display.is_empty() {
        return display.into_owned();
    }
    // `split_once('@')` is Python's `partition('@')` without the separator.
    let local = email
        .and_then(|address| address.split_once('@'))
        .map(|(local, _)| bounded(local))
        .filter(|local| !local.is_empty());
    local.map_or_else(|| UNNAMED_USER.to_string(), Cow::into_owned)
}

/// A team's label: for a personal team, "{user label}'s team"; else its
/// display name, else [`UNNAMED_TEAM`].
///
/// # Arguments
/// * `display_name` - `teams.display_name` (ignored for a personal team).
/// * `personal_of` - the label of the user whose personal team it is.
///
/// # Returns
/// The label.
#[tracing::instrument(level = "trace", skip_all)]
pub fn team_label(display_name: &str, personal_of: Option<&str>) -> String {
    if let Some(user) = personal_of {
        return format!("{user}'s team");
    }
    let display = bounded(display_name);
    if display.is_empty() {
        UNNAMED_TEAM.to_string()
    } else {
        display.into_owned()
    }
}

/// A catalog's label: its display name, else its description (cut to the
/// display-name length), else [`UNTITLED_CATALOG`].
///
/// # Arguments
/// * `display_name` - `catalogs.display_name`.
/// * `description` - `catalogs.description`.
///
/// # Returns
/// The label.
#[tracing::instrument(level = "trace", skip_all)]
pub fn catalog_label(display_name: &str, description: &str) -> String {
    // A description may span lines; a label never does.
    let collapsed = description.split_whitespace().collect::<Vec<_>>().join(" ");
    let label = [bounded(display_name), bounded(&collapsed)]
        .into_iter()
        .find(|candidate| !candidate.is_empty())
        .map_or_else(|| UNTITLED_CATALOG.to_string(), Cow::into_owned);
    label
}

/// A filename stem from a label: ASCII letters and digits lowercased, every
/// other run of characters one `-`, at most 40 bytes, no leading or
/// trailing `-`. Empty when nothing is left.
///
/// # Arguments
/// * `label` - the label.
///
/// # Returns
/// The stem; safe in a `Content-Disposition` header.
#[tracing::instrument(level = "trace", skip_all)]
pub fn export_stem(label: &str) -> String {
    let mut stem = String::with_capacity(label.len().min(40));
    for c in label.chars() {
        if c.is_ascii_alphanumeric() {
            stem.push(c.to_ascii_lowercase());
        } else if !stem.ends_with('-') && !stem.is_empty() {
            stem.push('-');
        }
        if stem.len() >= 40 {
            break;
        }
    }
    stem.trim_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_name_trims_and_accepts_empty() {
        assert_eq!(
            DisplayName::parse("  Alice Example \t").unwrap().as_str(),
            "Alice Example"
        );
        assert!(DisplayName::parse("   ").unwrap().is_empty());
    }

    #[test]
    fn display_name_counts_characters_not_bytes() {
        let eighty = "é".repeat(MAX_DISPLAY_NAME_CHARS);
        assert!(DisplayName::parse(&eighty).is_ok());
        let error = DisplayName::parse(&format!("{eighty}é")).unwrap_err();
        assert!(error.to_string().contains("80 characters"), "{error}");
    }

    #[test]
    fn display_name_refuses_control_characters_and_line_breaks() {
        for bad in ["a\nb", "a\rb", "a\u{0007}b", "a\u{2028}b", "a\u{2029}b"] {
            assert!(DisplayName::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn user_label_falls_back_to_the_email_local_part_then_a_placeholder() {
        assert_eq!(user_label("Alice", Some("alice@example.com")), "Alice");
        assert_eq!(user_label("", Some("alice@example.com")), "alice");
        assert_eq!(user_label("", Some("@example.com")), UNNAMED_USER);
        assert_eq!(user_label("", None), UNNAMED_USER);
    }

    #[test]
    fn team_label_of_a_personal_team_follows_its_user() {
        assert_eq!(team_label("ignored", Some("Alice")), "Alice's team");
        assert_eq!(team_label("Acme", None), "Acme");
        assert_eq!(team_label("", None), UNNAMED_TEAM);
    }

    #[test]
    fn catalog_label_falls_back_to_the_description_then_a_placeholder() {
        assert_eq!(catalog_label("Work notes", "Personal memory"), "Work notes");
        assert_eq!(catalog_label("", "Personal memory"), "Personal memory");
        assert_eq!(catalog_label("", "  "), UNTITLED_CATALOG);
    }

    #[test]
    fn a_long_description_is_cut_for_the_label() {
        // No spaces: a cut that lands on a space would be trimmed to 79.
        let long = "x".repeat(200);
        assert_eq!(
            catalog_label("", &long).chars().count(),
            MAX_DISPLAY_NAME_CHARS
        );
    }

    #[test]
    fn export_stem_is_header_safe_ascii() {
        assert_eq!(export_stem("Work notes: 2026!"), "work-notes-2026");
        assert_eq!(export_stem("Ünïcode only ✓"), "n-code-only");
        assert_eq!(export_stem("!!!"), "");
    }

    #[test]
    fn export_stem_stops_at_forty_bytes() {
        assert_eq!(export_stem(&"A".repeat(40)), "a".repeat(40));
        assert_eq!(export_stem(&"A".repeat(41)), "a".repeat(40));
        // A separator that lands on the cutoff is trimmed, not kept.
        assert_eq!(
            export_stem(&format!("{} b", "a".repeat(39))),
            "a".repeat(39)
        );
    }

    #[test]
    fn a_refused_display_name_names_its_limit() {
        match DisplayName::parse("a\nb") {
            Err(OkfError::LimitExceeded { limit, .. }) => assert_eq!(limit, "display_name"),
            other => panic!("expected LimitExceeded, got {other:?}"),
        }
    }

    #[hegel::test]
    fn an_export_stem_is_short_ascii_without_edge_hyphens(tc: hegel::TestCase) {
        use hegel::generators;
        let label: String = tc.draw(generators::text());
        let stem = export_stem(&label);
        assert!(stem.len() <= 40, "{stem:?}");
        assert!(stem
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'));
        assert!(!stem.starts_with('-') && !stem.ends_with('-'), "{stem:?}");
    }

    #[hegel::test]
    fn labels_are_never_empty(tc: hegel::TestCase) {
        use hegel::generators;
        let display: String = tc.draw(generators::text());
        let other: String = tc.draw(generators::text());
        assert!(!user_label(&display, Some(&other)).is_empty());
        assert!(!team_label(&display, None).is_empty());
        assert!(!catalog_label(&display, &other).is_empty());
    }

    #[hegel::test]
    fn a_parsed_display_name_reparses_to_itself(tc: hegel::TestCase) {
        use hegel::generators;
        let text: String = tc.draw(generators::text());
        if let Ok(name) = DisplayName::parse(&text) {
            assert_eq!(DisplayName::parse(name.as_str()).unwrap(), name);
            assert!(name.as_str().chars().count() <= MAX_DISPLAY_NAME_CHARS);
        }
    }
}
