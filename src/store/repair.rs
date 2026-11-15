//! The operator's index repair (`okf-mcp catalog repair-indexes`, issue
//! #426): [`crate::okf::ops::repair_catalog`] run on a catalog's live
//! documents and written at once, in one transaction under the catalog's
//! lock, as the command-line import writes.
//!
//! Agents repair through a changeset instead (the `repair_indexes` tool).
//! This path exists for catalogs created before new catalogs got starter
//! files, whose owners may never connect an agent that could.
//!
//! Lock order: as import's. `audit::lock_people` first (the catalog), then
//! the catalog's advisory lock (`catalogs::lock_by_name`), then the
//! documents. The advisory lock excludes publishes and imports. A changeset
//! write may run meanwhile; its entries keep their base revisions, and every
//! path this writes gets a new revision, so a later publish of such an
//! entry replays its bookkeeping or reports a conflict, never overwrites.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgPool;

use crate::error::OkfError;
use crate::okf::ops::{repair_catalog, OpContext, OpOutcome};
use crate::okf::rules;
use crate::okf::snapshot::Snapshot;
use crate::okf::workspace::{MemWorkspace, StoredFile};
use crate::store::audit::{self, Action, AuditActor, People};
use crate::store::catalogs::{self, CatalogEvent};
use crate::store::documents::{self, Derived, DocumentWrite, RevisionOp, RevisionRecord};
use crate::store::import::{existing_rows, ExistingRow};

/// The `revisions.message` of a repair.
pub const REPAIR_MESSAGE: &str = "repair indexes";

/// How to repair.
#[derive(Debug, Clone, Copy)]
pub struct RepairOptions<'a> {
    /// Compute and report the repair, but write nothing.
    pub dry_run: bool,
    /// Recorded as `updated_by` and the revisions' `actor`.
    pub actor: &'a str,
    /// Who, for the audit log's `catalog.repaired` entry.
    pub audit: &'a AuditActor,
    /// The time of every row written, and of the log entry (server clock).
    pub now: DateTime<Utc>,
}

/// What a repair did (or, in a dry run, would do).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RepairReport {
    /// The catalog.
    pub catalog: String,
    /// `/`-prefixed paths written, the root index first.
    pub written: Vec<String>,
    /// The op's summary line.
    pub message: String,
    /// One per created file and per TODO placeholder.
    pub notes: Vec<String>,
    /// The catalog's generation after the repair; `None` for a dry run or a
    /// repair that wrote nothing.
    pub generation: Option<i64>,
}

/// Repairs a catalog's indexes and log in place (see the module docs).
///
/// # Arguments
/// * `pool` - the database.
/// * `catalog` - the catalog's name.
/// * `options` - dry run, actor and time.
///
/// # Returns
/// What was (or would be) written. A catalog with nothing missing writes
/// nothing, bumps nothing and records nothing.
///
/// # Errors
/// [`OkfError::CatalogNotSelected`] for an unknown catalog, or one deleted
/// while this waited for its lock; [`OkfError::InvalidPath`] when its stored
/// paths cannot form a workspace, or a written path is machinery;
/// [`OkfError::SecretDetected`] or [`OkfError::FrontmatterParse`] from the
/// content rules (only text the catalog already held can trigger them, in a
/// listing's title or summary); [`OkfError::Database`], a failed audit write
/// included. Every error rolls back, leaving the catalog as it was.
pub async fn repair(
    pool: &PgPool,
    catalog: &str,
    options: &RepairOptions<'_>,
) -> Result<RepairReport, OkfError> {
    // Dropping `tx` without `commit()` (any `?`, or a dry run) rolls back.
    let mut tx = pool.begin().await?;
    audit::lock_people(
        &mut tx,
        options.audit,
        &People {
            catalogs: &[catalog],
            ..People::default()
        },
    )
    .await?;
    let found = catalogs::lock_by_name(&mut tx, catalog).await?;
    let existing = existing_rows(&mut tx, found.id).await?;
    let live: Vec<(String, StoredFile)> = existing
        .values()
        .filter(|row| row.is_live())
        .map(|row| {
            let file = StoredFile {
                content: row.content.clone(),
                executable: row.executable,
            };
            (row.path.clone(), file)
        })
        .collect();
    let (name, description) = (found.name.clone(), found.description.clone());
    let (actor, now) = (options.actor.to_string(), options.now);
    // The op reads every document and parses concepts: CPU work, so it runs
    // on a blocking thread. `move` hands it owned copies of what it needs.
    let computed =
        tokio::task::spawn_blocking(move || compute(live, &name, &description, &actor, now))
            .await
            // A panic in the task is re-raised here, as if it ran inline.
            .unwrap_or_else(|error| std::panic::resume_unwind(error.into_panic()))?;
    let Computed { outcome, files } = computed;
    let mut report = RepairReport {
        catalog: found.name.clone(),
        written: outcome
            .touched
            .iter()
            .map(|path| format!("/{path}"))
            .collect(),
        message: outcome.message,
        notes: outcome.notes,
        generation: None,
    };
    if options.dry_run || files.is_empty() {
        return Ok(report);
    }
    write_files(&mut tx, found.id, &existing, &files, options).await?;
    let generation: i64 = sqlx::query_scalar(
        "UPDATE catalogs SET generation = generation + 1 WHERE id = $1 RETURNING generation",
    )
    .bind(found.id)
    .fetch_one(&mut *tx)
    .await?;
    catalogs::record_catalog(
        &mut tx,
        options.audit,
        CatalogEvent {
            action: Action::CatalogRepaired,
            catalog_id: found.id,
            catalog,
            details: serde_json::json!({
                "files": report.written,
                "generation": generation,
            }),
        },
        options.now,
    )
    .await?;
    tx.commit().await?;
    report.generation = Some(generation);
    Ok(report)
}

