//! The bookkeeping ops `log` and `index` (`okf.py:752-820`) and
//! `ensure_index` (`:1471`). Each writes through the workspace and records
//! the [`ReplayOp`]s that redo its edit at publish (see [`mod@super::replay`]).
//!
//! Also two server-only pieces `okf.py` has no command for (issue #426): the
//! [`starter_files`] a new catalog is created with, and [`repair_indexes`],
//! which creates the root `index.md` and `log.md`, folder indexes and listings
//! a catalog is missing, so a catalog created empty can be repaired through a
//! changeset; [`repair_catalog`] is the operator's direct-write variant.
//!
//! Divergences from `okf.py`:
//! - a missing `log.md` is [`OkfError::NotFound`] (okf.py: `no log.md at <root>`);
//! - `ensure_index` upper-cases a folder name's first character where
//!   Python's `capitalize` title-cases it; they differ only for the few
//!   digraph letters such as `ǆ`;
//! - `ensure_index` needs no existing directory (`okf.py` expects `mv` to
//!   have created it): a workspace creates parents on write;
//! - `index` refuses a list or mapping `title` with "has no title", where
//!   `okf.py` crashes (`AttributeError` on `.replace`). Scalar titles such as
//!   `2026` or `true` are strings in both parsers, so they list alike.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};

use crate::error::OkfError;
use crate::okf::doc::Doc;
use crate::okf::ops::replay::{
    folder_heading, insert_index_bullet_text, lists_target, log_text, missing_heading,
    new_index_text, root_index_text, ReplayOp, LOG_TEMPLATE,
};
use crate::okf::ops::{load_concept, refused, LogKind, OpContext, OpOutcome};
use crate::okf::paths::{basename, dirname, is_concept, join};
use crate::okf::pystr::is_py_space;
use crate::okf::rules::is_machinery;
use crate::okf::workspace::Workspace;
use crate::okf::yaml::Value;

/// The catalog's log, at the root.
const LOG: &str = "log.md";
/// The root index.
const ROOT_INDEX: &str = "index.md";
/// A folder's index file name.
const INDEX: &str = "index.md";
/// The summary of a folder listing nobody has described yet.
const FOLDER_TODO: &str = "TODO: describe this folder";
/// The summary of a concept listing whose concept has no `description`.
const CONCEPT_TODO: &str = "TODO: describe this concept";
/// The heading folder listings go under.
const SECTIONS: &str = "Sections";

/// Python's `" ".join(text.split())`: runs of whitespace become one space,
/// and leading and trailing whitespace goes. Shared with `move_concept` and
/// `split_concept`, whose listings collapse the description the same way.
#[tracing::instrument(level = "debug", skip_all)]
pub(crate) fn collapse_whitespace(text: &str) -> String {
    text.split(is_py_space)
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Port of `cmd_log`: adds `* **<kind>:** <text>` under today's (UTC)
/// heading in the root `log.md`.
///
/// # Arguments
/// * `workspace` - the catalog.
/// * `ctx` - its `now` gives the date.
/// * `kind` - the entry's kind.
/// * `text` - the entry; whitespace runs collapse to single spaces.
///
/// # Returns
/// The outcome, with a `Log` replay op for `log.md`; nothing is touched
/// when the bullet is already under today's heading.
///
/// # Errors
/// [`OkfError::NotFound`] without a `log.md`; the workspace's write error.
#[tracing::instrument(level = "debug", skip_all)]
pub fn log(
    workspace: &dyn Workspace,
    ctx: &OpContext<'_>,
    kind: LogKind,
    text: &str,
) -> Result<OpOutcome, OkfError> {
    // Text mode with universal newlines, as `okf.py` reads `log.md`.
    let current = workspace.read_text(LOG)?;
    let date = ctx.now.format("%Y-%m-%d").to_string();
    let bullet = format!("* **{}:** {}", kind.as_str(), collapse_whitespace(text));
    let Some(updated) = log_text(&current, &date, &bullet) else {
        return Ok(OpOutcome::unchanged(format!(
            "{LOG}: already logged under {date}"
        )));
    };
    workspace.write_atomic(LOG, &updated)?;
    // A struct literal with `..default()` (not assignments after
    // `default()`, which clippy's `field_reassign_with_default` flags).
    let mut outcome = OpOutcome {
        message: format!("{LOG}: ## {date}: {bullet}"),
        ..OpOutcome::default()
    };
    outcome.record(LOG, ReplayOp::Log { date, bullet });
    Ok(outcome)
}

/// What [`list_concept`] lists: `index`'s arguments once resolved.
pub(crate) struct Listing<'a> {
    /// Catalog-relative path of the concept.
    pub(crate) concept: &'a str,
    /// The link text, before escaping.
    pub(crate) title: &'a str,
    /// The summary after ` - `; whitespace collapses.
    pub(crate) summary: &'a str,
    /// The heading to list under, if any.
    pub(crate) section: Option<&'a str>,
}

/// `cmd_index` after the concept is loaded: lists it in its folder's
/// `index.md` unless the index already links it, recording the replay op.
///
/// # Returns
/// `okf.py`'s message: `<folder>/index.md: <bullet>`, or
/// `<folder>/index.md: already lists <concept>`.
///
/// # Errors
/// [`OkfError::Refused`] without a folder `index.md`, or when `section`
/// names a missing heading; the workspace's read or write error.
#[tracing::instrument(level = "debug", skip_all)]
pub(crate) fn list_concept(
    workspace: &dyn Workspace,
    listing: &Listing<'_>,
    outcome: &mut OpOutcome,
) -> Result<String, OkfError> {
    let folder = dirname(listing.concept);
    let index_path = join(folder, "index.md");
    if !workspace.exists(&index_path) {
        return Err(refused(format!(
            "no index.md in /{folder}; create it first"
        )));
    }
    let text = workspace.read_text(&index_path)?;
    if lists_target(workspace, &text, &index_path, listing.concept) {
        return Ok(format!("{index_path}: already lists {}", listing.concept));
    }
    let bullet = concept_bullet(listing);
    let updated = insert_index_bullet_text(&text, &bullet, listing.section, false)
        .ok_or_else(|| missing_heading(&index_path, listing.section.unwrap_or("")))?;
    workspace.write_atomic(&index_path, &updated)?;
    let message = format!("{index_path}: {bullet}");
    outcome.record(
        &index_path,
        ReplayOp::ListBullet {
            target: listing.concept.to_string(),
            bullet,
            section: listing.section.map(str::to_string),
            create_section: false,
        },
    );
    Ok(message)
}

