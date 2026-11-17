# OKF MCP Server, Milestone 17 (display names): Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. In this project each task is one GitHub sub-issue and one PR (`github-issue-to-pr`).

> **Builds on `main` at `283594c`** (v0.21.0), plus the spec commit on `okf-mcp/okf-ids`.

**Goal:** Users, teams and catalogs keep their slug `name` as a permanent, opaque id, and gain a human **label** that people can set and edit. Every surface a person reads shows the label; the slug stays wherever an exact value is needed.

**Architecture:**
- **One module, `src/labels/`, owns the rule.**
  - `DisplayName` is the validated newtype for every write.
  - `user_label`, `team_label` and `catalog_label` are the three pure label functions.
  - `Labels` is a name-to-label directory loaded with the key cache.
- **The key cache carries `Labels`.** Every label input (a display name, an email, a description, an owner, a membership) already bumps `admin_generation` when it changes, so the directory reloads exactly when it must, on every replica (M16 announcements). Readers call `app.keys().labels()`, a pointer copy.
- **Primary views compute `label` from their own record.** References to other entities (a user's teams, a key's catalogs, an actor) resolve through `Labels`. The console fetches one scoped `GET /api/v1/labels` and resolves references client-side by lookup only; no fallback logic runs in JS.
- **Stored data never changes meaning.** Slugs stay in every column, URL, parameter and audit row. Only rendering changes.

**Tech Stack:** Rust 2021 with the existing axum 0.8 / sqlx 0.9 / rmcp 3.8 / tokio stack; `hegeltest` (imported as `hegel`) for property tests; vanilla JS console with the `tests/ui_e2e` browser tests. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-11-17-okf-display-names-design.md`.

---

## Conventions for every task

- **Branches and PRs.** Branch per task from `main`, one PR per task, linked to its sub-issue. Follow `AGENTS.md` (review rounds, keeping branches current, `tools/agentic/`) and `tools/agentic/brief-common.md`.
- **TDD.** Write the failing test, run it, implement, run it, commit.
- **Gates** run only through `tools/agentic/gate.sh`: `fmt`, `lint`, `test` and `dylint` before every push, plus `test-ui` for Tasks 10a-10c. While iterating, use `gate.sh narrow <cargo test args>` (one test-name filter per run).
- **Code style.** Comments explain Rust-specific constructs for a Python-expert, Rust-novice reader, as in the existing code.
  - Every public item gets a doc comment with `# Arguments`, `# Returns` and `# Errors`.
  - No `.unwrap()` outside tests.
  - Every new function has `#[tracing::instrument(.., skip_all)]` or a reason-bearing `uninstrumented_fn` allow (the `hyp_tracing` lint is at deny).
- **Privacy rules.** Each is a review checklist item.
  - A display name is personal data. **Never log it or put it in a span.** Log lines keep naming slugs, as today.
  - Audit details for a label change record **only the new length in characters**, never the text, as `catalog.description_changed` does today.
  - `GET /api/v1/labels` returns only the labels Task 5 lists for the caller.
- **No slug renames.** No task adds an `UPDATE ... SET name`.
- **Changelog.** One entry per task, `changelog/<UTC ts>-m17-task-<NN>-<slug>.md`, with `## Added`, `## Changed` and `## Notes`.
  - Take the timestamp from `date -u +%Y-%m-%dT%H-%M-%SZ`.
  - Task 12 also writes the milestone changelog.
- **The code in this plan has not been compiled.** If an implementation has to differ, fix this plan in the same PR (inline, or with a dated `> **Corrected (issue #N).**` note), then grep later tasks for the changed symbol.
- **Placeholders only** in tests and docs: `example.com`, `alice`, `Acme`, `u-0000000000000001`, `m-0000000000000001`. No real hostnames, names or keys.
- **Commit messages** end with the attribution line the coordinator gives.

## Verified facts the code relies on (at `283594c`)

**Schema**
- `users`, `teams`: `display_name text NOT NULL DEFAULT ''` (`migrations/0009_users_teams.sql:8-22`), never validated. `catalogs` has no display name (`0001_schema.sql:12-19`). The next migration number is `0019`.
- Creating a user also inserts its personal team with **the same `name` and `display_name`** (`src/store/users.rs:205-239`).

**Stores**
- `catalogs::update_description` (`src/store/catalogs.rs:335-389`): locks people, `UPDATE ... RETURNING old`, returns early on the same text, records `Action::CatalogDescriptionChanged` with `{"length": n}`, then `bump_admin_generation`.
- `catalogs::set_maintenance` (`src/store/catalogs.rs:498-`) has the **team-owner check** this plan reuses. It runs `SELECT m.team_id FROM catalogs c JOIN team_members m ... WHERE c.name = $1 AND m.user_id = $2 AND m.role = 'owner' FOR SHARE OF m` under the transaction, and refuses with `Forbidden` (an unknown catalog included).
- The API decides who is an owner at `src/http/api/catalogs.rs:403-410`: every-catalog admin → `None`; a `CredentialKind::Session { user_id }` → `Some(user_id)`; any other kind → the refusal.
- `users::set_email` (`src/store/users.rs:639-698`) is the model for a user update: `lock_target(.., RowLock::Update, actor)`, then `audit::refuse_admin_target`, then the update, the bump and `user_entry(..)`.
- `UserAuth` (`src/store/users.rs:54-63`) holds `id, name, email, is_admin`, built from `user_auth_columns!()` (`:85-89`), which `email_tokens` and `passwords` splice in. It is what `IssuedLink.user`, the passkey and setup pages, and the key cache's `users` map hold.
- Audit actions are one `actions!` list (`src/store/audit.rs:259-302`). Names must match `^[a-z]+\.[a-z_]+$` (`0012_audit.sql:45`). Entries refresh `audit_people.name/display_name` from `users` on every write naming the person (`0014_audit_row_locks.sql:35-53`). The audit listing reads `ap.name` only (`src/store/audit.rs:1393-1424`).

**Caches**
- `KeyCache::read` (`src/auth.rs:930-950`) loads keys, ceilings, `users::auth_by_id` and `teams::suspended_catalogs` with `tokio::try_join!`, then the gates.
- `Keys` swaps the whole `Arc<KeyCache>` on `reload`. Accessors such as `suspensions()` (`:1098`) clone an `Arc`.
- `CatalogState` holds `description` outside the snapshot, refreshed by the poll loop (`src/catalog.rs:162`, `src/app.rs:736`).
- `Suspensions = HashMap<String, SuspendedCatalog { team, notice }>` (`src/auth.rs:81`, `src/store/teams.rs:146-151`), and `OkfError::Suspended { catalog, team, notice }` builds its message in `suspended_message` (`src/error.rs:495-504`).

**HTTP**
- `require_manager` = provisioner OR every-catalog admin (`src/http/api/mod.rs:265-270`).
- `PROVISIONER_ROUTES` (`:185-203`) is the one allow-list.
- `settle(&app)` polls after a write.
- Status mapping (`src/http/mod.rs:138-169`):
  - `Refused` → 409;
  - `LimitExceeded` → 422;
  - `Forbidden` and `Suspended` → 403;
  - `UnknownName` and `CatalogNotSelected` → 404.

**Rendering**
- `starter_files(name, description, now)` writes `# {name}` through `root_index_text(heading, about)` (`src/okf/ops/log_index.rs:380-390`, `src/okf/ops/replay.rs:350-354`). `ROOT_HEADING` is `"Catalog"`.
- The export disposition is `filename="{name}.tar.gz"` (`src/http/archives.rs:404`).
- The consent pages: `UserPage { user, ceiling, .. }` (`src/oauth/consent.rs:269-278`) renders rows at `:333-393`. `GrantSummary` is built in `src/oauth/authorize.rs:339`.
- Emails:
  - `Template::Setup { link, user }` (`src/mail/templates.rs:208`) is filled from `issued.user.name` (`src/mail/invite.rs:79-82`);
  - `Template::Unlocked { catalog, .. }` is filled from `new.catalog` (`src/store/unlocks.rs:568-571`);
  - templates are queued as stored rows, so **field names stay** and only the values change.
- Passkey registration passes `(handle, account = email || name, display = name)` (`src/http/passkeys.rs:539-546`).
- The setup page's welcome uses `view.user` (`src/http/signin.rs:875`), filled from `email_tokens::peek(..).name` (`:906-912`).

**MCP and the console**
- `list_catalogs` builds `CatalogInfo { name, description, files, last_change, suspended }` (`src/mcp/catalogs.rs:41-112`).
- `INSTRUCTIONS` is `src/mcp/mod.rs:56-63`. `docs/reference/tools.md` is generated by a test (the doc comment at `src/mcp/mod.rs:65`).
- The console header is built in `refreshIdentity` (`src/http/ui/js/core/shell.js:9-34`). Browser tests live in `tests/ui_e2e/*.rs` and run with `just test-ui`.

## Decisions taken while planning (2026-11-17)

1. **The label directory lives in the key cache**, not in `CatalogState` or per request. Every input to a label already bumps `admin_generation` (user create, email change, team create, membership change, catalog create, description change, owner change). Tasks 3 and 5's new writes bump it too.
2. **References resolve through one scoped endpoint, `GET /api/v1/labels`**, rather than reshaping every view. Existing view fields stay the same shape (the provisioner and the CLI read them); only additive fields are added.
3. **Root `index.md` heading:** the catalog's `display_name`, else `ROOT_HEADING` ("Catalog"). The spec's wording, "the catalog label", would fall back to the description, but the description is already the paragraph under the heading, so the heading would repeat it. Task 9 corrects the spec.
4. **Audit details of label changes are `{"length": n}`**, following the existing privacy rule ("People in entries"). The spec's "recording the old and new value" is corrected by Task 3.
5. **Actor resolution:** an actor is shown as a user's label **only when it equals a user slug**. Session actors are user slugs; key and OAuth actors pass through unchanged. A key whose free-text actor equals another user's slug would be shown with that user's label. That is accepted: such a key can already impersonate that string in history today.
6. **Catalog-label edits by a team owner are refused while the team is suspended** (403 `Suspended`), as the admin path already refuses through `Principal::require`.
7. **This is milestone 17.**

## Scope

| Task | Delivers |
|---|---|
| 1 | `src/labels/mod.rs`: `DisplayName`, `user_label`, `team_label`, `catalog_label`, `export_stem`; unit and property tests |
| 2 | Migration 0019; `Catalog.display_name`; `NewCatalog.display_name`; `UserAuth.display_name` |
| 3 | `users::set_display_name`, `teams::set_display_name`, `catalogs::update_details` (with the shared owner check); three audit actions |
| 4 | `src/labels/directory.rs`: `Labels`, `Labels::load`, `catalog_choices`; `Keys::labels()`; suspension labels |
| 5 | REST: `label` on views; `GET /labels`; `PATCH /me`, `/users/{name}`, `/teams/{name}`; the widened catalog PATCH and create; provisioner routes |
| 6 | CLI: `user update`, `team update`, `catalog update --display-name`, `catalog create --display-name` |
| 7 | MCP: label fields in outputs, `actor_label`, parameter descriptions, `INSTRUCTIONS`, the suspended error naming labels, regenerated `tools.md` |
| 8 | Emails, the setup page, consent and sign-in-code pages, passkey `displayName` |
| 9 | The new catalog's `index.md` heading, the export filename |
| 10a | Console core: `labels.js`, `GET /labels` in `refreshIdentity`, the header, the Account page's display-name edit |
| 10b | Console admin pages: users, teams, catalogs (label columns, edit drawers, owner edit) |
| 10c | Console other pages: browse, health, runs, changesets, transfer, unlocks, keys, my-keys, my-grants |
| 11 | Audit: the listing returns `person_label` from the snapshot; the audit page shows it |
| 12 | Docs, skills, README, spec corrections, milestone changelog, the provisioner follow-up issue |