/// The op's outcome and the files it wrote, checked by the write rules.
struct Computed {
    /// The op's outcome.
    outcome: OpOutcome,
    /// `(path, file, derived columns)` per written path, in write order.
    files: Vec<(String, StoredFile, Derived)>,
}

/// Runs the repair on a copy of the live documents and checks every file
/// it wrote against the write rules (runs on a blocking thread).
fn compute(
    live: Vec<(String, StoredFile)>,
    name: &str,
    description: &str,
    actor: &str,
    now: DateTime<Utc>,
) -> Result<Computed, OkfError> {
    let workspace = MemWorkspace::from_stored(live)?;
    let ctx = OpContext { now, by: actor };
    let outcome = repair_catalog(&workspace, &ctx, name, description)?;
    let mut written = Vec::with_capacity(outcome.touched.len());
    for path in &outcome.touched {
        rules::check_path(path)?;
        let file = workspace
            .stored(path)
            .expect("the repair wrote every path it touched");
        let text = String::from_utf8_lossy(&file.content);
        rules::check_content(path, &text)?;
        written.push((path.clone(), file));
    }
    // The derived columns (kind, title) of the written files only: they are
    // indexes and the log, whose columns do not depend on other documents.
    let snapshot = Snapshot::build(&MemWorkspace::from_stored(written.clone())?)?;
    let files = written
        .into_iter()
        .map(|(path, file)| {
            let derived = Derived::of(snapshot.get(&path));
            (path, file, derived)
        })
        .collect();
    Ok(Computed { outcome, files })
}

/// Writes each repaired file as the next revision of its path (1 for a new
/// path; a tombstone's numbers continue), with a `create` or `update`
/// revision carrying [`REPAIR_MESSAGE`].
async fn write_files(
    conn: &mut sqlx::PgConnection,
    catalog_id: i64,
    existing: &HashMap<String, ExistingRow>,
    files: &[(String, StoredFile, Derived)],
    options: &RepairOptions<'_>,
) -> Result<(), OkfError> {
    for (path, file, derived) in files {
        let previous = existing.get(path);
        let write = DocumentWrite {
            catalog_id,
            path,
            revision: previous.map_or(1, |row| row.revision + 1),
            actor: options.actor,
            now: options.now,
        };
        let record = RevisionRecord {
            op: if previous.is_some_and(ExistingRow::is_live) {
                RevisionOp::Update
            } else {
                RevisionOp::Create
            },
            file: Some(file),
            changeset_id: None,
            message: REPAIR_MESSAGE,
        };
        documents::write_document(conn, &write, file, derived).await?;
        documents::record_revision(conn, &write, &record).await?;
    }
    Ok(())
}