/// The bullet [`list_concept`] writes: `* [<title>](<file name>) - <summary>`,
/// with `[` and `]` in the title escaped and the summary's whitespace
/// collapsed.
#[tracing::instrument(level = "trace", skip_all)]
fn concept_bullet(listing: &Listing<'_>) -> String {
    let label = listing.title.replace('[', "\\[").replace(']', "\\]");
    format!(
        "* [{label}]({}) - {}",
        basename(listing.concept),
        collapse_whitespace(listing.summary)
    )
}

/// Port of `cmd_index`: lists a concept in its folder's `index.md`.
///
/// # Arguments
/// * `workspace` - the catalog; `_ctx` is unused (no timestamp).
/// * `concept` - the concept reference.
/// * `summary` - the text after ` - `.
/// * `section` - the heading to list under; `None` for `# Concepts`.
/// * `title` - the link text; `None` (or empty) for the concept's `title`.
///
/// # Returns
/// The outcome, with a `ListBullet` replay op; nothing is touched when the
/// index already links the concept.
///
/// # Errors
/// As [`crate::okf::ops::stamp`] when loading the concept;
/// [`OkfError::Refused`] without a title, without a folder `index.md`, or
/// for a missing `section` heading.
#[tracing::instrument(level = "debug", skip_all)]
pub fn index(
    workspace: &dyn Workspace,
    _ctx: &OpContext<'_>,
    concept: &str,
    summary: &str,
    section: Option<&str>,
    title: Option<&str>,
) -> Result<OpOutcome, OkfError> {
    let loaded = load_concept(workspace, concept)?;
    // `a.title or data.get("title")`: an empty string counts as absent. Every
    // YAML scalar is a `Value::Str`, so only a list or mapping is dropped here.
    let frontmatter_title = loaded.data.get("title").and_then(Value::as_str);
    let title = title
        .filter(|t| !t.is_empty())
        .or(frontmatter_title.filter(|t| !t.is_empty()))
        .ok_or_else(|| refused(format!("{} has no title; pass title", loaded.path)))?;
    let listing = Listing {
        concept: &loaded.path,
        title,
        summary,
        section,
    };
    let mut outcome = OpOutcome::default();
    let message = list_concept(workspace, &listing, &mut outcome)?;
    outcome.message = message;
    Ok(outcome)
}

/// Port of `ensure_index`: makes sure `folder/index.md` exists and is
/// listed in its parent's index, creating missing ancestor indexes too.
///
/// As in `okf.py`, the folder's own `index.md` is written first; then the
/// parent is ensured (recursively), and only after that is the folder listed
/// in it. So the notes come out outermost-first.
///
/// A new index is `# <Folder name>` and a TODO line; its parent gains
/// `* [<Folder name>](<name>/) - TODO: describe this folder` under
/// `# Sections` (created when missing), with an absolute `/<folder>/` href
/// when the parent's bullets are absolute.
///
/// # Arguments
/// * `workspace` - the catalog.
/// * `folder` - catalog-relative folder; `""` (the root) does nothing.
/// * `outcome` - gains each file written, its replay ops (`CreateIndex`,
///   then the parent's `ListBullet`) and one note per created index.
///
/// # Errors
/// The workspace's read or write error (a missing root `index.md` is
/// [`OkfError::NotFound`]). Files written before the error stay written,
/// so run it on a scratch copy (as the write path does) and drop the copy
/// on error.
#[tracing::instrument(level = "debug", skip_all)]
pub fn ensure_index(
    workspace: &dyn Workspace,
    folder: &str,
    outcome: &mut OpOutcome,
) -> Result<(), OkfError> {
    if folder.is_empty() {
        return Ok(());
    }
    let index_path = join(folder, "index.md");
    if workspace.exists(&index_path) {
        return Ok(());
    }
    workspace.write_atomic(&index_path, &new_index_text(folder))?;
    outcome.record(
        &index_path,
        ReplayOp::CreateIndex {
            folder: folder.to_string(),
        },
    );
    let parent = dirname(folder);
    ensure_index(workspace, parent, outcome)?;
    let parent_index = join(parent, INDEX);
    let parent_text = workspace.read_text(&parent_index)?;
    insert_folder_listing(workspace, folder, &parent_text, outcome)?;
    outcome.notes.push(format!(
        "created /{folder}/index.md; replace its TODO lines (and the parent's listing)"
    ));
    Ok(())
}

/// `ensure_index`'s listing of `folder` in its parent's index:
/// `* [<Folder name>](<name>/) - TODO: describe this folder` under
/// `# Sections` (created when missing), with an absolute `/<folder>/` href
/// when the parent's bullets are absolute. It does not check whether the
/// parent lists the folder already ([`repair_indexes`] does, first).
///
/// # Arguments
/// * `workspace` - the catalog.
/// * `folder` - catalog-relative folder, not the root.
/// * `parent_text` - the parent's `index.md`, as read.
/// * `outcome` - gains the parent's `ListBullet` replay op.
///
/// # Errors
/// The workspace's write error.
#[tracing::instrument(level = "trace", skip_all)]
fn insert_folder_listing(
    workspace: &dyn Workspace,
    folder: &str,
    parent_text: &str,
    outcome: &mut OpOutcome,
) -> Result<(), OkfError> {
    let parent_index = join(dirname(folder), INDEX);
    let bullet = folder_bullet(folder, parent_text);
    let updated = insert_index_bullet_text(parent_text, &bullet, Some(SECTIONS), true)
        .expect("create_section always finds a place for the bullet");
    workspace.write_atomic(&parent_index, &updated)?;
    outcome.record(
        &parent_index,
        ReplayOp::ListBullet {
            target: join(folder, INDEX),
            bullet,
            section: Some(SECTIONS.to_string()),
            create_section: true,
        },
    );
    Ok(())
}

/// The bullet [`insert_folder_listing`] writes for `folder` in its parent's
/// index: `* [<Folder name>](<href>) - TODO: describe this folder`, with
/// the href `/<folder>/` when the parent's bullets are absolute, else
/// `<name>/`.
#[tracing::instrument(level = "trace", skip_all)]
fn folder_bullet(folder: &str, parent_text: &str) -> String {
    let absolute = parent_text
        .split('\n')
        .any(|line| line.starts_with("* [") && line.contains("](/"));
    let href = if absolute {
        format!("/{folder}/")
    } else {
        format!("{}/", basename(folder))
    };
    format!("* [{}]({href}) - {FOLDER_TODO}", folder_heading(folder))
}