**Deliberately deferred:** the provisioner's own changes (a follow-up issue in its repository, filed by Task 12), renamable aliases, and rewriting the headings of existing catalogs.

## File map

| File | Tasks |
|---|---|
| `src/labels/mod.rs` | 1 (create), 4 (`pub mod directory`) |
| `src/labels/directory.rs` | 4 (create) |
| `src/lib.rs` | 1 (`pub mod labels;`) |
| `migrations/0019_display_names.sql` | 2 (create) |
| `src/store/catalogs.rs` | 2, 3 |
| `src/store/users.rs`, `src/store/teams.rs` | 2 (`UserAuth`), 3 |
| `src/store/audit.rs` | 3 (actions), 11 (listing) |
| `src/auth.rs` | 4 (`KeyCache.labels`, `Keys::labels`) |
| `src/store/teams.rs` `SuspendedCatalog`, `src/error.rs` | 4 (labels on suspension), 7 (message) |
| `src/http/api/people.rs`, `me.rs`, `catalogs.rs`, `identity.rs`, `mod.rs` | 5 |
| `src/http/api/labels.rs` | 5 (create) |
| `src/cli.rs`, `src/cli/people.rs` | 6 |
| `src/mcp/*.rs`, `docs/reference/tools.md` | 7 |
| `src/mail/invite.rs`, `src/store/unlocks.rs`, `src/http/signin.rs`, `src/oauth/consent.rs`, `src/oauth/authorize.rs`, `src/http/passkeys.rs` | 8 |
| `src/okf/ops/log_index.rs`, `src/http/archives.rs` | 9 |
| `src/http/ui/js/core/labels.js` | 10a (create) |
| `src/http/ui/js/core/shell.js`, `pages/account.js` | 10a |
| `src/http/ui/js/pages/{users,teams,catalogs}.js`, `components/user-account.js` | 10b |
| `src/http/ui/js/pages/{browse,health,runs,changesets,transfer,unlocks,keys,my-keys,my-grants}.js` | 10c |
| `src/http/ui/js/pages/audit.js` | 11 |
| `tests/ui_e2e/{account,users,teams,catalogs,browse,changesets,audit}.rs` | 10a, 10b, 10c, 11 |
| `README.md`, `docs/operations.md`, `skills/*/SKILL.md`, the spec | 12 (and any task that corrects the spec) |
| `changelog/` | every task |

## Task dependencies

Each line is `task (the tasks it needs)`. A task starts when every task in its parentheses has merged. Tasks on one line can run in parallel.

```
wave 1:  1 ()
wave 2:  2 (1)
wave 3:  3 (2)    4 (2)
wave 4:  5 (3, 4)    6 (3)    7 (4)    8 (4)    9 (2, 4)    11 (4)
wave 5:  10a (5)
wave 6:  10b (10a)    10c (10a)
wave 7:  12 (all)
```

Why each edge, and the shared files to merge with care:
- **2 needs 1:** `NewCatalog.display_name` is a `&DisplayName`.
- **3 and 4 both edit `src/store/teams.rs`.** 3 adds `set_display_name`; 4 adds two fields to `SuspendedCatalog`. Keep both.
- **9 needs 2** (`Catalog.display_name` feeds the heading) **and 4** (the export filename reads `Labels`).
- **5 needs 3** (the store writes behind the PATCHes) **and 4** (`Labels` for `GET /labels` and the references).
- **6 needs only 3.**
- **7, 8, 9 and 11 need 4** (`Labels`). They touch disjoint files; only 7 edits `src/error.rs`.
- **10a needs 5** (`GET /labels`, `PATCH /me`). **10b and 10c need 10a** (`labels.js`). They edit different page files; both may touch `core/labels.js`. Keep both helpers.
- **12 is last.**

---

### Task 1: The label rule and `DisplayName`

**Files:**
- Create: `src/labels/mod.rs`
- Modify: `src/lib.rs` (add `pub mod labels;` in alphabetical position)

- [ ] **Step 1: Write the failing tests** (in `src/labels/mod.rs`, `#[cfg(test)] mod tests`)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_name_trims_and_accepts_empty() {
        assert_eq!(DisplayName::parse("  Alice Example \t").unwrap().as_str(), "Alice Example");
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
        assert_eq!(catalog_label("", &long).chars().count(), MAX_DISPLAY_NAME_CHARS);
    }

    #[test]
    fn export_stem_is_header_safe_ascii() {
        assert_eq!(export_stem("Work notes: 2026!"), "work-notes-2026");
        assert_eq!(export_stem("Ünïcode only ✓"), "n-code-only");
        assert_eq!(export_stem("!!!"), "");
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
```

Run `tools/agentic/gate.sh narrow --lib -- labels::tests`. Expect a compile failure: there is no module `labels`.

- [ ] **Step 2: Implement `src/labels/mod.rs`**

```rust
//! Human labels for users, teams and catalogs (spec
//! 2026-11-17-okf-display-names-design.md). A slug (`name`) is a permanent,
//! opaque id; a label is what a person reads. This module is the one place
//! the fallback rule lives: every surface calls these functions (or reads
//! the [`directory::Labels`] built with them), so no page spells it again.

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
            return Err(refusal(&format!("it is longer than {MAX_DISPLAY_NAME_CHARS} characters")));
        }
        Ok(Self(trimmed.to_string()))
    }

    /// The text.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(uninstrumented_fn, reason = "trivial accessor or conversion: returns or maps a value, a span would record nothing new")
    )]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether it is unset.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(uninstrumented_fn, reason = "trivial accessor or conversion: returns or maps a value, a span would record nothing new")
    )]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// The error for a refused display name.
#[cfg_attr(
    dylint_lib = "hyp_tracing",
    allow(uninstrumented_fn, reason = "trivial conversion: builds an error value, no I/O")
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
```

> Note on `export_stem("Ünïcode only ✓")`: `Ü` and `ï` are not ASCII, so they become separators, giving `n-code-only`. That is intended: the stem only helps a person recognise the file, and the slug that follows keeps it unique (Task 9).

- [ ] **Step 3: Run the tests.** `tools/agentic/gate.sh narrow --lib -- labels::tests`. Expect PASS.
- [ ] **Step 4: Full gates, changelog, commit.** `gate.sh fmt`, `lint`, `test`, `dylint`. Changelog `m17-task-01-label-rule`. Commit `feat(labels): the display-name newtype and the label rule`.

---

### Task 2: Migration 0019 and the display name on rows

**Files:**
- Create: `migrations/0019_display_names.sql`
- Modify:
  - `src/store/catalogs.rs` (`Catalog`, `select_catalogs!`, `NewCatalog`, `create`, `RETURNING`);
  - `src/store/users.rs` (`UserAuth`, `user_auth_columns!`, `NewUser`);
  - `src/store/teams.rs` (`NewTeam`);
  - every `NewCatalog { .. }` / `NewUser { .. }` / `NewTeam { .. }` literal (fix them by following the compiler errors; tests pass `&DisplayName::default()`);
  - `src/http/api/catalogs.rs`, `src/http/api/people.rs`, `src/cli.rs`, `src/cli/people.rs` (parse the incoming `String` with `DisplayName::parse(..)?` before building the struct).

- [ ] **Step 1: Write the failing store tests** (`src/store/catalogs.rs` tests; the helpers `test_db()`, `seed_team` and `now()` already exist there, so use them as the neighbouring tests do)

```rust
#[tokio::test]
async fn create_stores_the_display_name_and_find_reads_it() {
    let db = test_db().await;
    seed_team(&db.pool, "acme").await;
    let display = DisplayName::parse("Work notes").unwrap();
    let row = create(
        &db.pool,
        &NewCatalog { name: "m-0000000000000001", display_name: &display, description: "", owner_team: "acme" },
        &AuditActor::cli(),
        now(),
    )
    .await
    .unwrap();
    assert_eq!(row.display_name, "Work notes");
    assert_eq!(find(&db.pool, "m-0000000000000001").await.unwrap().display_name, "Work notes");
}

#[tokio::test]
async fn the_database_refuses_an_over_long_display_name() {
    let db = test_db().await;
    seed_team(&db.pool, "acme").await;
    let refused = sqlx::query("UPDATE teams SET display_name = repeat('x', 81) WHERE name = 'acme'")
        .execute(&db.pool)
        .await;
    assert!(refused.is_err(), "the CHECK should refuse 81 characters");
}
```

and in `src/store/users.rs` tests:

```rust
#[tokio::test]
async fn user_auth_carries_the_display_name() {
    let db = test_db().await;
    let display = DisplayName::parse("Alice Example").unwrap();
    create(&db.pool, &NewUser { name: "alice", display_name: &display }, &AuditActor::cli(), now())
        .await
        .unwrap();
    set_email(&db.pool, "alice", Some("alice@example.com"), &AuditActor::cli(), now()).await.unwrap();
    let auth = find_by_email(&db.pool, "alice@example.com").await.unwrap().unwrap();
    assert_eq!(auth.display_name, "Alice Example");
}
```

Run `gate.sh narrow --lib -- store::catalogs::tests::create_stores_the_display_name`. Expect a compile failure.

- [ ] **Step 2: The migration**

```sql
-- Display names (spec 2026-11-17-okf-display-names-design.md). A slug
-- (`name`) stays the permanent id; `display_name` is the human label, empty
-- meaning unset. Catalogs gain the column; users and teams had it, never
-- validated, so existing values are normalised the way `DisplayName::parse`
-- would (trim, control characters to spaces, at most 80 characters) before
-- the CHECK goes on. Additive: an older replica ignores the new column.

ALTER TABLE catalogs ADD COLUMN display_name text NOT NULL DEFAULT '';

UPDATE users SET display_name =
    left(btrim(regexp_replace(display_name, '[[:cntrl:]\u2028\u2029]', ' ', 'g')), 80)
WHERE display_name ~ '[[:cntrl:]\u2028\u2029]' OR char_length(display_name) > 80
   OR display_name <> btrim(display_name);
UPDATE teams SET display_name =
    left(btrim(regexp_replace(display_name, '[[:cntrl:]\u2028\u2029]', ' ', 'g')), 80)
WHERE display_name ~ '[[:cntrl:]\u2028\u2029]' OR char_length(display_name) > 80
   OR display_name <> btrim(display_name);