/// The catalogs with no live root `index.md`, by name: what
/// `repair-indexes --missing-only` repairs.
///
/// # Arguments
/// * `pool` - the database.
///
/// # Returns
/// Their names, sorted.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn missing_root_index(pool: &PgPool) -> Result<Vec<String>, OkfError> {
    let names = sqlx::query_scalar(
        "SELECT c.name FROM catalogs c WHERE NOT EXISTS ( \
             SELECT 1 FROM documents d \
             WHERE d.catalog_id = c.id AND d.path = 'index.md' AND d.deleted_at IS NULL) \
         ORDER BY c.name",
    )
    .fetch_all(pool)
    .await?;
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{test_db, TEST_TEAM};
    use crate::store::audit::testing::entries;
    use crate::store::import::{import, ImportOptions, ImportScope};
    use chrono::TimeZone;

    const NOTE: &str = "---\ntype: Note\ntitle: A\ndescription: A note.\n---\n# A\n";

    fn at(minute: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 15, 12, minute, 0).unwrap()
    }

    /// Creates `name`, then imports `files` into it: a catalog as one
    /// created before starter files and imported without indexes is (the
    /// import drops the starter files it lacks).
    async fn legacy(pool: &PgPool, name: &str, description: &str, files: &[(&str, &str)]) {
        let new = catalogs::NewCatalog {
            name,
            description,
            owner_team: TEST_TEAM,
        };
        catalogs::create(pool, &new, &AuditActor::cli(), at(0))
            .await
            .unwrap();
        let options = ImportOptions {
            replace: false,
            scope: ImportScope::Everything,
            expect_generation: None,
            actor: "test/import",
            audit: &AuditActor::cli(),
            now: at(1),
        };
        import(pool, name, crate::archive::files(files), &options)
            .await
            .unwrap();
    }

    fn options(audit: &AuditActor, dry_run: bool) -> RepairOptions<'_> {
        RepairOptions {
            dry_run,
            actor: "okf-mcp/repair-indexes",
            audit,
            now: at(2),
        }
    }

    async fn live_text(pool: &PgPool, path: &str) -> Option<String> {
        sqlx::query_scalar::<_, Vec<u8>>(
            "SELECT content FROM documents WHERE path = $1 AND deleted_at IS NULL",
        )
        .bind(path)
        .fetch_optional(pool)
        .await
        .unwrap()
        .map(|bytes| String::from_utf8(bytes).unwrap())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_repair_writes_the_missing_files_once_and_records_it() {
        let db = test_db().await;
        legacy(
            &db.pool,
            "pg",
            "Team notes.",
            &[("a.md", NOTE), ("infra/b.md", NOTE)],
        )
        .await;
        let audit = AuditActor::cli();

        let report = repair(&db.pool, "pg", &options(&audit, false))
            .await
            .unwrap();
        let again = repair(&db.pool, "pg", &options(&audit, false))
            .await
            .unwrap();

        assert_eq!(
            report.written,
            ["/index.md", "/log.md", "/infra/index.md"],
            "{report:?}"
        );
        assert_eq!(report.generation, Some(2));
        let root = live_text(&db.pool, "index.md").await.unwrap();
        // The root a new catalog would get, plus the listings.
        assert!(
            root.starts_with("---\nokf_version: \"0.2\"\n---\n\n# pg\n\nTeam notes.\n"),
            "{root}"
        );
        assert!(root.contains("* [A](a.md) - A note."), "{root}");
        assert!(
            root.contains("* [Infra](infra/) - TODO: describe this folder"),
            "{root}"
        );
        let log = live_text(&db.pool, "log.md").await.unwrap();
        assert!(
            log.contains("* **Update:** Repaired the catalog's indexes (okf-mcp catalog repair-indexes): wrote index.md, log.md, infra/index.md."),
            "{log}"
        );
        // The starter files' tombstones (revision 2) continue their numbers.
        let history: Vec<(String, i32, String, String, String)> = sqlx::query_as(
            "SELECT path, revision, op, actor, message FROM revisions \
             WHERE message = $1 ORDER BY path",
        )
        .bind(REPAIR_MESSAGE)
        .fetch_all(&db.pool)
        .await
        .unwrap();
        let row = |path: &str, revision| {
            (
                path.to_string(),
                revision,
                "create".to_string(),
                "okf-mcp/repair-indexes".to_string(),
                REPAIR_MESSAGE.to_string(),
            )
        };
        assert_eq!(
            history,
            [
                row("index.md", 3),
                row("infra/index.md", 1),
                row("log.md", 3)
            ]
        );
        let kind: Option<String> =
            sqlx::query_scalar("SELECT kind FROM documents WHERE path = 'infra/index.md'")
                .fetch_one(&db.pool)
                .await
                .unwrap();
        assert_eq!(kind.as_deref(), Some("index"));
        // A second run finds nothing missing: no write, bump or entry.
        assert!(
            again.written.is_empty() && again.generation.is_none(),
            "{again:?}"
        );
        assert_eq!(again.message, "indexes complete: nothing to repair");
        let recorded = entries(&db.pool, "catalog.repaired").await;
        assert_eq!(recorded.len(), 1, "{recorded:?}");
        assert_eq!(recorded[0].target.as_deref(), Some("pg"));
        assert_eq!(
            recorded[0].details,
            serde_json::json!({"files": ["/index.md", "/log.md", "/infra/index.md"], "generation": 2})
        );
        assert_eq!(catalogs::find(&db.pool, "pg").await.unwrap().generation, 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_repair_that_waited_for_a_publish_builds_on_what_it_wrote() {
        let db = test_db().await;
        legacy(&db.pool, "pg", "", &[("a.md", NOTE)]).await;
        let catalog = catalogs::find(&db.pool, "pg").await.unwrap();
        // Hold the catalog's lock, as a publish in progress does, and write
        // a curated root index.md under it.
        let mut holder = db.pool.begin().await.unwrap();
        catalogs::lock(&mut holder, catalog.id).await.unwrap();
        let pool = db.pool.clone();
        let waiting = tokio::spawn(async move {
            let audit = AuditActor::cli();
            repair(&pool, "pg", &options(&audit, false)).await
        });
        catalogs::tests::until_a_lock_is_awaited(&db.pool).await;
        let curated = "---\nokf_version: \"0.2\"\n---\n# Ours\n\nCurated words.\n";
        let file = StoredFile {
            content: curated.as_bytes().to_vec(),
            executable: false,
        };
        // The starter index.md's tombstone is revision 2.
        let write = DocumentWrite {
            catalog_id: catalog.id,
            path: "index.md",
            revision: 3,
            actor: "publisher",
            now: at(1),
        };
        documents::write_document(&mut holder, &write, &file, &Derived::default())
            .await
            .unwrap();
        holder.commit().await.unwrap();
        let repaired = tokio::time::timeout(std::time::Duration::from_secs(30), waiting)
            .await
            .expect("the repair waited for the held catalog lock and never finished")
            .unwrap()
            .unwrap();

        // The repair saw the published root: it kept it, listed the concept
        // in it, and continued its revision.
        // The root existed, so only the log was created; the listing came after.
        assert_eq!(repaired.written, ["/log.md", "/index.md"], "{repaired:?}");
        let root = live_text(&db.pool, "index.md").await.unwrap();
        assert!(root.starts_with(curated), "{root}");
        assert!(root.contains("* [A](a.md) - A note."), "{root}");
        let revision: i32 =
            sqlx::query_scalar("SELECT revision FROM documents WHERE path = 'index.md'")
                .fetch_one(&db.pool)
                .await
                .unwrap();
        assert_eq!(revision, 4);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_dry_run_reports_the_repair_and_writes_nothing() {
        let db = test_db().await;
        legacy(&db.pool, "pg", "", &[("a.md", NOTE)]).await;
        let audit = AuditActor::cli();

        let dry = repair(&db.pool, "pg", &options(&audit, true))
            .await
            .unwrap();

        assert_eq!(dry.written, ["/index.md", "/log.md"]);
        assert_eq!(dry.generation, None);
        // No description: the root's TODO line is a placeholder to fill in.
        assert!(dry
            .notes
            .iter()
            .any(|note| note.contains("no description, so replace its TODO line")));
        assert_eq!(live_text(&db.pool, "index.md").await, None);
        assert!(entries(&db.pool, "catalog.repaired").await.is_empty());
        assert_eq!(catalogs::find(&db.pool, "pg").await.unwrap().generation, 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn missing_root_index_lists_only_catalogs_without_one() {
        let db = test_db().await;
        legacy(&db.pool, "old", "", &[("a.md", NOTE)]).await;
        let new = catalogs::NewCatalog {
            name: "fresh",
            description: "",
            owner_team: TEST_TEAM,
        };
        catalogs::create(&db.pool, &new, &AuditActor::cli(), at(0))
            .await
            .unwrap();
        let audit = AuditActor::cli();

        let before = missing_root_index(&db.pool).await.unwrap();
        repair(&db.pool, "old", &options(&audit, false))
            .await
            .unwrap();
        let after = missing_root_index(&db.pool).await.unwrap();
        let unknown = repair(&db.pool, "nope", &options(&audit, false)).await;

        assert_eq!(before, ["old"]);
        assert!(after.is_empty(), "{after:?}");
        assert!(matches!(unknown, Err(OkfError::CatalogNotSelected { .. })));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_failed_audit_write_fails_the_repair() {
        let db = test_db().await;
        legacy(&db.pool, "pg", "", &[("a.md", NOTE)]).await;
        crate::http::api::testing::fail_on(&db.pool, "BEFORE INSERT ON audit_entries FOR EACH ROW")
            .await;
        let audit = AuditActor::cli();

        let refused = repair(&db.pool, "pg", &options(&audit, false)).await;

        assert!(refused.is_err());
        assert_eq!(live_text(&db.pool, "index.md").await, None);
        assert_eq!(catalogs::find(&db.pool, "pg").await.unwrap().generation, 1);
    }
}