/// Does `bullet`, written in `index`, link `target`? Not for every path or
/// label: a link's destination stops at whitespace or `)`, `#` starts its
/// fragment, and its label ends at the first unescaped `]` (see
/// [`crate::okf::markdown`]), so `a note.md`, a folder `a]` or a title
/// ending in `\` cannot be listed this way. The repair checks the exact
/// bullet it would write and skips one that no later run would recognise.
#[cfg_attr(
    dylint_lib = "hyp_tracing",
    allow(
        uninstrumented_fn,
        reason = "one-line delegation to an instrumented function"
    )
)]
fn links_back(workspace: &dyn Workspace, index: &str, bullet: &str, target: &str) -> bool {
    lists_target(workspace, bullet, index, target)
}

/// The note for a listing [`repair_indexes`] cannot write.
#[tracing::instrument(level = "trace", skip_all)]
fn unlinkable_note(target: &str, index: &str) -> String {
    format!("did not list /{target} in /{index}: no markdown link to it can be written")
}

/// How many [`starter_files`] a new catalog holds.
pub const STARTER_FILE_COUNT: usize = 2;

/// The files a new catalog starts with: a root `index.md` and a `log.md`
/// with one `Creation` entry, so the bookkeeping tools (`log`, `index`,
/// `create_concept` with `index_summary`) work in it at once. They follow
/// `okf-catalog-template`'s skeleton, without its people and projects.
///
/// # Arguments
/// * `name` - the catalog's name, the index's heading.
/// * `description` - the catalog's description, the index's paragraph (a
///   TODO line when blank).
/// * `now` - the creation time; its UTC date heads the log entry.
///
/// # Returns
/// `(path, text)` pairs, sorted by path.
///
/// # Examples
/// ```
/// use chrono::{TimeZone, Utc};
/// use okf_mcp::okf::ops::starter_files;
/// let now = Utc.with_ymd_and_hms(2026, 11, 14, 12, 0, 0).unwrap();
/// let files = starter_files("pg", "Systems.", now);
/// assert_eq!(files[0].0, "index.md");
/// assert!(files[0].1.contains("okf_version") && files[0].1.contains("# pg\n\nSystems.\n"));
/// assert!(files[1].1.ends_with("## 2026-11-14\n\n* **Creation:** Created the catalog.\n"));
/// ```
#[tracing::instrument(level = "debug", skip_all)]
pub fn starter_files(
    name: &str,
    description: &str,
    now: DateTime<Utc>,
) -> [(&'static str, String); STARTER_FILE_COUNT] {
    let date = now.format("%Y-%m-%d").to_string();
    let bullet = format!("* **{}:** Created the catalog.", LogKind::Creation.as_str());
    let log = log_text(LOG_TEMPLATE, &date, &bullet)
        .expect("an empty log has no entry under any date yet");
    [(ROOT_INDEX, root_index_text(name, description)), (LOG, log)]
}

/// Creates whatever bookkeeping a catalog is missing, never changing what
/// it has: the root `index.md` and `log.md`, an `index.md` for every folder
/// holding a concept or an index (and their ancestors), a listing of every
/// folder index in its parent's `# Sections`, and a listing of every concept
/// in its folder's index (where `index` puts it). Machinery is skipped,
/// since no write tool may change it.
///
/// Existing text is only ever added to: headings, prose, bullets and
/// summaries stay as they are, nothing is removed, and a target an index
/// already links (however it is worded) is not listed again. So running it
/// twice changes nothing the second time. A file or folder no markdown link
/// can list (a space, `)` or `#` in its path, or a label that cannot be
/// written, such as a folder named `a]`) is not listed; a note names it
/// instead. Every edit is a replay op
/// ([`ReplayOp::CreateIndex`], [`ReplayOp::CreateLog`],
/// [`ReplayOp::ListBullet`]), so publish redoes it onto indexes that changed
/// meanwhile instead of overwriting them.
///
/// A listing takes the concept's frontmatter `title` (else its first `# `
/// heading, else its file name) and `description`; what is missing becomes
/// a TODO placeholder, and each one gets a note.
///
/// # Arguments
/// * `workspace` - the catalog.
///
/// # Returns
/// The outcome: every file written, its replay ops, a summary message, and
/// one note per created file and per placeholder.
///
/// # Errors
/// The workspace's listing, read or write error. Files written before the
/// error stay written, so run it on a scratch copy (as the write path does).
#[tracing::instrument(level = "debug", skip_all)]
pub fn repair_indexes(workspace: &dyn Workspace) -> Result<OpOutcome, OkfError> {
    let (mut outcome, placeholders) = repair_counted(workspace)?;
    outcome.message = repair_message(outcome.touched.len(), placeholders);
    Ok(outcome)
}

/// [`repair_indexes`]' edits, and how many TODO placeholders they wrote;
/// the outcome's message is left empty for the caller.
#[tracing::instrument(level = "trace", skip_all)]
fn repair_counted(workspace: &dyn Workspace) -> Result<(OpOutcome, usize), OkfError> {
    let markdown = workspace.list_markdown()?;
    let mut outcome = OpOutcome::default();
    let mut placeholders = 0_usize;
    if !workspace.exists(ROOT_INDEX) {
        workspace.write_atomic(ROOT_INDEX, &new_index_text(""))?;
        outcome.record(
            ROOT_INDEX,
            ReplayOp::CreateIndex {
                folder: String::new(),
            },
        );
        outcome
            .notes
            .push("created /index.md; replace its heading and TODO line".to_string());
        placeholders += 1;
    }
    if !workspace.exists(LOG) {
        workspace.write_atomic(LOG, LOG_TEMPLATE)?;
        outcome.record(LOG, ReplayOp::CreateLog);
        outcome.notes.push("created /log.md".to_string());
    }
    let concepts: Vec<&str> = markdown
        .iter()
        .map(String::as_str)
        .filter(|path| is_concept(path) && !is_machinery(path))
        .collect();
    // Every folder below the root that holds a concept or an index, and
    // every ancestor of one (an existing nested index may have lost its
    // parent's), sorted: a `BTreeSet` is a sorted set, and a folder sorts
    // before anything inside it, so a parent's index exists before its
    // children are listed in it.
    let mut folders: BTreeSet<&str> = BTreeSet::new();
    for path in markdown.iter().map(String::as_str) {
        if is_machinery(path) || (basename(path) != INDEX && !is_concept(path)) {
            continue;
        }
        let mut folder = dirname(path);
        // `insert` returns false once a folder (and so all its ancestors)
        // is already in the set.
        while !folder.is_empty() && folders.insert(folder) {
            folder = dirname(folder);
        }
    }
    for folder in folders {
        let index_path = join(folder, INDEX);
        if !workspace.exists(&index_path) {
            // Not `ensure_index`: that lists the new index in its parent
            // unconditionally, and the parent may link the folder already.
            workspace.write_atomic(&index_path, &new_index_text(folder))?;
            outcome.record(
                &index_path,
                ReplayOp::CreateIndex {
                    folder: folder.to_string(),
                },
            );
            outcome
                .notes
                .push(format!("created /{index_path}; replace its TODO line"));
            placeholders += 1;
        }
        let parent_index = join(dirname(folder), INDEX);
        let parent_text = workspace.read_text(&parent_index)?;
        if !lists_target(workspace, &parent_text, &parent_index, &index_path) {
            let bullet = folder_bullet(folder, &parent_text);
            if !links_back(workspace, &parent_index, &bullet, &index_path) {
                outcome
                    .notes
                    .push(unlinkable_note(&format!("{folder}/"), &parent_index));
                continue;
            }
            insert_folder_listing(workspace, folder, &parent_text, &mut outcome)?;
            outcome.notes.push(format!(
                "listed /{folder}/ in /{parent_index} with a TODO summary"
            ));
            placeholders += 1;
        }
    }
    for concept in concepts {
        let index_path = join(dirname(concept), INDEX);
        let text = workspace.read_text(&index_path)?;
        if lists_target(workspace, &text, &index_path, concept) {
            continue;
        }
        let about = ConceptAbout::read(workspace, concept)?;
        let title = about
            .title
            .clone()
            .unwrap_or_else(|| folder_heading(concept.strip_suffix(".md").unwrap_or(concept)));
        let listing = Listing {
            concept,
            title: &title,
            summary: about.description.as_deref().unwrap_or(CONCEPT_TODO),
            section: None,
        };
        if !links_back(workspace, &index_path, &concept_bullet(&listing), concept) {
            outcome.notes.push(unlinkable_note(concept, &index_path));
            continue;
        }
        if about.title.is_none() {
            outcome.notes.push(format!(
                "listed /{concept} under its file name: it has no title"
            ));
            placeholders += 1;
        }
        if about.description.is_none() {
            outcome.notes.push(format!(
                "listed /{concept} with a TODO summary: it has no description"
            ));
            placeholders += 1;
        }
        list_concept(workspace, &listing, &mut outcome)?;
    }
    Ok((outcome, placeholders))
}

/// A repair's summary line.
#[tracing::instrument(level = "trace", skip_all)]
fn repair_message(written: usize, placeholders: usize) -> String {
    if written == 0 {
        "indexes complete: nothing to repair".to_string()
    } else {
        format!(
            "repaired {written} file(s); {placeholders} TODO placeholder(s) to fill in (see notes)"
        )
    }
}

/// The operator's repair (`okf-mcp catalog repair-indexes`): as
/// [`repair_indexes`], except that a missing root `index.md` is the one a
/// new catalog gets (headed with the catalog's name, its description as the
/// first paragraph, see [`starter_files`]), and a repair that wrote anything
/// adds an `Update` entry to `log.md` saying so, since no agent is there to
/// log it.
///
/// The outcome is for a direct write, not a changeset: it records no replay
/// ops, because the edit is published at once under the catalog's lock.
///
/// # Arguments
/// * `workspace` - the catalog's live documents.
/// * `ctx` - its `now` dates the log entry.
/// * `name` - the catalog's name.
/// * `description` - the catalog's description.
///
/// # Returns
/// The files written (the root first), a summary message and one note per
/// created file and per TODO placeholder; nothing written when nothing is
/// missing.
///
/// # Errors
/// As [`repair_indexes`].
#[tracing::instrument(level = "debug", skip_all)]
pub fn repair_catalog(
    workspace: &dyn Workspace,
    ctx: &OpContext<'_>,
    name: &str,
    description: &str,
) -> Result<OpOutcome, OkfError> {
    let mut outcome = OpOutcome::default();
    let mut placeholders = 0_usize;
    if !workspace.exists(ROOT_INDEX) {
        workspace.write_atomic(ROOT_INDEX, &root_index_text(name, description))?;
        outcome.touch(ROOT_INDEX);
        if collapse_whitespace(description).is_empty() {
            outcome.notes.push(
                "created /index.md; the catalog has no description, so replace its TODO line"
                    .to_string(),
            );
            placeholders += 1;
        } else {
            outcome
                .notes
                .push("created /index.md from the catalog's name and description".to_string());
        }
    }
    let (repaired, more) = repair_counted(workspace)?;
    placeholders += more;
    for path in &repaired.touched {
        outcome.touch(path);
    }
    outcome.notes.extend(repaired.notes);
    if !outcome.touched.is_empty() {
        let text = format!(
            "Repaired the catalog's indexes (okf-mcp catalog repair-indexes): wrote {}.",
            outcome.touched.join(", ")
        );
        log(workspace, ctx, LogKind::Update, &text)?;
        outcome.touch(LOG);
    }
    outcome.message = repair_message(outcome.touched.len(), placeholders);
    Ok(outcome)
}

/// What a listing says about a concept, from its text.
struct ConceptAbout {
    /// Frontmatter `title`, else the first `# ` heading; whitespace collapsed.
    title: Option<String>,
    /// Frontmatter `description`; whitespace collapsed.
    description: Option<String>,
}

impl ConceptAbout {
    /// Reads a concept's title and description. Frontmatter that does not
    /// parse counts as none: the listing gets placeholders instead of
    /// failing the whole repair.
    ///
    /// # Arguments
    /// * `workspace` - the catalog.
    /// * `concept` - its catalog-relative path.
    ///
    /// # Errors
    /// The workspace's read error.
    #[tracing::instrument(level = "trace", skip_all)]
    fn read(workspace: &dyn Workspace, concept: &str) -> Result<Self, OkfError> {
        let text = workspace.read_text(concept)?;
        let doc = Doc::from_text(concept, &text);
        // `.ok()` turns a parse error into `None`, like `except: data = None`.
        let data = if doc.has_frontmatter() {
            doc.parse().ok()
        } else {
            None
        };
        // Blank values count as missing, as `index`'s `title or ...` does.
        let field = |key: &str| {
            data.as_ref()
                .and_then(|data| data.get(key))
                .and_then(Value::as_str)
                .map(collapse_whitespace)
                .filter(|value| !value.is_empty())
        };
        let heading = || {
            doc.body()
                .split('\n')
                .find_map(|line| line.strip_prefix("# "))
                .map(collapse_whitespace)
                .filter(|heading| !heading.is_empty())
        };
        Ok(Self {
            title: field("title").or_else(heading),
            description: field("description"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::okf::workspace::MemWorkspace;
    use chrono::{TimeZone, Utc};

    const INFRA_INDEX: &str =
        "# Infra\n\n# Concepts\n\n* [Time server](time-server.md) - GPS clock\n";

    fn ctx() -> OpContext<'static> {
        OpContext {
            now: Utc.with_ymd_and_hms(2026, 10, 27, 12, 0, 0).unwrap(),
            by: "tester",
        }
    }

    fn refusal(error: OkfError) -> String {
        match error {
            OkfError::Refused { reason } => reason,
            other => panic!("expected Refused, got {other:?}"),
        }
    }

    fn infra() -> MemWorkspace {
        MemWorkspace::from_files([
            ("infra/index.md", INFRA_INDEX),
            (
                "infra/ntp.md",
                "---\ntype: Service\ntitle: NTP [pool]\n---\n",
            ),
            ("infra/untitled.md", "---\ntype: Note\n---\n"),
            ("people/x.md", "---\ntype: Person\ntitle: X\n---\n"),
        ])
    }

    #[test]
    fn log_writes_the_entry_and_records_its_replay() {
        let ws = MemWorkspace::from_files([("log.md", "# Log\n")]);

        let outcome = log(&ws, &ctx(), LogKind::Creation, "  Added   the\n pool. ").unwrap();

        assert_eq!(
            ws.read_to_string("log.md").unwrap(),
            "# Log\n\n## 2026-10-27\n\n* **Creation:** Added the pool.\n"
        );
        assert_eq!(outcome.touched, ["log.md"]);
        assert_eq!(
            outcome.replay["log.md"],
            [ReplayOp::Log {
                date: "2026-10-27".to_string(),
                bullet: "* **Creation:** Added the pool.".to_string(),
            }]
        );
        assert_eq!(
            outcome.message,
            "log.md: ## 2026-10-27: * **Creation:** Added the pool."
        );
    }

    #[test]
    fn log_reports_a_duplicate_and_needs_log_md() {
        let ws = MemWorkspace::from_files([("log.md", "# Log\n")]);
        log(&ws, &ctx(), LogKind::Update, "Once.").unwrap();

        let again = log(&ws, &ctx(), LogKind::Update, "Once.").unwrap();
        let missing = log(&MemWorkspace::default(), &ctx(), LogKind::Update, "x").unwrap_err();

        assert!(again.touched.is_empty());
        assert_eq!(again.message, "log.md: already logged under 2026-10-27");
        assert!(matches!(missing, OkfError::NotFound { .. }));
    }

    #[test]
    fn index_lists_a_concept_with_its_title_escaped() {
        let ws = infra();

        let outcome = index(&ws, &ctx(), "infra/ntp", "  the   pool ", None, None).unwrap();

        let bullet = "* [NTP \\[pool\\]](ntp.md) - the pool";
        assert_eq!(
            ws.read_to_string("infra/index.md").unwrap(),
            format!("{INFRA_INDEX}{bullet}\n")
        );
        assert_eq!(outcome.message, format!("infra/index.md: {bullet}"));
        assert_eq!(
            outcome.replay["infra/index.md"],
            [ReplayOp::ListBullet {
                target: "infra/ntp.md".to_string(),
                bullet: bullet.to_string(),
                section: None,
                create_section: false,
            }]
        );
    }

    #[test]
    fn index_of_a_listed_concept_changes_nothing() {
        let ws = infra();
        index(&ws, &ctx(), "infra/ntp", "pool", None, None).unwrap();

        let again = index(&ws, &ctx(), "infra/ntp.md", "other", None, Some("T")).unwrap();

        assert!(again.touched.is_empty());
        assert_eq!(again.message, "infra/index.md: already lists infra/ntp.md");
    }

    #[test]
    fn index_refusals_use_okf_py_wording() {
        let ws = infra();

        let untitled = index(&ws, &ctx(), "infra/untitled", "s", None, None).unwrap_err();
        let no_index = index(&ws, &ctx(), "people/x", "s", None, None).unwrap_err();
        let no_heading = index(
            &ws,
            &ctx(),
            "infra/untitled",
            "s",
            Some("Nowhere"),
            Some("T"),
        )
        .unwrap_err();

        assert_eq!(
            refusal(untitled),
            "infra/untitled.md has no title; pass title"
        );
        assert_eq!(refusal(no_index), "no index.md in /people; create it first");
        assert_eq!(
            refusal(no_heading),
            "no heading 'Nowhere' in infra/index.md"
        );
        assert_eq!(ws.read_to_string("infra/index.md").unwrap(), INFRA_INDEX);
    }

    #[test]
    fn index_lists_a_scalar_title_and_refuses_a_list_title() {
        let ws = MemWorkspace::from_files([
            ("infra/index.md", INFRA_INDEX),
            ("infra/year.md", "---\ntype: Note\ntitle: 2026\n---\n"),
            ("infra/listed.md", "---\ntype: Note\ntitle: [a, b]\n---\n"),
        ]);

        let year = index(&ws, &ctx(), "infra/year", "y", None, None).unwrap();
        let listed = index(&ws, &ctx(), "infra/listed", "l", None, None).unwrap_err();

        // okf.py writes the same bullet for `title: 2026`, and crashes on the list.
        assert_eq!(year.message, "infra/index.md: * [2026](year.md) - y");
        assert_eq!(refusal(listed), "infra/listed.md has no title; pass title");
    }

    #[test]
    fn ensure_index_follows_the_parents_link_style() {
        let ws = MemWorkspace::from_files([
            ("index.md", "# Root\n\n* [Infra](infra/) - systems\n"),
            (
                "analysis/index.md",
                "# Analysis\n\n# Sections\n\n* [Sub](/analysis/sub/) - a folder\n",
            ),
        ]);
        let mut absolute = OpOutcome::default();
        let mut nested = OpOutcome::default();

        ensure_index(&ws, "analysis/new-stuff", &mut absolute).unwrap();
        ensure_index(&ws, "ops/deep", &mut nested).unwrap();

        assert_eq!(
            absolute.touched,
            ["analysis/new-stuff/index.md", "analysis/index.md"]
        );
        assert_eq!(
            ws.read_to_string("analysis/new-stuff/index.md").unwrap(),
            "# New stuff\n\nTODO: say what this folder holds.\n"
        );
        assert_eq!(
            ws.read_to_string("analysis/index.md").unwrap(),
            concat!(
                "# Analysis\n\n# Sections\n\n* [Sub](/analysis/sub/) - a folder\n",
                "* [New stuff](/analysis/new-stuff/) - TODO: describe this folder\n",
            )
        );
        assert_eq!(
            absolute.replay["analysis/index.md"],
            [ReplayOp::ListBullet {
                target: "analysis/new-stuff/index.md".to_string(),
                bullet: "* [New stuff](/analysis/new-stuff/) - TODO: describe this folder"
                    .to_string(),
                section: Some("Sections".to_string()),
                create_section: true,
            }]
        );
        assert_eq!(
            nested.touched,
            ["ops/deep/index.md", "ops/index.md", "index.md"]
        );
        assert_eq!(
            ws.read_to_string("index.md").unwrap(),
            concat!(
                "# Root\n\n* [Infra](infra/) - systems\n\n# Sections\n\n",
                "* [Ops](ops/) - TODO: describe this folder\n",
            )
        );
        assert_eq!(
            nested.notes,
            [
                "created /ops/index.md; replace its TODO lines (and the parent's listing)",
                "created /ops/deep/index.md; replace its TODO lines (and the parent's listing)",
            ]
        );
    }

    #[test]
    fn ensure_index_of_an_existing_folder_or_the_root_does_nothing() {
        let ws = MemWorkspace::from_files([("index.md", "# Root\n"), ("ops/index.md", "# Ops\n")]);
        let mut outcome = OpOutcome::default();

        ensure_index(&ws, "ops", &mut outcome).unwrap();
        ensure_index(&ws, "", &mut outcome).unwrap();

        assert_eq!(outcome, OpOutcome::default());
    }

    /// A catalog from before starter files: no root index or log, a curated
    /// folder index that misses a concept and a subfolder, and machinery.
    fn legacy() -> MemWorkspace {
        MemWorkspace::from_files([
            ("infra/index.md", LEGACY_INFRA),
            (
                "infra/time-server.md",
                "---\ntype: System\ntitle: Time server\ndescription: GPS clock.\n---\n",
            ),
            (
                "infra/ntp.md",
                "---\ntype: Service\ntitle: NTP\ndescription: \"Keeps\\n   time.\"\n---\n",
            ),
            ("infra/hosts/web.md", "---\ntype: Host\n---\n# Web   host\n"),
            ("a-note.md", "No frontmatter at all.\n"),
            ("tools/helper.md", "# Tooling\n"),
        ])
    }

    const LEGACY_INFRA: &str = concat!(
        "# Infra\n\nOur systems, by hand.\n\n# Concepts\n\n",
        "* [Time server](time-server.md) - curated words\n",
    );

    fn sorted(mut notes: Vec<String>) -> Vec<String> {
        notes.sort();
        notes
    }

    #[test]
    fn repair_indexes_creates_what_an_empty_catalog_lacks() {
        let ws = MemWorkspace::default();

        let outcome = repair_indexes(&ws).unwrap();

        assert_eq!(outcome.touched, ["index.md", "log.md"]);
        assert_eq!(
            ws.read_to_string("index.md").unwrap(),
            "---\nokf_version: \"0.2\"\n---\n\n# Catalog\n\nTODO: say what this catalog holds.\n"
        );
        assert_eq!(
            ws.read_to_string("log.md").unwrap(),
            "# Knowledge Catalog Update Log\n"
        );
        assert_eq!(
            outcome.replay["index.md"],
            [ReplayOp::CreateIndex {
                folder: String::new()
            }]
        );
        assert_eq!(outcome.replay["log.md"], [ReplayOp::CreateLog]);
        assert_eq!(
            outcome.message,
            "repaired 2 file(s); 1 TODO placeholder(s) to fill in (see notes)"
        );
        assert_eq!(
            outcome.notes,
            [
                "created /index.md; replace its heading and TODO line",
                "created /log.md"
            ]
        );
    }

    #[test]
    fn repair_indexes_fills_a_legacy_catalog_and_keeps_its_curated_text() {
        let ws = legacy();

        let outcome = repair_indexes(&ws).unwrap();

        assert_eq!(
            outcome.touched,
            [
                "index.md",
                "log.md",
                "infra/hosts/index.md",
                "infra/index.md"
            ]
        );
        assert_eq!(
            ws.read_to_string("index.md").unwrap(),
            concat!(
                "---\nokf_version: \"0.2\"\n---\n\n# Catalog\n\n",
                "TODO: say what this catalog holds.\n\n# Concepts\n\n",
                "* [A note](a-note.md) - TODO: describe this concept\n\n# Sections\n\n",
                "* [Infra](infra/) - TODO: describe this folder\n",
            )
        );
        // The curated prose and listing stay; the missing ones are added.
        assert_eq!(
            ws.read_to_string("infra/index.md").unwrap(),
            concat!(
                "# Infra\n\nOur systems, by hand.\n\n# Concepts\n\n",
                "* [Time server](time-server.md) - curated words\n",
                "* [NTP](ntp.md) - Keeps time.\n\n# Sections\n\n",
                "* [Hosts](hosts/) - TODO: describe this folder\n",
            )
        );
        assert_eq!(
            ws.read_to_string("infra/hosts/index.md").unwrap(),
            concat!(
                "# Hosts\n\nTODO: say what this folder holds.\n\n# Concepts\n\n",
                "* [Web host](web.md) - TODO: describe this concept\n",
            )
        );
        // Machinery is never written: no write tool may change it.
        assert!(!ws.exists("tools/index.md"));
        assert_eq!(
            outcome.message,
            "repaired 4 file(s); 7 TODO placeholder(s) to fill in (see notes)"
        );
        assert_eq!(
            sorted(outcome.notes),
            [
                "created /index.md; replace its heading and TODO line",
                "created /infra/hosts/index.md; replace its TODO line",
                "created /log.md",
                "listed /a-note.md under its file name: it has no title",
                "listed /a-note.md with a TODO summary: it has no description",
                "listed /infra/ in /index.md with a TODO summary",
                "listed /infra/hosts/ in /infra/index.md with a TODO summary",
                "listed /infra/hosts/web.md with a TODO summary: it has no description",
            ]
        );
        // Every edit of an index is a replay op, so publish never overwrites
        // an index that changed meanwhile.
        assert_eq!(
            outcome.replay["infra/index.md"],
            [
                ReplayOp::ListBullet {
                    target: "infra/hosts/index.md".to_string(),
                    bullet: "* [Hosts](hosts/) - TODO: describe this folder".to_string(),
                    section: Some("Sections".to_string()),
                    create_section: true,
                },
                ReplayOp::ListBullet {
                    target: "infra/ntp.md".to_string(),
                    bullet: "* [NTP](ntp.md) - Keeps time.".to_string(),
                    section: None,
                    create_section: false,
                },
            ]
        );
    }

    #[test]
    fn repair_indexes_twice_changes_nothing_the_second_time() {
        let ws = legacy();
        repair_indexes(&ws).unwrap();
        let before = ws.read_to_string("infra/index.md").unwrap();

        let again = repair_indexes(&ws).unwrap();

        assert_eq!(again.touched, Vec::<String>::new());
        assert!(again.replay.is_empty() && again.notes.is_empty());
        assert_eq!(again.message, "indexes complete: nothing to repair");
        assert_eq!(ws.read_to_string("infra/index.md").unwrap(), before);
    }

    #[test]
    fn repair_indexes_accepts_any_wording_of_an_existing_listing() {
        // Absolute links, a folder linked by its index file, and a concept
        // listed under # Sections all count as listed: nothing is duplicated.
        let root = concat!(
            "---\nokf_version: \"0.2\"\n---\n# Root\n\n# Sections\n\n",
            "* [Ops, by hand](/ops/index.md) - curated\n* [Top](/top.md) - misfiled\n",
        );
        let ws = MemWorkspace::from_files([
            ("index.md", root),
            ("log.md", "# Log\n"),
            ("top.md", "---\ntype: Note\ntitle: Top\n---\n"),
            ("ops/index.md", "# Ops\n\n* [Run](/ops/run.md) - curated\n"),
            ("ops/run.md", "---\ntype: Note\ntitle: Run\n---\n"),
            ("ops/deep/index.md", "# Deep\n"),
        ]);

        let outcome = repair_indexes(&ws).unwrap();

        // Only the unlisted folder index is listed, in its parent's style.
        assert_eq!(outcome.touched, ["ops/index.md"]);
        assert_eq!(ws.read_to_string("index.md").unwrap(), root);
        assert_eq!(
            ws.read_to_string("ops/index.md").unwrap(),
            concat!(
                "# Ops\n\n* [Run](/ops/run.md) - curated\n\n# Sections\n\n",
                "* [Deep](/ops/deep/) - TODO: describe this folder\n",
            )
        );
        assert_eq!(
            outcome.notes,
            ["listed /ops/deep/ in /ops/index.md with a TODO summary"]
        );
    }

    #[test]
    fn repair_indexes_creates_the_missing_ancestors_of_a_nested_index() {
        // `infra/hosts/index.md` exists, `infra/index.md` does not: the
        // ancestor is created and listed, and the nested index listed in it.
        let hosts = "# Hosts\n\nCurated hosts.\n";
        let ws = MemWorkspace::from_files([
            ("index.md", "---\nokf_version: \"0.2\"\n---\n# Root\n"),
            ("log.md", "# Log\n"),
            ("infra/hosts/index.md", hosts),
        ]);

        let outcome = repair_indexes(&ws).unwrap();
        let again = repair_indexes(&ws).unwrap();

        assert_eq!(outcome.touched, ["infra/index.md", "index.md"]);
        assert_eq!(
            ws.read_to_string("infra/index.md").unwrap(),
            concat!(
                "# Infra\n\nTODO: say what this folder holds.\n\n# Sections\n\n",
                "* [Hosts](hosts/) - TODO: describe this folder\n",
            )
        );
        assert_eq!(
            ws.read_to_string("index.md").unwrap(),
            concat!(
                "---\nokf_version: \"0.2\"\n---\n# Root\n\n# Sections\n\n",
                "* [Infra](infra/) - TODO: describe this folder\n",
            )
        );
        assert_eq!(ws.read_to_string("infra/hosts/index.md").unwrap(), hosts);
        assert_eq!(
            outcome.message,
            "repaired 2 file(s); 3 TODO placeholder(s) to fill in (see notes)"
        );
        assert!(again.touched.is_empty() && again.notes.is_empty());
    }

    #[test]
    fn repair_indexes_keeps_a_curated_link_to_a_folder_whose_index_is_missing() {
        // The root links `infra/` already, but `infra/index.md` is gone: the
        // index is created, and the root keeps its one curated link.
        let root = concat!(
            "---\nokf_version: \"0.2\"\n---\n# Root\n\n# Sections\n\n",
            "* [Our systems](infra/) - curated\n",
        );
        let ws = MemWorkspace::from_files([
            ("index.md", root),
            ("log.md", "# Log\n"),
            (
                "infra/ntp.md",
                "---\ntype: Note\ntitle: NTP\ndescription: Keeps time.\n---\n",
            ),
        ]);

        let outcome = repair_indexes(&ws).unwrap();
        let again = repair_indexes(&ws).unwrap();

        assert_eq!(outcome.touched, ["infra/index.md"]);
        assert_eq!(ws.read_to_string("index.md").unwrap(), root);
        assert_eq!(
            ws.read_to_string("infra/index.md").unwrap(),
            concat!(
                "# Infra\n\nTODO: say what this folder holds.\n\n# Concepts\n\n",
                "* [NTP](ntp.md) - Keeps time.\n",
            )
        );
        assert!(!outcome.replay.contains_key("index.md"));
        assert_eq!(
            outcome.notes,
            ["created /infra/index.md; replace its TODO line"]
        );
        assert!(again.touched.is_empty() && again.notes.is_empty());
    }

    #[test]
    fn repair_indexes_skips_listings_no_link_can_reach_and_settles() {
        // A link's destination stops at whitespace or `)`, and `#` starts
        // its fragment: these names cannot be linked, so the repair notes
        // them instead of writing a listing no later run recognises. A
        // folder with a space still gets its index, and linkable files in
        // it are listed relatively; under an absolute-style parent the
        // folder's own listing would need its path, so it is skipped too.
        let note = "---\ntype: Note\ntitle: N\ndescription: D.\n---\n";
        let root = "---\nokf_version: \"0.2\"\n---\n# Root\n\n* [Top](/top.md) - curated\n";
        let ws = MemWorkspace::from_files([
            ("index.md", root),
            ("log.md", "# Log\n"),
            ("top.md", note),
            ("a note.md", note),
            ("x)y.md", note),
            ("h#1.md", note),
            ("tab\tx.md", note),
            ("my notes/a.md", note),
            ("ok/b.md", note),
        ]);

        let outcome = repair_indexes(&ws).unwrap();
        let again = repair_indexes(&ws).unwrap();

        assert_eq!(
            outcome.touched,
            ["my notes/index.md", "ok/index.md", "index.md"]
        );
        assert_eq!(
            ws.read_to_string("index.md").unwrap(),
            format!("{root}\n# Sections\n\n* [Ok](/ok/) - TODO: describe this folder\n")
        );
        assert!(ws
            .read_to_string("my notes/index.md")
            .unwrap()
            .ends_with("# Concepts\n\n* [N](a.md) - D.\n"));
        assert_eq!(
            sorted(
                outcome
                    .notes
                    .iter()
                    .filter(|line| line.starts_with("did not list"))
                    .cloned()
                    .collect()
            ),
            [
                "did not list /a note.md in /index.md: no markdown link to it can be written",
                "did not list /h#1.md in /index.md: no markdown link to it can be written",
                "did not list /my notes/ in /index.md: no markdown link to it can be written",
                "did not list /tab\tx.md in /index.md: no markdown link to it can be written",
                "did not list /x)y.md in /index.md: no markdown link to it can be written",
            ]
        );
        // The second run writes nothing; it only repeats the notes.
        assert!(again.touched.is_empty() && again.replay.is_empty());
        assert!(again
            .notes
            .iter()
            .all(|line| line.starts_with("did not list")));
    }

    #[test]
    fn repair_indexes_checks_the_whole_bullet_label_included() {
        // The path is linkable but the label is not: a folder `a]` gets the
        // heading `A]`, and a title ending in `\` escapes the closing `]`.
        // Neither bullet would parse, so neither is written, and the
        // repair settles.
        let ws = MemWorkspace::from_files([
            ("index.md", "---\nokf_version: \"0.2\"\n---\n# Root\n"),
            ("log.md", "# Log\n"),
            ("a]/index.md", "# A\n"),
            (
                "c.md",
                "---\ntype: Note\ntitle: 'C:\\'\ndescription: A drive.\n---\n",
            ),
            (
                "d.md",
                "---\ntype: Note\ntitle: D [draft]\ndescription: Kept.\n---\n",
            ),
        ]);

        let outcome = repair_indexes(&ws).unwrap();
        let again = repair_indexes(&ws).unwrap();

        // The title with brackets is escaped and listed; the others aren't.
        assert_eq!(
            ws.read_to_string("index.md").unwrap(),
            concat!(
                "---\nokf_version: \"0.2\"\n---\n# Root\n\n# Concepts\n\n",
                "* [D \\[draft\\]](d.md) - Kept.\n",
            )
        );
        assert_eq!(
            sorted(outcome.notes),
            [
                "did not list /a]/ in /index.md: no markdown link to it can be written",
                "did not list /c.md in /index.md: no markdown link to it can be written",
            ]
        );
        assert!(again.touched.is_empty() && again.replay.is_empty());
    }

    #[test]
    fn repair_indexes_lists_a_concept_whose_frontmatter_does_not_parse() {
        let ws = MemWorkspace::from_files([
            ("index.md", "---\nokf_version: \"0.2\"\n---\n# Root\n"),
            ("log.md", "# Log\n"),
            (
                "broken.md",
                "---\ntitle: [unclosed\n---\n# Broken heading\n",
            ),
        ]);

        let outcome = repair_indexes(&ws).unwrap();

        assert_eq!(
            ws.read_to_string("index.md").unwrap(),
            concat!(
                "---\nokf_version: \"0.2\"\n---\n# Root\n\n# Concepts\n\n",
                "* [Broken heading](broken.md) - TODO: describe this concept\n",
            )
        );
        assert_eq!(
            outcome.notes,
            ["listed /broken.md with a TODO summary: it has no description"]
        );
    }

    #[test]
    fn starter_files_are_a_titled_root_index_and_a_dated_log() {
        let [(index_path, index_text), (log_path, log_text)] =
            starter_files("pg", "  Systems\n and   people. ", ctx().now);
        let [(_, untitled), _] = starter_files("bare", " ", ctx().now);

        assert_eq!((index_path, log_path), ("index.md", "log.md"));
        assert_eq!(
            index_text,
            "---\nokf_version: \"0.2\"\n---\n\n# pg\n\nSystems and people.\n"
        );
        assert_eq!(
            untitled,
            "---\nokf_version: \"0.2\"\n---\n\n# bare\n\nTODO: say what this catalog holds.\n"
        );
        assert_eq!(
            log_text,
            concat!(
                "# Knowledge Catalog Update Log\n\n## 2026-10-27\n\n",
                "* **Creation:** Created the catalog.\n",
            )
        );
        // The bookkeeping tools work on them at once.
        let ws = MemWorkspace::from_files([
            (index_path, index_text.as_str()),
            (log_path, log_text.as_str()),
            ("a.md", "---\ntype: Note\ntitle: A\n---\n"),
        ]);
        assert_eq!(
            index(&ws, &ctx(), "a", "first", None, None)
                .unwrap()
                .message,
            "index.md: * [A](a.md) - first"
        );
        assert_eq!(
            log(&ws, &ctx(), LogKind::Update, "x").unwrap().touched,
            ["log.md"]
        );
    }

    #[test]
    fn repair_catalog_names_the_root_and_logs_what_it_wrote() {
        let ws = MemWorkspace::from_files([("infra/ntp.md", "---\ntype: S\ntitle: NTP\n---\n")]);

        let outcome = repair_catalog(&ws, &ctx(), "pg", "Team notes.").unwrap();
        let again = repair_catalog(&ws, &ctx(), "pg", "Team notes.").unwrap();

        assert_eq!(outcome.touched, ["index.md", "log.md", "infra/index.md"]);
        assert!(
            outcome.replay.is_empty(),
            "a direct write records no replay"
        );
        assert_eq!(
            ws.read_to_string("index.md").unwrap(),
            concat!(
                "---\nokf_version: \"0.2\"\n---\n\n# pg\n\nTeam notes.\n\n# Sections\n\n",
                "* [Infra](infra/) - TODO: describe this folder\n",
            )
        );
        assert_eq!(
            ws.read_to_string("log.md").unwrap(),
            concat!(
                "# Knowledge Catalog Update Log\n\n## 2026-10-27\n\n",
                "* **Update:** Repaired the catalog's indexes (okf-mcp catalog repair-indexes): ",
                "wrote index.md, log.md, infra/index.md.\n",
            )
        );
        assert_eq!(
            outcome.notes[0],
            "created /index.md from the catalog's name and description"
        );
        // infra/index.md and its listing (2), and NTP's missing description.
        assert_eq!(
            outcome.message,
            "repaired 3 file(s); 3 TODO placeholder(s) to fill in (see notes)"
        );
        assert!(again.touched.is_empty(), "{again:?}");
        assert_eq!(again.message, "indexes complete: nothing to repair");
    }

    #[test]
    fn repair_catalog_of_a_complete_catalog_logs_nothing() {
        let ws = MemWorkspace::from_files([
            ("index.md", "---\nokf_version: \"0.2\"\n---\n# Root\n"),
            ("log.md", "# Log\n"),
        ]);

        let outcome = repair_catalog(&ws, &ctx(), "pg", "").unwrap();

        assert!(outcome.touched.is_empty());
        assert_eq!(ws.read_to_string("log.md").unwrap(), "# Log\n");
    }
}