ALTER TABLE users    ADD CONSTRAINT users_display_name_length    CHECK (char_length(display_name) <= 80);
ALTER TABLE teams    ADD CONSTRAINT teams_display_name_length    CHECK (char_length(display_name) <= 80);
ALTER TABLE catalogs ADD CONSTRAINT catalogs_display_name_length CHECK (char_length(display_name) <= 80);
```

> `\u2028` inside a Postgres regex is a regex escape, not a string escape: with `standard_conforming_strings` on (the default), `'[[:cntrl:]\u2028\u2029]'` reaches the regex engine as written, and the ARE syntax reads `\uXXXX`. Step 3's test checks this on a real row.

> **Corrected (issue #599).** The SQL above did not match `DisplayName::parse`: `[[:cntrl:]]` depends on the database locale, `btrim` trims only ASCII spaces where Rust's `str::trim` trims all Unicode White_Space (U+00A0, U+3000, ...), and `left(.., 80)` can leave a trailing space that `labels::bounded` would trim. The shipped `migrations/0019_display_names.sql` uses explicit code-point classes (`[\u0001-\u001f\u007f-\u009f\u2028\u2029]` for the controls, the White_Space set for the trims), trims again after the cut, and rewrites only rows whose value changes (`UPDATE .. FROM (SELECT ..) n WHERE .. <> n.display_name`). It also orders its locks (PR #613 review): sqlx holds every lock until the migration commits. It starts with `LOCK TABLE users, teams IN SHARE ROW EXCLUSIVE MODE`, so an older replica's create cannot land between a cleanup snapshot and the CHECK (which would fail the migration). Then come the cleanup `UPDATE`s, then the `users` and `teams` CHECKs, then one `ALTER TABLE catalogs` adding the column and its CHECK. The ACCESS EXCLUSIVE locks come last because `catalogs` first would block every catalog read for the whole cleanup. Read that file, not this block.

- [ ] **Step 3: A migration test on legacy data** (`src/db.rs` tests, beside `migration_0010_keeps_every_row_and_both_migrators_rerun_as_no_ops`, which is the pattern: `MIGRATOR.run_to(N, ..)`, seed with `sqlx::raw_sql`, then `MIGRATOR.run(..)`).

```rust
#[tokio::test(flavor = "multi_thread")]
async fn migration_0019_normalises_legacy_display_names_and_adds_the_checks() {
    // Arrange: a database at 0018 (what a v0.21 server left), holding the
    // dirty values 0009 never refused. Raw strings (`r#"..."#`): Rust would
    // read `\u2028` as a malformed escape; `E'..'` makes Postgres read it.
    let db = pg_support::TestDb::start().await;
    MIGRATOR.run_to(18, &db.pool).await.unwrap();
    sqlx::raw_sql(
        r#"INSERT INTO users (name, display_name, created_at) VALUES
               ('long', repeat('x', 85), now()),
               ('ctrl', E'  Alice\tExample\u2028Smith  ', now()),
               ('clean', 'Bob', now());
           INSERT INTO teams (name, display_name, created_at) VALUES
               ('acme', E'Acme\nLtd', now());"#,
    )
    .execute(&db.pool)
    .await
    .unwrap();

    // Act: the rest of the migrations, 0019 included.
    MIGRATOR.run(&db.pool).await.unwrap();

    // Assert: exact normalised values (control characters and U+2028 become
    // spaces, then trim, then the first 80 characters), untouched clean rows,
    // and the CHECKs in force.
    let names: Vec<(String, String)> =
        sqlx::query_as("SELECT name, display_name FROM users ORDER BY name")
            .fetch_all(&db.pool)
            .await
            .unwrap();
    assert_eq!(
        names,
        [
            ("clean".to_string(), "Bob".to_string()),
            ("ctrl".to_string(), "Alice Example Smith".to_string()),
            ("long".to_string(), "x".repeat(80)),
        ]
    );
    let team: (String,) = sqlx::query_as("SELECT display_name FROM teams WHERE name = 'acme'")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(team.0, "Acme Ltd");
    let catalog_default: (String,) =
        sqlx::query_as("SELECT column_default FROM information_schema.columns \
                        WHERE table_name = 'catalogs' AND column_name = 'display_name'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(catalog_default.0, "''::text");
    for table in ["users", "teams"] {
        let refused = sqlx::query(&format!(
            "UPDATE {table} SET display_name = repeat('x', 81) WHERE name IN ('clean', 'acme')"
        ))
        .execute(&db.pool)
        .await;
        assert!(refused.is_err(), "{table}: the CHECK should refuse 81 characters");
    }
}
```

The expected `"Alice Example Smith"` relies on the migration's replace-then-trim order. The tab and U+2028 each become one space, and no run of spaces is collapsed: the source has exactly one character at each spot. If Step 2's SQL changes, change this expectation with it. The test also proves that adding the CHECKs succeeds on a database that held dirty data.

> **Corrected (PR #596 review).** The first draft asserted only on an already-migrated, empty database, which would pass with the normalising `UPDATE`s deleted.

> **Corrected (issue #599).** The shipped test (`src/db.rs`) seeds more rows, for the corrected SQL: Unicode-whitespace trims (`E'\u00a0Carol\u3000'` to `Carol`), a C1 control (`E'Dan\u009fEve'` to `Dan Eve`), the trim after the 80-character cut (`repeat('y', 79) || E' \u0085z'` to 79 `y`), an inner double space kept, and an empty value. It checks that the clean rows' `xmin` is unchanged (not rewritten), that every result re-parses unchanged with `DisplayName::parse`, and that each CHECK refuses by its constraint name on all three tables. Step 1's tests: there is no `seed_team` helper; the tests use `test_db()`'s fixture team `TEST_TEAM`, and the CHECK test covers all three tables by constraint name.

- [ ] **Step 4: Rows and inputs**
  - `Catalog` gains `pub display_name: String` (doc: "Its display name; empty when unset."). Add `c.display_name` to `select_catalogs!`, to the `INSERT` (`$6`) and to its `RETURNING`.
  - `NewCatalog<'a>` gains `pub display_name: &'a DisplayName`.
  - `NewUser<'a>` and `NewTeam<'a>` change `display_name: &'a str` to `&'a DisplayName`. Bind with `.as_str()`.
  - `UserAuth` gains `pub display_name: String`; `user_auth_columns!` becomes `"u.id, u.name, u.display_name, u.email::text AS email, u.is_admin"`.
  - **HTTP bodies stay `String`**: `NewCatalog` (catalogs API) gains `#[serde(default)] display_name: String`. Each handler runs `let display_name = DisplayName::parse(&body.display_name)?;` before building the store struct. Same in `create_user` and `create_team` (`people.rs:509`, `:928`) and in the CLI's `user create` and `team create`.
- [ ] **Step 5: Run the tests, full gates, changelog `m17-task-02-display-name-columns`, commit** `feat(store): display_name on catalogs; validated on every create`.

---

### Task 3: Store writes for display names

**Files:**
- Modify: `src/store/audit.rs` (three actions), `src/store/users.rs`, `src/store/teams.rs`, `src/store/catalogs.rs`, `src/cli.rs` (the existing `catalog update` call site)

- [ ] **Step 1: The audit actions** (append to the `actions!` list, after their siblings):
  - `UserDisplayNameChanged => "user.display_name_changed"` after `UserEmailChanged`;
  - `TeamDisplayNameChanged => "team.display_name_changed"` after `TeamUnsuspended`;
  - `CatalogDisplayNameChanged => "catalog.display_name_changed"` after `CatalogDescriptionChanged`.

  Check `audit.rs`'s tests for a list of every action name (for example, an export or filter test); if one exists, add the three.

- [ ] **Step 2: Write the failing tests**

`src/store/users.rs`:

```rust
#[tokio::test]
async fn set_display_name_changes_it_records_its_length_and_bumps() {
    let db = test_db().await;
    seed_user(&db.pool, "alice", Some("alice@example.com")).await;
    let before = admin_generation(&db.pool).await.unwrap();
    let name = DisplayName::parse("Alice Example").unwrap();
    set_display_name(&db.pool, "alice", &name, &AuditActor::cli(), now()).await.unwrap();
    assert_eq!(find(&db.pool, "alice").await.unwrap().display_name, "Alice Example");
    assert!(admin_generation(&db.pool).await.unwrap() > before);
    let entries = audit_entries(&db.pool, "user.display_name_changed").await;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].details, serde_json::json!({"length": 13}));
    // The same text again records nothing.
    set_display_name(&db.pool, "alice", &name, &AuditActor::cli(), now()).await.unwrap();
    assert_eq!(audit_entries(&db.pool, "user.display_name_changed").await.len(), 1);
}

#[tokio::test]
async fn a_provisioner_may_not_rename_an_admin_user() {
    let db = test_db().await;
    seed_user(&db.pool, "staff", None).await;
    make_admin(&db.pool, "staff").await;
    let refused = set_display_name(
        &db.pool, "staff", &DisplayName::parse("x").unwrap(), &provisioner_actor(&db.pool).await, now(),
    )
    .await;
    assert!(matches!(refused, Err(OkfError::Forbidden { .. })), "{refused:?}");
}
```

(`audit_entries`, `make_admin` and `provisioner_actor` stand for the helpers the neighbouring `set_email` and `set_admin` tests already use. Use those helpers under their real names; add a helper only if none exists.)

`src/store/teams.rs`:

```rust
#[tokio::test]
async fn set_display_name_refuses_a_personal_team() {
    let db = test_db().await;
    seed_user(&db.pool, "alice", None).await;
    let refused = set_display_name(
        &db.pool,
        &TeamDisplayNameChange { team: "alice", display_name: &DisplayName::parse("x").unwrap(), owner: None },
        &AuditActor::cli(),
        now(),
    )
    .await;
    assert!(matches!(refused, Err(OkfError::Refused { .. })), "{refused:?}");
}

#[tokio::test]
async fn an_owner_may_rename_their_team_and_a_member_may_not() {
    let db = test_db().await;
    let alice = seed_user(&db.pool, "alice", None).await;
    let bob = seed_user(&db.pool, "bob", None).await;
    create_team_owned_by(&db.pool, "acme", "alice").await;
    add_member(&db.pool, "acme", "bob").await;
    let name = DisplayName::parse("Acme").unwrap();
    let change = |owner| TeamDisplayNameChange { team: "acme", display_name: &name, owner: Some(owner) };
    set_display_name(&db.pool, &change(&alice.id), &AuditActor::cli(), now()).await.unwrap();
    let refused = set_display_name(&db.pool, &change(&bob.id), &AuditActor::cli(), now()).await;
    assert!(matches!(refused, Err(OkfError::Forbidden { .. })), "{refused:?}");
    assert_eq!(find(&db.pool, "acme").await.unwrap().display_name, "Acme");
}
```

`src/store/catalogs.rs`:

```rust
#[tokio::test]
async fn update_details_changes_either_field_and_records_each() {
    let db = test_db().await;
    seed_catalog(&db.pool, "pg").await;
    let display = DisplayName::parse("Work notes").unwrap();
    let previous = update_details(
        &db.pool,
        &DetailsChange { catalog: "pg", display_name: Some(&display), description: None, owner: None },
        &AuditActor::cli(),
        now(),
    )
    .await
    .unwrap();
    assert_eq!(previous.display_name, "");
    let row = find(&db.pool, "pg").await.unwrap();
    assert_eq!((row.display_name.as_str(), row.description.as_str()), ("Work notes", ""));
    assert_eq!(entries(&db.pool, "catalog.display_name_changed").await.len(), 1);
    assert_eq!(entries(&db.pool, "catalog.description_changed").await.len(), 0);
}

#[tokio::test]
async fn update_details_as_owner_checks_the_owner_team() {
    let db = test_db().await;
    let alice = seed_user(&db.pool, "alice", None).await;
    let bob = seed_user(&db.pool, "bob", None).await;
    seed_catalog_owned_by(&db.pool, "alice").await; // names it "pg", owned by alice's team
    let display = DisplayName::parse("Mine").unwrap();
    let change = |owner| DetailsChange { catalog: "pg", display_name: Some(&display), description: None, owner: Some(owner) };
    update_details(&db.pool, &change(&alice.id), &AuditActor::cli(), now()).await.unwrap();
    let refused = update_details(&db.pool, &change(&bob.id), &AuditActor::cli(), now()).await;
    assert!(matches!(refused, Err(OkfError::Forbidden { .. })), "{refused:?}");
    // An unknown catalog, asked as an owner, is the same refusal: it reveals nothing.
    let unknown = DetailsChange { catalog: "nope", ..change(&alice.id) };
    assert!(matches!(update_details(&db.pool, &unknown, &AuditActor::cli(), now()).await, Err(OkfError::Forbidden { .. })));
}
```

Keep the existing `update_description_*` tests by porting them to `update_details` with `display_name: None` (the behaviours they pin still hold: the old text returned, the same text recording nothing, an unknown catalog refused).

- [ ] **Step 3: The shared owner check** (`src/store/catalogs.rs`). Extract from `set_maintenance`:

```rust
/// Holds `user_id`'s owner membership of catalog `name`'s owner team for the
/// rest of the transaction (`FOR SHARE OF m`), so a concurrent removal or
/// demotion waits for the change rather than racing it.
///
/// # Arguments
/// * `conn` - the change's transaction.
/// * `name` - the catalog.
/// * `user_id` - `users.id` of the would-be owner.
/// * `not_owner` - the refusal to return.
///
/// # Returns
/// The owner team's id.
///
/// # Errors
/// `not_owner()` when the user is no owner (an unknown catalog included);
/// [`OkfError::Database`].
#[tracing::instrument(level = "trace", skip_all, fields(db.system.name = "postgresql", db.operation.name = "catalogs.lock_owner"))]
pub(crate) async fn lock_owner(
    conn: &mut PgConnection,
    name: &str,
    user_id: &str,
    not_owner: impl Fn() -> OkfError,
) -> Result<String, OkfError> {
    sqlx::query_scalar::<_, String>(
        "SELECT m.team_id FROM catalogs c \
         JOIN team_members m ON m.team_id = c.owner_team_id \
         WHERE c.name = $1 AND m.user_id = $2 AND m.role = 'owner' FOR SHARE OF m",
    )
    .bind(name)
    .bind(user_id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(not_owner)
}
```

and make `set_maintenance` call it (its tests must pass unchanged).

- [ ] **Step 4: `catalogs::update_details`** replaces `update_description`:

```rust
/// A change of a catalog's display name and/or description:
/// [`update_details`]' input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DetailsChange<'a> {
    /// The catalog's name.
    pub catalog: &'a str,
    /// The new display name, or `None` to leave it.
    pub display_name: Option<&'a DisplayName>,
    /// The new description, or `None` to leave it.
    pub description: Option<&'a str>,
    /// As [`MaintenanceChange::owner`]: `Some(users.id)` when the caller may
    /// make the change only as an owner of the catalog's owner team.
    pub owner: Option<&'a str>,
}

/// What a catalog's details were before [`update_details`].
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Details {
    /// Its display name.
    pub display_name: String,
    /// Its description.
    pub description: String,
}
```

The function body follows `update_description` and `set_maintenance`:
1. Validate the name. If it is invalid, refuse with `not_owner()` when `owner` is set, else `CatalogNotSelected`.
2. `pool.begin()`, then `audit::lock_people(.., catalogs: &[name])`.
3. If `owner` is set, run `lock_owner(..)`.
4. Run one `UPDATE`:

   ```sql
   UPDATE catalogs c SET display_name = COALESCE($2, c.display_name),
                         description  = COALESCE($3, c.description)
   FROM (SELECT id, display_name, description FROM catalogs WHERE name = $1 FOR NO KEY UPDATE) old
   WHERE c.id = old.id RETURNING c.id, old.display_name, old.description
   ```

   binding `display_name.map(DisplayName::as_str)` and `description`.

   > **Corrected (issue #600).** The `UPDATE` also returns `old.owner_team_id`, and an owner's change is refused (`not_owner()`) when it differs from the team `lock_owner` returned, as `set_maintenance` already does: a catalog moved to another team between the membership read and the row lock must not be renamed by the old team's owner. The shipped statement is in `src/store/catalogs.rs` (`update_details`).
5. `None` from the `UPDATE` → `CatalogNotSelected` (or `not_owner()` when `owner` is set, so an unknown catalog reveals nothing).
6. Record `CatalogDisplayNameChanged` with `{"length": n}` if the display name changed, and `CatalogDescriptionChanged` (as today) if the description changed.
7. If either changed, run `bump_admin_generation` and commit; otherwise drop `tx`.
8. Return `Details`.

The `not_owner` text is `"this needs admin on catalog {name:?}, or an owner of its team signed in to the console"`. **Port every existing caller in this task**, so Task 3 compiles and passes its gates on its own (`grep -rn update_description src tests`):
- the CLI's `catalog update` calls `update_details` with `display_name: None, description: Some(&description), owner: None`, and prints what it printed before;
- the REST handler `update_catalog` (`src/http/api/catalogs.rs:300`) does the same, using `previous.description` as its `previous`; its body, permission check and answer stay exactly as they are until Task 5;
- the poll test at `src/app.rs:1526` and any other test call do the same.

> **Corrected (PR #596 review).** The first draft ported only the CLI, which left the REST handler and `src/app.rs`'s test calling a removed function until Task 5.

- [ ] **Step 5: `users::set_display_name`** follows `set_email` (`src/store/users.rs:639-698`):
  1. Lock the target with `lock_target(&mut tx, name, RowLock::Update, actor)`.
  2. Call `audit::refuse_admin_target(actor, is_admin, name)?`.
  3. Run `UPDATE users u SET display_name = $2 FROM (SELECT id, display_name FROM users WHERE id = $1) old WHERE u.id = old.id RETURNING old.display_name <> u.display_name`.
  4. If it changed, `bump_admin_generation` and record `user_entry(Action::UserDisplayNameChanged, &user_id, json!({"length": n}))`.

  The personal team's `display_name` column is left alone: its label is derived (Decision 3 of the spec).

- [ ] **Step 6: `teams::set_display_name`** takes `TeamDisplayNameChange { team, display_name, owner: Option<&str /* users.id */> }`:
  1. Lock people with `teams: &[team]`.
  2. Read `SELECT id, personal_of IS NOT NULL FROM teams WHERE name = $1 FOR NO KEY UPDATE`. None → `unknown(team)`; personal → `Refused { reason: "a personal team's label follows its user; change the user's display name" }`.
  3. If `owner` is set, run `SELECT 1 FROM team_members WHERE team_id = $1 AND user_id = $2 AND role = 'owner' FOR SHARE`. None → `Forbidden { reason: "this needs an owner of team {team:?} or an admin that reaches every catalog" }`.
  4. Run the update, `RETURNING old <> new`.
  5. If it changed: `record_team(.., TeamEvent { action: TeamDisplayNameChanged, team, person: None, details: json!({"length": n}) }, now)`, then `bump_admin_generation`, the order of every other team writer (`suspend`, `set_member`). *(Corrected, issue #600: the draft bumped first.)*

  Lock order as in `lessons.md`: the people, then the team row, then the membership, then `server_state`.

- [ ] **Step 7: Run the tests, full gates, changelog `m17-task-03-display-name-writes`, commit** `feat(store): set display names; owners rename their teams and catalogs`.

---

### Task 4: The label directory in the key cache

**Files:**
- Create: `src/labels/directory.rs`
- Modify: `src/labels/mod.rs` (`pub mod directory;`), `src/auth.rs` (`KeyCache.labels`, `KeyCache::read`, `Keys::labels`), `src/store/teams.rs` (`SuspendedCatalog` gains labels)

- [ ] **Step 1: Write the failing tests** (`src/labels/directory.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Labels {
        Labels::from_rows(
            vec![
                UserRow { name: "u-1".into(), display_name: "Alice".into(), email: Some("alice@example.com".into()) },
                UserRow { name: "u-2".into(), display_name: "".into(), email: Some("bob@example.com".into()) },
            ],
            vec![
                TeamRow { name: "u-1".into(), display_name: "".into(), personal_of: Some("u-1".into()) },
                TeamRow { name: "acme".into(), display_name: "Acme".into(), personal_of: None },
            ],
            vec![
                CatalogRow { name: "m-1".into(), display_name: "".into(), description: "Personal memory".into(), owner_team: Some("u-1".into()) },
                CatalogRow { name: "m-2".into(), display_name: "Personal memory".into(), description: "".into(), owner_team: Some("acme".into()) },
            ],
            vec![("u-1".into(), "acme".into()), ("u-1".into(), "u-1".into())],
        )
    }

    #[test]
    fn labels_follow_the_rule() {
        let labels = sample();
        assert_eq!(labels.user("u-2"), Some("bob"));
        assert_eq!(labels.team("u-1"), Some("Alice's team"));
        assert_eq!(labels.catalog("m-1"), Some("Personal memory"));
        assert_eq!(labels.user("u-9"), None);
    }

    #[test]
    fn an_actor_resolves_only_when_it_is_a_user_slug() {
        let labels = sample();
        assert_eq!(labels.actor("u-1"), Some("Alice"));
        assert_eq!(labels.actor("claude-code/model"), None);
    }

    #[test]
    fn duplicate_catalog_labels_are_told_apart_by_owner_team() {
        let labels = sample();
        let choices = labels.catalog_choices(["m-1", "m-2"]);
        assert_eq!(choices[0].1, "Personal memory (Alice's team)");
        assert_eq!(choices[1].1, "Personal memory (Acme)");
        let alone = labels.catalog_choices(["m-2"]);
        assert_eq!(alone[0].1, "Personal memory");
    }

    #[test]
    fn same_owner_duplicates_fall_back_to_the_id() {
        let labels = Labels::from_rows(
            vec![],
            vec![TeamRow { name: "acme".into(), display_name: "Acme".into(), personal_of: None }],
            vec![
                CatalogRow { name: "m-1".into(), display_name: "Notes".into(), description: "".into(), owner_team: Some("acme".into()) },
                CatalogRow { name: "m-2".into(), display_name: "Notes".into(), description: "".into(), owner_team: Some("acme".into()) },
            ],
            vec![],
        );
        let choices = labels.catalog_choices(["m-1", "m-2"]);
        assert_eq!(choices[0].1, "Notes (id: m-1)");
        assert_eq!(choices[1].1, "Notes (id: m-2)");
    }

    #[test]
    fn a_literal_label_shaped_like_a_fallback_still_gets_a_distinct_choice() {
        // m-3's own display name is exactly m-1's fallback text.
        let labels = Labels::from_rows(
            vec![],
            vec![TeamRow { name: "acme".into(), display_name: "Acme".into(), personal_of: None }],
            vec![
                CatalogRow { name: "m-1".into(), display_name: "Notes".into(), description: "".into(), owner_team: Some("acme".into()) },
                CatalogRow { name: "m-2".into(), display_name: "Notes".into(), description: "".into(), owner_team: Some("acme".into()) },
                CatalogRow { name: "m-3".into(), display_name: "Notes (id: m-1)".into(), description: "".into(), owner_team: Some("acme".into()) },
            ],
            vec![],
        );
        let choices = labels.catalog_choices(["m-1", "m-2", "m-3"]);
        assert_eq!(choices[0].1, "Notes (id: m-1)");
        assert_eq!(choices[1].1, "Notes (id: m-2)");
        assert_eq!(choices[2].1, "Notes (id: m-1) (id: m-3)");
    }

    #[test]
    fn owner_teams_sharing_a_label_fall_back_to_the_id() {
        let labels = Labels::from_rows(
            vec![],
            vec![
                TeamRow { name: "acme-1".into(), display_name: "Acme".into(), personal_of: None },
                TeamRow { name: "acme-2".into(), display_name: "Acme".into(), personal_of: None },
            ],
            vec![
                CatalogRow { name: "m-1".into(), display_name: "Notes".into(), description: "".into(), owner_team: Some("acme-1".into()) },
                CatalogRow { name: "m-2".into(), display_name: "Notes".into(), description: "".into(), owner_team: Some("acme-2".into()) },
                CatalogRow { name: "m-3".into(), display_name: "Other".into(), description: "".into(), owner_team: Some("acme-1".into()) },
            ],
            vec![],
        );
        let choices = labels.catalog_choices(["m-1", "m-2", "m-3"]);
        assert_eq!(choices[0].1, "Notes (id: m-1)");
        assert_eq!(choices[1].1, "Notes (id: m-2)");
        assert_eq!(choices[2].1, "Other");
    }

    #[hegel::test]
    fn catalog_choices_are_always_distinct(tc: hegel::TestCase) {
        use hegel::generators;
        // A small pool of labels, including ones shaped like the generated
        // suffixes ("(Same)", "(id: m-N)"), so every kind of collision is
        // common: plain duplicates, owner-suffix collisions and literal
        // labels equal to another catalog's fallback.
        const POOL: [&str; 6] =
            ["L0", "L1", "L0 (Same)", "L0 (id: m-0)", "L0 (id: m-1)", "L0 (id: m-1) (id: m-2)"];
        let count = tc.draw(generators::integers::<usize>().min_value(1).max_value(8));
        let rows: Vec<CatalogRow> = (0..count)
            .map(|i| {
                let label = tc.draw(generators::integers::<usize>().max_value(POOL.len() - 1));
                let team = tc.draw(generators::integers::<u8>().max_value(1));
                CatalogRow {
                    name: format!("m-{i}"),
                    display_name: POOL[label].to_string(),
                    description: String::new(),
                    owner_team: Some(format!("t-{team}")),
                }
            })
            .collect();
        let teams = (0..2)
            .map(|t| TeamRow { name: format!("t-{t}"), display_name: "Same".into(), personal_of: None })
            .collect();
        let slugs: Vec<String> = rows.iter().map(|row| row.name.clone()).collect();
        let labels = Labels::from_rows(vec![], teams, rows, vec![]);
        let choices = labels.catalog_choices(slugs.iter().map(String::as_str));
        let distinct: std::collections::HashSet<&str> = choices.iter().map(|(_, label)| label.as_str()).collect();
        assert_eq!(distinct.len(), choices.len());
    }

    #[test]
    fn the_directory_keeps_a_catalogs_raw_display_name() {
        let labels = sample();
        // m-1 has no display name; its label comes from the description.
        assert_eq!(labels.catalog_display_name("m-1"), Some(""));
        assert_eq!(labels.catalog("m-1"), Some("Personal memory"));
        assert_eq!(labels.catalog_display_name("m-2"), Some("Personal memory"));
        assert_eq!(labels.catalog_display_name("m-9"), None);
    }

    #[test]
    fn teams_of_lists_a_users_teams() {
        let labels = sample();
        let mut teams: Vec<&str> = labels.teams_of("u-1").collect();
        teams.sort_unstable();
        assert_eq!(teams, ["acme", "u-1"]);
    }
}
```

- [ ] **Step 2: Implement `src/labels/directory.rs`**

```rust
//! [`Labels`]: every user's, team's and catalog's label, by slug, loaded
//! with the key cache (plan Decision 1). Every input to a label bumps
//! `admin_generation`, so this reloads exactly when a label can change.

use std::collections::HashMap;
use std::sync::Arc;

use sqlx::PgPool;

use super::{catalog_label, team_label, user_label};
use crate::error::OkfError;

/// A `users` row as the directory reads it.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct UserRow {
    /// Its slug.
    pub name: String,
    /// Its display name.
    pub display_name: String,
    /// Its email, if set.
    pub email: Option<String>,
}

/// A `teams` row as the directory reads it.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct TeamRow {
    /// Its slug.
    pub name: String,
    /// Its display name.
    pub display_name: String,
    /// The user slug whose personal team it is.
    pub personal_of: Option<String>,
}

/// A `catalogs` row as the directory reads it.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct CatalogRow {
    /// Its slug.
    pub name: String,
    /// Its display name.
    pub display_name: String,
    /// Its description.
    pub description: String,
    /// Its owner team's slug.
    pub owner_team: Option<String>,
}

/// One catalog's label and owner.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CatalogEntry {
    // The stored value: Tasks 5 and 7 return it as `display_name`, and a
    // label cannot tell whether it was unset.
    display_name: Arc<str>,
    label: Arc<str>,
    owner_team: Option<Arc<str>>,
}

/// Every label, by slug. Users and teams are separate maps: a user and its
/// personal team share a slug.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Labels {
    users: HashMap<String, Arc<str>>,
    teams: HashMap<String, Arc<str>>,
    catalogs: HashMap<String, CatalogEntry>,
    // user slug -> the slugs of its teams.
    memberships: HashMap<String, Vec<String>>,
}

impl Labels {
    /// Reads every user, team, catalog and membership.
    ///
    /// # Arguments
    /// * `pool` - the database.
    ///
    /// # Returns
    /// The directory.
    ///
    /// # Errors
    /// [`OkfError::Database`].
    #[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "labels.load"))]
    pub async fn load(pool: &PgPool) -> Result<Self, OkfError> {
        let (users, teams, catalogs, memberships) = tokio::try_join!(
            sqlx::query_as::<_, UserRow>("SELECT name, display_name, email::text AS email FROM users")
                .fetch_all(pool),
            sqlx::query_as::<_, TeamRow>(
                "SELECT t.name, t.display_name, u.name AS personal_of \
                 FROM teams t LEFT JOIN users u ON u.id = t.personal_of",
            )
            .fetch_all(pool),
            sqlx::query_as::<_, CatalogRow>(
                "SELECT c.name, c.display_name, c.description, t.name AS owner_team \
                 FROM catalogs c LEFT JOIN teams t ON t.id = c.owner_team_id",
            )
            .fetch_all(pool),
            sqlx::query_as::<_, (String, String)>(
                "SELECT u.name, t.name FROM team_members m \
                 JOIN users u ON u.id = m.user_id JOIN teams t ON t.id = m.team_id",
            )
            .fetch_all(pool),
        )?;
        Ok(Self::from_rows(users, teams, catalogs, memberships))
    }

    /// Builds the directory from rows (the unit tests' entry point).
    ///
    /// # Arguments
    /// * `users`, `teams`, `catalogs` - the rows.
    /// * `memberships` - `(user slug, team slug)` pairs.
    ///
    /// # Returns
    /// The directory.
    #[tracing::instrument(level = "debug", skip_all)]
    pub fn from_rows(
        users: Vec<UserRow>,
        teams: Vec<TeamRow>,
        catalogs: Vec<CatalogRow>,
        memberships: Vec<(String, String)>,
    ) -> Self {
        let users: HashMap<String, Arc<str>> = users
            .into_iter()
            .map(|row| {
                let label = user_label(&row.display_name, row.email.as_deref());
                (row.name, Arc::from(label))
            })
            .collect();
        let teams = teams
            .into_iter()
            .map(|row| {
                // A personal team whose user is gone (never: RESTRICT) would
                // fall back to its own display name.
                let owner = row.personal_of.as_deref().and_then(|user| users.get(user));
                let label = team_label(&row.display_name, owner.map(|label| &**label));
                (row.name, Arc::from(label))
            })
            .collect();
        let catalogs = catalogs
            .into_iter()
            .map(|row| {
                let entry = CatalogEntry {
                    label: Arc::from(catalog_label(&row.display_name, &row.description)),
                    display_name: Arc::from(row.display_name),
                    owner_team: row.owner_team.map(Arc::from),
                };
                (row.name, entry)
            })
            .collect();
        let mut by_user: HashMap<String, Vec<String>> = HashMap::new();
        for (user, team) in memberships {
            // `entry(..).or_default()` is Python's `setdefault(key, [])`.
            by_user.entry(user).or_default().push(team);
        }
        Self { users, teams, catalogs, memberships: by_user }
    }
}
```

Add the accessors, each a trivial-accessor `uninstrumented_fn` allow:
- `user(&self, slug) -> Option<&str>`, `team(..)`, `catalog(..)`;
- `actor(&self, actor) -> Option<&str>` (= `self.user(actor)`; doc comment cites plan Decision 5);
- `catalog_owner(&self, slug) -> Option<&str>`;
- `catalog_display_name(&self, slug) -> Option<&str>`: the stored display name (empty when unset), which Tasks 5 and 7 return as `display_name`;
- `teams_of(&self, user) -> impl Iterator<Item = &str>`;
- `catalog_choices<'a>(&self, slugs: impl IntoIterator<Item = &'a str>) -> Vec<(String, String)>` returns `(slug, shown text)` in the input order, every shown text distinct:
  1. **Owner suffix.** Count the labels among the given slugs. A label that occurs once is shown as is; a duplicate becomes `{label} ({owner team label})`, or `{label} (no team)` for an unassigned catalog.
  2. **Fallback until unique.** Repeat until no shown text occurs twice among the complete final set (every entry, suffixed or not): for every text that does, switch each member of that group not yet on its fallback to its fallback `{label} (id: {slug})`, built from its own label.

  Why this ends with unique choices:
  - **Fallbacks never collide with each other.** A slug is `[a-z0-9-]+`, so it contains neither a space nor `)`. The text after the last ` (id: ` of a fallback is therefore its slug and a closing `)`, and two different slugs give two different texts.
  - **Every pass shrinks the work.** Each pass that finds a duplicate moves at least one entry onto its fallback, and an entry never leaves it. So the loop runs at most once per entry.
  - **So nothing is left duplicated.** When the loop stops, no group of two or more remains: any such group would have to consist only of fallbacks, which cannot collide.
  - **Labels shaped like fallbacks are covered.** A literal label can equal another entry's fallback text, for example `Notes (id: m-1)`. It then sits in a duplicate group and moves onto its own fallback.

  Keep the input order, and use a `HashMap<&str, usize>` per pass. The lists are a person's catalogs, so a few passes over a few entries cost nothing.

  An unknown slug is returned with the slug as its text: it is a catalog created since the last load, and showing its id beats hiding it. It takes part in the duplicate check like any other text.

  > **Corrected (PR #596 review, twice).** The first draft stopped at the owner-team suffix, which does not separate two catalogs of one team or teams that share a label. The second added one slug pass, which a literal display name shaped like a suffix could still collide with. The loop above checks the complete final set.

- [ ] **Step 3: Load it with the key cache** (`src/auth.rs`)
  - `KeyCache` gains `labels: Arc<Labels>` (default empty).
  - `KeyCache::read` adds `Labels::load(pool)` as a fifth `try_join!` arm and stores it with a `with_labels(..)` builder like `with_users`.
  - `Keys::labels(&self) -> Arc<Labels>` clones the `Arc` like `suspensions()` does.

  Test in `src/auth.rs` tests: after `users::create` and `Keys::reload`, `keys.labels().user("alice")` is `Some("alice")` for a user with email `alice@example.com`.

  > **Corrected (issue #601, 2026-11-17).** `users::NewUser` has no email, so the shipped test (`the_cache_loads_labels_and_reloads_them_after_a_create`) creates `alice` with display name "Alice Example" and a catalog "Team notes", reloads, and asserts the user, personal-team and catalog labels, the catalog owner and `teams_of`. The suspension test also asserts that `SuspendedCatalog.catalog_label` and `team_label` equal the directory's labels. `suspended_catalogs` fills both fields with the slugs; `KeyCache::read` overwrites them from the directory.

- [ ] **Step 4: Suspension labels** (`src/store/teams.rs`). `SuspendedCatalog` gains `pub catalog_label: String` and `pub team_label: String`. `suspended_catalogs` keeps its query. `KeyCache::read` fills the two fields from the loaded `Labels` after the `try_join!`, falling back to the slugs only if a row is missing. The rows come from one load, so that happens only in a race, and the slug is honest then. Every `SuspendedCatalog { .. }` literal in tests gains the fields.
- [ ] **Step 5: Run the tests, full gates, changelog `m17-task-04-label-directory`, commit** `feat(labels): the label directory, loaded with the key cache`.

---

### Task 5: REST: labels on views, `GET /labels`, and the edit routes

**Files:**
- Create: `src/http/api/labels.rs`
- Modify: `src/http/api/mod.rs` (mount; `PROVISIONER_ROUTES`), `people.rs`, `me.rs`, `catalogs.rs`, `identity.rs`

- [ ] **Step 1: Write the failing route tests.** Use the tests module pattern of each file (`people.rs` has `json!` request helpers at `:1990-2000`):
  - `user_view_has_a_label`: `POST /users {"name":"alice","display_name":"Alice Example"}` → `body["label"] == "Alice Example"`; a user without a display name but with an email → the local part.
  - `team_view_labels_a_personal_team_after_its_user`: `GET /teams/alice` → `label == "Alice Example's team"`; each `members[]` entry has `label`.
  - `catalog_summary_has_display_name_and_label`.
  - `patch_me_sets_my_display_name` (session); `patch_me_refuses_a_key_without_a_user` (404, like `GET /me`).
  - `patch_me_refuses_an_over_long_name` (422, `"limit": "display_name"`).
  - `account_lists_owned_team_details`: `GET /me` for an owner of shared team `acme` (display name "Acme") returns `owned_team_details` with `{name: "acme", display_name: "Acme", label: "Acme", personal: false}` and its personal team with `personal: true`; `owned_teams` is unchanged.
  - `catalog_summary_display_name_is_the_stored_value`: a catalog with only a description has `display_name: ""` and `label` equal to the description.
  - `patch_user_is_for_managers_only`: admin → 200; provisioner → 200; scoped admin → 403; session → 403.
  - `patch_team_owner_session_ok_member_403_personal_409`.
  - `patch_catalog_owner_session_may_rename_its_teams_catalog`: an owner-only write-ceiling session → 200 and `label` is the new name; a member → 403; a suspended team's owner → 403 `Suspended`.
  - `patch_catalog_admin_may_still_change_the_description` (the old body still works).
  - `provisioner_routes_include_the_three_patches`: extend the existing allow-list test.
  - `labels_scope`:
    - an every-catalog admin sees all users, teams and catalogs;
    - a session member sees only itself, its teams, its reachable catalogs and those catalogs' owner teams;
    - a scoped key without a user sees only its catalogs and their owner teams.

- [ ] **Step 2: Additive view fields**
  - **`UserView`:** `pub label: String` = `user_label(&display_name, email.as_deref())`, computed in `From<UserRecord>`.
  - **`TeamView`:** `pub label: String`. `From<TeamRecord>` cannot see the user, so replace it with `TeamView::of(record, &Labels)`, computing `team_label(&display_name, personal_of.and_then(|user| labels.user(user)))`. Each `MemberView` gains `pub label: String` = `labels.user(&member.user)`, falling back to the slug only if missing (a user created since the last load). Call sites pass `&app.keys().labels()`. The handlers already `settle` after a write, so the directory is fresh.
  - **`CatalogSummary`:** `pub display_name: String`, `pub label: String`, `pub owner_team_label: Option<String>`. Read them from `app.keys().labels()` in `of` (pass `&Labels` in): `catalog_display_name`, `catalog` and the owner team's `team`. The create handler builds them from the row with `catalog_label`.
  - **`AccountView` (`me.rs`):**
    - `pub label: String` from the record.
    - `pub owned_team_details: Vec<OwnedTeam>`, beside the existing `owned_teams: Vec<String>` (kept as is). Each `OwnedTeam { name, display_name, label, personal: bool }` is read with one query on `teams` for the names in `owned_teams`. The Account page needs the current display name to prefill its team editor (Task 10a), and the Teams page is admin-only.
  - **`WhoAmI` (`identity.rs`):** `pub user_label: Option<String>` from `labels.user(user)`.

  Keep every existing field and its meaning.

- [ ] **Step 3: `GET /api/v1/labels`** (`src/http/api/labels.rs`)

```rust
/// `GET /labels`' answer: the labels the caller may see, by slug.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub(crate) struct LabelsView {
    /// User slug -> label.
    pub users: BTreeMap<String, String>,
    /// Team slug -> label.
    pub teams: BTreeMap<String, String>,
    /// Catalog slug -> label, told apart where two share one.
    pub catalogs: BTreeMap<String, String>,
}
```

What the handler returns:
- **An every-catalog admin** (`require_every_catalog(&principal).is_ok()`) gets everything: every user, every team, and `catalog_choices` over every catalog.
- **Anyone else** gets:
  - **catalogs:** the loaded catalogs it `reaches`, through `catalog_choices`;
  - **teams:** its user's `teams_of`, plus each reachable catalog's `catalog_owner`;
  - **users:** its own user only.
- **A provisioner** is refused before the handler (it is not in the allow-list).

Mount it with `.route("/labels", get(labels))`. Test the scope as Step 1 says.

- [ ] **Step 4: The edit routes**
  - **`PATCH /me`** (`me.rs`): body `{display_name: String}` (`deny_unknown_fields`).
    - A principal without `user` → 404 `NotFound { path: "me" }`, as `account`.
    - Otherwise parse, then `users::set_display_name(.., name, .., &actor, now)`. The caller is that user, so `refuse_admin_target` does not trigger: it refuses only provisioners.
    - `settle`, then return `account` (the same handler body; extract `account_view(&app, &principal)`).
  - **`PATCH /users/{name}`** (`people.rs`): `require_manager`, parse, `users::set_display_name`, `settle`, return `UserView`.
  - **`PATCH /teams/{name}`** (`people.rs`). Work out `owner` with the tuple match from `set_maintenance` (`catalogs.rs:403-410`), but with `require_manager` as the first arm, so a provisioner passes. Then `teams::set_display_name`, `settle`, and return `TeamView::of`.
  - **`PATCH /catalogs/{catalog}`** (`catalogs.rs`). The body becomes:

    ```rust
    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct DetailsBody {
        /// The new display name; left as it is when absent.
        display_name: Option<String>,
        /// The new description; left as it is when absent.
        description: Option<String>,
    }
    ```

    1. A body with neither field is a 422: `OkfError::LimitExceeded { limit: "body", reason: "give display_name, description or both" }`.
    2. Work out who is asking:
       - `principal.provisioner` → allowed;
       - `principal.require(Access::Admin, &catalog)` ok → allowed;
       - else a `Session { user_id }` → owner path, which first refuses a suspended catalog (`if let Some(s) = principal.suspension(&catalog) { return Err(OkfError::Suspended { .. }) }`, built as `Principal::require` builds it; plan Decision 6), then `owner = Some(user_id)`;
       - else → the original refusal.
    3. Call `update_details`.
    4. The answer is `DetailsChanged { name, previous, description, previous_display_name, display_name, label }`. It is a strict superset of today's `DescriptionChanged`: `name`, `previous` (still the old **description**, as a string) and `description` keep their names, types and meaning, and the three new fields are added. External clients of this public route may read `previous`, so its shape never changes (plan Decision 2).

       > **Corrected (PR #596 review).** The first draft allowed `previous` to become an object when no in-repo reader used it; absence of in-repo readers says nothing about external clients.
  - **`POST /catalogs`** passes `display_name` (Task 2 already parsed it).
  - **`PROVISIONER_ROUTES`** gains `("PATCH", "/api/v1/users/{name}")`, `("PATCH", "/api/v1/teams/{name}")` and `("PATCH", "/api/v1/catalogs/{catalog}")`. The provisioner arm of the catalog PATCH is explicit (step 2 above); it cannot reach catalog content, and this route touches only the row.
  > **Corrected (issue #602, 2026-11-17).** What shipped differs from the text above in these ways; none changes a later task's inputs:
  > - `owned_team_details` is a `Vec<OwnedTeamView>` in `me.rs`. `teams::owned_names` was replaced by `teams::owned` (store type `OwnedTeam { name, display_name, personal }`), one query that gives both `owned_teams` and the details; the label is `team_label` over the user's own label. Task 10a reads `account.owned_team_details` exactly as described.
  > - `Labels` gained `users()`, `teams()` and `catalog_slugs()` iterators, for the every-catalog admin's `GET /labels`.
  > - The suspended-owner refusal calls `Principal::check_not_suspended` (it does not build `OkfError::Suspended` itself, so Task 7's new fields need no change here), and only when the session reaches the catalog or belongs to its owner team: a session outside both gets the store's `Forbidden`, so it cannot probe which catalogs are suspended.
  > - `PATCH /me` answers 404 for a credential without a user (as `GET /me`) and 403 for a key or OAuth token that has one: a key is not the person's console session.
  > - The route tests are in `src/http/api/labels_tests.rs` and `provisioner_tests.rs`, not in each handler file.
- [ ] **Step 5: Run the tests, full gates, changelog `m17-task-05-rest-labels`, commit** `feat(api): labels on views, GET /labels, display-name edits`.

---

### Task 6: CLI

**Files:** `src/cli.rs`, `src/cli/people.rs`

- [ ] **Step 1: Failing tests** in the CLI test pattern already used for `user create` and `catalog update` (`src/cli/people.rs` tests; `src/cli.rs` tests or `tests/admin_parity.rs`):
  - `user update alice --display-name "Alice Example"` prints `updated user alice display name: "" -> "Alice Example"`;
  - `team update acme --display-name Acme` does the same for a team;
  - `team update alice ...` on a personal team fails with the store's refusal text;
  - `catalog update pg --display-name "Work notes"` prints `updated catalog pg display name: "" -> "Work notes"`;
  - `catalog update pg` with neither flag fails (clap `ArgGroup` with `required = true, multiple = true`);
  - `catalog create pg --owner acme --display-name "Work notes"` stores it.
- [ ] **Step 2: Implement.**
  - `UserCommand::Update { name, #[arg(long)] display_name: String }` and `TeamCommand::Update { .. }` likewise.
  - `CatalogCommand::Update` makes `description` an `Option<String>`, adds `display_name: Option<String>`, and puts both in an `ArgGroup` (`#[group(required = true, multiple = true)]` on a flattened struct, or `#[command(group(..))]`). It prints one line per field changed, as above.
  - `CatalogCommand::Create` gains `--display-name` (default `""`).
  - Everything calls the Task 3 store functions with `AuditActor::cli()` and `owner: None`, and parses with `DisplayName::parse`.
- [ ] **Step 3: Gates, changelog `m17-task-06-cli`, commit** `feat(cli): update display names`.

> **Corrected (issue #603, 2026-11-17).** The store's `users::set_display_name` and `teams::set_display_name` return `()`, so the CLI reads the old value with `users::find` / `teams::find` just before the write to print `"" -> "Alice Example"`. `catalog update` prints through a pure `describe_details_change` in `src/cli.rs` so the lines are testable; the catalog tests assert stored state plus that function's exact text. Tests live in `src/cli.rs` and `src/cli/people.rs`.

---

### Task 7: MCP surface

**Files:** `src/mcp/catalogs.rs`, `documents.rs`, `history.rs`, `changesets.rs`, `search.rs`, `reports.rs`, `context.rs`, `mod.rs` (`INSTRUCTIONS`); `src/mcp/maintenance/*.rs` (param docs); `src/error.rs` (`suspended_message`); `docs/reference/tools.md` (regenerated)

- [ ] **Step 1: Failing tests** (`src/mcp/testing.rs` patterns, `TestApp`):
  - `list_catalogs_carries_display_name_and_label`: a catalog with display name "Work notes" → `display_name == "Work notes"`, `label == "Work notes"`. Without one → `label` is its description.
  - `list_catalogs_names_a_suspended_team_by_label`: `suspended.team_label == "Acme"`.
  - `history_resolves_a_session_actor_to_a_label`: a revision whose actor is `alice` (a user with display name "Alice Example") gets `actor_label: "Alice Example"`. A revision by actor `claude-code/x` gets `actor_label: null`.
  - `get_concept_and_open_changeset_carry_catalog_label`.
  - `the_suspended_error_names_label_and_id`: the message is `catalog "Work notes" (m-1) is unavailable: its team "Acme" (acme) is suspended`.
  - `an_unknown_catalog_error_is_unchanged`: `catalog "nope" does not exist or is not available`, exactly as before.
  - The existing generated-docs test fails until `docs/reference/tools.md` is regenerated: that is the check that the output schemas changed.
- [ ] **Step 2: A context helper.** `ToolContext::labels(&self) -> Arc<Labels>` (from the app's key cache) and `ToolContext::catalog_label(&self, slug) -> String` (the label, or the slug when missing).
- [ ] **Step 3: Output fields.** Each is additive, with a doc comment (it becomes the schema description):
  - `CatalogInfo`: `display_name: String` and `label: String`. `SuspensionInfo` gains `team_label: String` (it is built `From<&SuspendedCatalog>`, which now carries it).
  - `Concept` (`documents.rs:88`), `History` (`history.rs:58`), `Opened` (`changesets.rs:218`), `ListedChangeset` (`:51`), search hits and `SkippedCatalog` (`search.rs:77`), and the report outputs (`reports.rs:69,95,118,139,158`): each gains `catalog_label: String` next to `catalog`.
  - `Revision` and `ListedChangeset` gain `actor_label: Option<String>` (`labels.actor(&actor).map(str::to_string)`; plan Decision 5).
- [ ] **Step 4: Parameter descriptions.** Every doc comment that says the catalog "name" becomes "The catalog id (the `name` from `list_catalogs`)". This covers each `catalog: Option<String>`, the `maintenance_*` `catalog: String`, and `SearchParams.catalogs`. Keep each comment's existing defaults text.
- [ ] **Step 5: `INSTRUCTIONS`** gains, before "To write":

  > Catalogs, users and teams each have an id (`name`) and a human `label`. Say the label when talking to people, pass the id in tool calls, and never ask a person to type an id.

- [ ] **Step 6: The suspended message.**
  - `OkfError::Suspended` gains `catalog_label: String, team_label: String`.
  - `suspended_message(catalog, catalog_label, team, team_label, notice)` writes `catalog {catalog_label:?} ({catalog}) is unavailable: its team {team_label:?} ({team}) is suspended`.
  - Every construction site fills the labels from `SuspendedCatalog` (`grep -rn "OkfError::Suspended {" src`).
  - The other messages in `error.rs` keep echoing only the caller's value or the slug (spec Part 4).
> **Corrected (issue #604, 2026-11-17).** `SuspendedCatalog` already carried `catalog_label` and `team_label` from Task 4, so Step 6 only threads them into `OkfError::Suspended` (the JSON error body is unchanged). Search hits are wrapped as `LabeledHit { catalog_label, #[serde(flatten)] hit: CatalogHit }` in `src/mcp/search.rs`, so `okf::search` stays label-free. The `maintenance_*` results gain no `catalog_label`: the investigator client parses them with `deny_unknown_fields` and pins their schemas, so only their parameter descriptions change. The `publish`, `discard` and `pending_changes` results also gain `catalog_label`. The new tests live in `src/mcp/labels_tests.rs`.

- [ ] **Step 7: Regenerate `docs/reference/tools.md`** with the generating test's documented command (see the doc comment at `src/mcp/mod.rs:65` and the test that compares it). Then run gates, write changelog `m17-task-07-mcp-labels`, and commit `feat(mcp): labels beside ids in tool outputs`.

---

### Task 8: Emails, sign-in, consent, passkeys

**Files:** `src/mail/invite.rs`, `src/store/unlocks.rs`, `src/http/signin.rs`, `src/oauth/consent.rs`, `src/oauth/authorize.rs`, `src/http/passkeys.rs`

- [ ] **Step 1: Failing tests**
  - `invite.rs`: the queued setup template for a user with display name "Alice Example" has `user == "Alice Example"`; for one with only `alice@example.com` it is `"alice"`. Read the queued row's JSON as the existing invite test does.
  - `unlocks.rs`: the queued `Unlocked` template's `catalog` is the catalog label.
  - `signin.rs`: the setup page HTML holds `Welcome, <strong>Alice Example</strong>`, and a display name `<b>x</b>` renders escaped.
  - `consent.rs`:
    - `user_page_html` shows `Signed in as <strong id="consent-user">Alice Example</strong>`;
    - a catalog row's visible text is its label while `name="grant.m-1"` and `value="m-1"` keep the slug;
    - two catalogs sharing a label show the owner-team suffix.
  - `consent.rs` summary: `GrantSummary.catalogs` joins labels; `user` is the label.
  - `passkeys.rs`: the registration options' `user.displayName` is the label and `user.name` stays the email.
- [ ] **Step 2: Implement**
  - **`invite.rs:79-82`:** `user: user_label(&issued.user.display_name, issued.user.email.as_deref())`. `UserAuth` carries `display_name` since Task 2.
  - **`unlocks.rs:568`:** before the template, read `SELECT display_name, description FROM catalogs WHERE id = $1` in the same transaction, and fill `catalog: catalog_label(..)`.
  - **`signin.rs:906-912`:** `user: &user_label(&user.display_name, user.email.as_deref())`. Bind the label to a local first; the view borrows it.
  - **`consent.rs`:**
    - `UserPage` gains `pub user_label: &'a str` and `pub catalog_labels: &'a [(String, String)]`, the `catalog_choices` of the ceiling's keys. Keep `user` if anything else reads it, otherwise rename the field.
    - The row builder iterates `catalog_labels` and looks up the level in `ceiling`. Visible text is `escape(label)`; ids and names stay `escape(slug)`; the `aria-label` is `"Access to {label}"`.
    - The default-catalog `<option value="{slug}">{label}</option>`.
    - The caller in `authorize.rs` builds both from `app.keys().labels()`.
  - **`authorize.rs:339` `summary`:** it needs `&Labels`. `catalogs` becomes the labels joined with ", ", from `catalog_choices`, and `default_catalog` and `user` become labels.
  - **`passkeys.rs:539-546`:** `let display = user_label(&user.display_name, user.email.as_deref());`, then pass `&display` as the third argument.
- [ ] **Step 3: Gates, changelog `m17-task-08-rendered-labels`, commit** `feat: emails, sign-in, consent and passkeys name people and catalogs by label`.

---

### Task 9: The new catalog's heading and the export filename

**Files:** `src/okf/ops/log_index.rs`, `src/store/catalogs.rs` (`write_starter_files`), `src/http/archives.rs`, the spec (Decision 3 correction)

- [ ] **Step 1: Failing tests**
  - The `starter_files` doc test changes to `starter_files("Systems", "About systems.", now)` and asserts `# Systems\n\nAbout systems.\n`.
  - A store test: a catalog created with display name "Work notes" has a root `index.md` with `# Work notes`, and one without a display name has `# Catalog`.
  - `archives.rs`: exporting `m-1` labelled "Work notes" sends `attachment; filename="work-notes-m-1.tar.gz"`, and a label with no ASCII letters sends `filename="m-1.tar.gz"`.
- [ ] **Step 2: Implement**
  - Rename `starter_files`' first parameter to `heading` (doc: "the root index's heading: the catalog's display name, or [`ROOT_HEADING`]").
  - `write_starter_files` passes `if catalog.display_name.is_empty() { ROOT_HEADING } else { &catalog.display_name }`. Import `ROOT_HEADING` with `pub(crate) use` from `replay.rs` if it is private.
  - In `archives.rs:404`:

    ```rust
    let label = app.keys().labels().catalog(&name).map(export_stem).unwrap_or_default();
    // The stem is ASCII letters, digits and '-', and `name` is `[a-z0-9-]+`:
    // both are safe in a header.
    let file = if label.is_empty() { name.clone() } else { format!("{label}-{name}") };
    let disposition = format!("attachment; filename=\"{file}.tar.gz\"");
    ```

  - Add to the spec, under Part 3 "Catalog content and exports", a `> **Corrected (issue #N).**` note: the heading is the display name, else "Catalog", not the description fallback, because the description is already the paragraph beneath it.
- [ ] **Step 3: Gates, changelog `m17-task-09-heading-export`, commit** `feat: a new catalog's heading and its export filename use its display name`.

---

### Task 10a: Console core: labels, header, Account

**Files:**
- Create: `src/http/ui/js/core/labels.js`
- Modify: `src/http/ui/js/core/shell.js`, `src/http/ui/js/core/store.js` (a `labels` slot), `src/http/ui/js/pages/account.js`, `src/http/ui/styles.css` (the `.id-line` style)
- Test: `tests/ui_e2e/account.rs`, `tests/ui_e2e/login.rs` (header)

- [ ] **Step 1: Failing browser tests**
  - The header `#whoami` reads `Alice Example - write` for a session user with that display name, and `#catalog-select`'s option text is the catalog's label while its `value` is the slug.
  - Account page:
    - `#account-display-name` is an input holding the current display name;
    - typing "Alice B" and pressing `#account-display-name-save` shows a toast, and after a reload the header says `Alice B - ...`;
    - an 81-character name shows the server's refusal and keeps the old value;
    - the slug is shown in `#account-id` as `Account ID: alice`.
  - **A non-admin team owner renames their team from Account.** Seed bob, a non-admin user who owns the shared team `acme` (display name "Acme") and is a member of no other shared team.
    - The Account page's "Teams you own" section lists `acme` with an input `#account-team-acme` holding "Acme".
    - Saving "Acme Ltd" through `#account-team-acme-save` shows a toast; after a reload the input holds "Acme Ltd".
    - The personal team is listed as "Named after you", with no input.
    - A member who is not an owner sees the team in Teams but no editor.

  Follow `tests/ui_e2e/harness.rs` (`drive`, `Ui`) and the existing `account.rs` journeys.
- [ ] **Step 2: `core/labels.js`**

```js
// Labels for slugs (spec 2026-11-17-okf-display-names-design.md). The
// server computes every label (GET /api/v1/labels); this module only looks
// them up. A slug it does not know (created since the last refresh) is
// shown as itself, in the id style, rather than hidden.
import { h } from './dom.js';
import { store } from './store.js';

const EMPTY = { users: {}, teams: {}, catalogs: {} };

export function setLabels(labels) { store.labels = labels ?? EMPTY; }

function lookup(kind, slug) { return (store.labels ?? EMPTY)[kind][slug]; }

export const userLabel = slug => lookup('users', slug) ?? slug;
export const teamLabel = slug => lookup('teams', slug) ?? slug;
export const catalogLabel = slug => lookup('catalogs', slug) ?? slug;

// A label with its id beneath, muted: for table cells and drawer titles.
// `label` may already be known (a view's own `label` field).
export function named(label, slug) {
    return h('span', { class: 'named' },
        h('span', { class: 'named-label' }, label),
        h('span', { class: 'id-line muted' }, slug));
}
```

- [ ] **Step 3: `refreshIdentity`.** Add `api.get('/labels')` to the `Promise.all`, then `setLabels(..)` before anything renders.
  - The `#whoami` text uses `me.user_label ?? me.user` for a session. For a key it uses `(me.user ? `${me.user_label ?? me.user} / ` : '')` followed by the key name.
  - The catalog `<option>` text becomes `catalogLabel(name)`, with `value: name` kept.
  - Pages that call `refreshIdentity()` after a write get fresh labels for free.
- [ ] **Step 4: Account page** (`account.js:44-48`):
  - The Name row becomes "Display name", with a text input `#account-display-name` (`maxlength="80"`) and a `#account-display-name-save` button. Saving calls `api.patch('/me', { display_name })`, toasts "Display name saved", then `refreshIdentity()`.
  - Add a muted row `#account-id`: "Account ID: <code>{account.user}</code>".
  - The Teams list shows `teamLabel(team)`, and the ceiling table's catalog column shows `catalogLabel(catalog)`.
  - **"Teams you own"** (the owner editor for non-admins; the Teams page stays admin-only, since its list and detail routes use `require_reader`). It is built from `account.owned_team_details`.
    - Each shared team gets an input `#account-team-<name>`, prefilled with `display_name`, and a `#account-team-<name>-save` button that calls `api.patch(`/teams/${name}`, { display_name })`, toasts "Team name saved" and runs `refreshIdentity()`.
    - A personal team shows its label and "Named after you", with no input.

    > **Corrected (PR #596 review).** The first draft put the team editor only in the admin Teams drawer, which a non-admin owner cannot open.
  - Use the existing `api.patch` helper if `core/api.js` has one; otherwise add `patch` beside `put` in the same style.
- [ ] **Step 5: CSS.** `.named { display: flex; flex-direction: column; }` and `.id-line { font-size: 0.8em; font-family: var(--pico-font-family-monospace); }`, using the existing tokens. No new colours.
- [ ] **Step 6: `just test-ui` via `gate.sh test-ui`, plus the other gates; changelog `m17-task-10a-console-core`; commit** `feat(console): labels in the header and an editable display name`.

---

### Task 10b: Console admin pages

**Files:** `src/http/ui/js/pages/users.js`, `teams.js`, `catalogs.js`, `src/http/ui/js/components/user-account.js`; tests `tests/ui_e2e/users.rs`, `teams.rs`, `catalogs.rs`

- [ ] **Step 1: Failing browser tests**
  - **Users table:** the first column shows `named(user.label, user.name)`, and the drawer title is the label. The drawer has a display-name field that saves through `PATCH /users/{name}`, and the row then shows the new label.
  - **Teams table:** the first column is `named(team.label, team.name)`, and members show `member.label`. A shared team's drawer has a display-name field (`PATCH /teams/{name}`); a personal team's drawer has none and says "Named after its user".
  - **Catalogs table:**
    - the first column is `named(catalog.label, catalog.name)`, and the Owner column is `catalog.owner_team_label`;
    - the drawer has Display name and Description fields saved together through `PATCH /catalogs/{name}`;
    - a non-admin team owner, signed in with a session, opens their catalog's drawer from the Catalogs link, sees Display name and Description (and no team picker, member access or delete), saves a new display name, and sees the row's label change; a plain member opening the same row sees no editor;
    - the delete dialog still asks for the slug and shows it.
- [ ] **Step 2: Implement.** Make these edits; the line numbers are from `283594c`:
  - **`users.js`:**
    - Name column `:36-41` → `named(user.label, user.name)` (keep the "you" pill after it); drop the separate Display name column, since the label is now the headline;
    - the filter at `:61` also matches `label`;
    - the create form keeps `name` and `display_name`;
    - the drawer title is `user.label` and the subtitle `'User'`; add the display-name edit field in the drawer's form section.
  - **`user-account.js` toasts** `:81-165`: name the user by `userLabel(name)`.
  - **`teams.js`:**
    - columns `:35-42` → `named(team.label, team.name)`;
    - the drawer subtitle `:223` → `team.personal_of ? 'Personal team' : 'Team'`, title `team.label`;
    - members `:164-202` → `named(member.label, member.user)`;
    - add the display-name field, hidden when `team.personal_of`.
  - **`catalogs.js`:**
    - Name `:22` → `named(c.label, c.name)`; Owner `:32-34` → `c.owner_team_label ?? c.owner_team`;
    - the drawer `:84-99` "Save description" becomes "Save", with both inputs sending only the fields that changed;
    - **Share one details editor between the admin and owner drawers.** Today a non-admin row opens `openMaintenance` (`catalogs.js:41`, `:150-158`), which has no form. `openEdit` can't be reused: it starts with `namesOf('teams')` (`:87-89`), and `GET /teams` is admin-only (`require_reader`), so it returns early for an owner.
      - Extract `detailsEditor(row, ticket)` from `openEdit` (`:91-99`). It builds the Display name and Description form, saves through `PATCH /catalogs/{name}` sending only the changed fields, toasts, and calls `load()` and `refreshIdentity()`. It uses no team list.
      - `openEdit` (admins) uses it in place of its inline form and keeps the owner picker, members, maintenance and delete sections.
      - `openMaintenance` becomes `openOwner`. It runs `maySwitchMaintenance(row)` (the existing owner check), then opens the drawer with `detailsEditor(row, ticket)` **only when allowed**, followed by the maintenance switch.
      - The owner path has no team picker, member access or delete. The table's `onRow` stays `admin ? openEdit : openOwner`.

      > **Corrected (PR #596 review).** The first draft only changed the form's visibility, which an owner never reaches.
    - toasts `:99`, `:112`, `:247`, `:260` → the label.
- [ ] **Step 3: Gates (with `test-ui`), changelog `m17-task-10b-console-admin`, commit** `feat(console): label-first users, teams and catalogs, with edits`.

---

### Task 10c: Console other pages

**Files:** `src/http/ui/js/pages/browse.js`, `health.js`, `runs.js`, `changesets.js`, `transfer.js`, `unlocks.js`, `keys.js`, `my-keys.js`, `my-grants.js`; tests `tests/ui_e2e/browse.rs`, `changesets.rs`, `keys.rs`

- [ ] **Step 1: Failing browser tests**
  - The browse suspended view reads `"Work notes" is unavailable / Its team "Acme" is suspended`.
  - The Changesets page's catalog column is the label, and its actor column is `actor_label ?? actor` (the REST changeset view must carry `actor_label`; if `GET /api/v1/catalogs/{c}/changesets` builds its own view rather than reusing `ListedChangeset`, add `actor_label` there too, with a test).
  - The keys page's user column and catalog grants show labels.
- [ ] **Step 2: Implement.** Make these edits; the line numbers are from `283594c`:
  - `browse.js:85-88` → `catalogLabel`, `teamLabel`;
  - `health.js:122` → "Catalog {catalogLabel(name)}";
  - `runs.js:69`, `:86` → `catalogLabel`; the actor through `userLabel` only when the run view gives `actor_label`, else as is;
  - `changesets.js:16-18`, `:58`, `:81` → as tested;
  - `transfer.js:61-74` → option text `catalogLabel(name)`, value the slug;
  - `unlocks.js:71`, `:100` → `userLabel`, `catalogLabel`;
  - `keys.js:50-63`, `:165-171`, `:276`, `:323-330`, `my-keys.js:151-172`, and `my-grants.js` → `userLabel` for users and `catalogLabel` for the default catalog and grants.

  URLs (`#/browse/<slug>/…`) are not touched.
- [ ] **Step 3: Gates (with `test-ui`), changelog `m17-task-10c-console-pages`, commit** `feat(console): labels across the remaining pages`.

---

### Task 11: Audit shows labels

**Files:** `src/store/audit.rs` (listing and export), `src/http/api/audit/*.rs` (view), `src/http/ui/js/pages/audit.js`; tests `src/http/api/audit/tests.rs`, `tests/ui_e2e/audit.rs`

- [ ] **Step 1: Failing tests**
  - Listing: an entry about a user whose `audit_people.display_name` is "Alice Example" has `person_label: "Alice Example"`. After the user is deleted (`display_name` blanked, `audit.rs:888-889`), it is `UNNAMED_USER`, never the slug.
  - Catalog targets get `target_label` from `Labels` while the catalog exists, else `null`. Shared-team targets get it from `Labels`, else `null`.
  - UI: the person column shows the label.
- [ ] **Step 2: Implement.**
  - The listing SELECT also reads `ap.display_name`. Email is not snapshotted, so the person label is `user_label(&display_name, None)` (an email fallback is unavailable in audit, by design).
  - The view gains `person_label: Option<String>` and `target_label: Option<String>`.

    > **Corrected (issue #610).** The fields are on the nested objects: `actor.person_label` (`ActorView::Known`) and `target.person_label` and `target.target_label` (`TargetView`), next to the `person` and `name` they label, so every existing field keeps its shape. `audit::list` and `audit::export_lines` take a `&Labels` (the routes pass `app.keys().labels()`; `okf-mcp audit` calls `Labels::load`). `target_label` is set for `catalog` targets and shared `team` targets only (`null` for a personal team). The export gains the fields inside its JSON objects rather than a trailing column, since it is JSON Lines.
  - In `audit.js:34-58`, show `person_label ?? person ?? label` and `target_label ?? target_name`.
  - The filters (`:119-124`) keep matching slugs.
  - The export format keeps its columns and appends `person_label` last. If the export has a fixed schema test, update it.
- [ ] **Step 3: Gates, changelog `m17-task-11-audit-labels`, commit** `feat(audit): show people and targets by label`.

---

### Task 12: Docs, skills, close-out

**Files:** `README.md`, `docs/operations.md`, `skills/*/SKILL.md`, `skills/README.md`, the spec, `changelog/*-m17-milestone.md`

- [ ] **Step 1: `docs/operations.md`.**
  - In the users/teams/`/me` field reference (`:3120-3159`), add `label` and `display_name` everywhere they now appear, the three PATCH routes, `GET /labels`, and the provisioner allow-list additions.
  - Add a short "Display names and labels" section: the rule, who edits what, and "slugs never change".
  - Where client config examples show `X-OKF-Catalogs` (`:1358`), say the value is the catalog's id from `list_catalogs`.
- [ ] **Step 2: README.** Do the same where the README shows client config (`:259-274`, `:477`) and the CLI command list (`user update`, `team update`, `catalog update --display-name`).
- [ ] **Step 3: Skills.** In each `skills/*/SKILL.md` that tells the agent to pass `catalog`, add the line "Say the catalog's `label` to people; pass its id (`name`)". `okf-review-oldest`'s `human:<owner>` stays the owner's id (spec Part 4).
- [ ] **Step 4: Spec status.** Set the status line to "delivered in milestone 17", and make sure every correction note from Tasks 3, 5 and 9 is present.
- [ ] **Step 5: The milestone changelog,** listing what shipped and the follow-ups.
- [ ] **Step 6: The coordinator files the provisioner follow-up issue** in the provisioner's own repository, not this one, with the contents of spec Part 5 "Rollout" step 2. It depends on the release carrying this milestone. Link it from the epic.
- [ ] **Step 7: Gates, commit** `docs: display names and labels`.

---

## Self-review

**Spec coverage**

| Spec item | Task |
|---|---|
| Permanent slugs (Decision 1) | no task renames; convention |
| Label fallbacks (Decision 2) | 1 |
| Who edits (Decision 3) | 3 (store checks), 5 (routes) |
| Stored ids, display-time labels (Decision 4) | 4, 7, 11 |
| MCP inputs and outputs (Decision 5) | 7 |
| Scope: this repo (Decision 6) | 12 files the follow-up |
| Schema and `DisplayName` | 1, 2 |
| `label` on views | 5 |
| Disambiguation | 4 (`catalog_choices`), 8 (consent), 10a (header) |
| Edit routes and owner check | 3, 5 |
| Provisioner routes | 5 |
| CLI | 6 |
| Propagation | 3 (bumps), 4 (reload with the key cache) |
| Console surfaces | 10a-10c, 11 |
| Emails | 8 |
| Sign-in, consent, passkeys | 8 |
| Heading and export | 9 |
| Deliberately still slugs | no change made |
| MCP outputs, inputs, instructions, errors | 7 |
| Attribution | 7 (`actor_label`), 11 |
| Skills and docs | 12 |
| Migration | 2 |
| Rollout | 12 |
| Testing | in every task; property tests in 1 |

**Types used across tasks**
- `DisplayName` (1) → 2, 3, 5, 6.
- `user_label`, `team_label`, `catalog_label`, `export_stem` (1) → 4, 5, 8, 9, 11.
- `Labels`, with `user`, `team`, `catalog`, `actor`, `catalog_owner`, `teams_of` and `catalog_choices` (4) → 5, 7, 8, 9, 10a.
- `DetailsChange`, `Details` and `update_details` (3) → 5, 6.
- `TeamDisplayNameChange` (3) → 5, 6.
- `SuspendedCatalog.catalog_label` and `.team_label` (4) → 7.

**Spec corrections this plan makes:** Decision 3 (the heading, Task 9), Decision 4 (audit details record the length, Task 3), and the audit-page wording for deleted users (the spec said they keep their last label; deletion blanks the snapshot, so they show the placeholder).

## Open questions for the maintainer

None.
