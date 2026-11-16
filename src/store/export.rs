//! Export (design "Export"): a catalog's live documents as a `.tar.gz`.

use std::io::Write;

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::archive::{self, ExportFile};
use crate::auth::CatalogScope;
use crate::error::OkfError;
use crate::store::audit::{self, Action, AuditActor, Entry, People, Target};
use crate::store::{catalogs, documents};

/// Writes a catalog's live documents to `out` as a `.tar.gz`: no top-level
/// directory, modes 0644/0755, each file's mtime its `updated_at`, and no
/// tombstones.
///
/// The catalog lookup and the document read share one read-only
/// `REPEATABLE READ` transaction, so they see one snapshot (taken by the
/// first query): a catalog deleted concurrently is either unknown (deleted
/// first) or exported whole as it was, never exported empty or half-deleted.
/// Compressing is CPU work and runs on a blocking thread.
///
/// Every export is recorded (`catalog.exported`, spec Part 5), whoever asks,
/// scoped to the owner team the read's snapshot saw, in a short transaction
/// of its own once the read has ended and before compressing: a failed record fails the export, so no archive leaves
/// without its entry.
///
/// # Arguments
/// * `pool` - the database.
/// * `catalog` - the catalog's name.
/// * `out` - where to write (a `Vec<u8>`, a file, an HTTP body buffer).
/// * `options` - who exports and when, and the incarnation expected.
///
/// # Returns
/// `out`, with the archive finished, and the catalog generation the archive
/// holds (for `import --expect-generation`).
///
/// # Errors
/// [`OkfError::CatalogNotSelected`] for an unknown catalog;
/// [`OkfError::Busy`] when it is no longer `options.expect_catalog_id`
/// (checked in the snapshot, before any document is read);
/// [`OkfError::Database`]; [`OkfError::Io`] if writing fails.
// `W: Write + Send + 'static`: `out` moves to a blocking thread, so it must
// be safe to send there (`Send`) and must not borrow anything that thread
// could outlive (`'static`). `Vec<u8>` and `File` both qualify.
#[tracing::instrument(level = "debug", skip_all)]
pub async fn export<W>(
    pool: &PgPool,
    catalog: &str,
    out: W,
    options: &ExportOptions<'_>,
) -> Result<(W, i64), OkfError>
where
    W: Write + Send + 'static,
{
    let ExportOptions {
        audit,
        now,
        expect_catalog_id,
    } = *options;
    let mut tx = pool.begin().await?;
    // Must be the transaction's first statement. Under the default
    // READ COMMITTED each query would see its own snapshot.
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    let found = catalogs::find(&mut *tx, catalog).await?;
    catalogs::check_incarnation(catalog, found.id, expect_catalog_id)?;
    let rows = documents::published(&mut *tx, found.id).await?;
    // The owner team's audit id, in the same snapshot as the rows: the
    // entry belongs to the team that owned what was exported, even if the
    // catalog moves or is deleted before the entry is written. A plain read
    // (every team has its `audit_teams` row from creation, migration 0014),
    // since a read-only transaction cannot run `audit_team_ref`.
    let team: Option<i64> = sqlx::query_scalar(
        "SELECT a.id FROM catalogs c JOIN audit_teams a ON a.team_id = c.owner_team_id \
         WHERE c.id = $1",
    )
    .bind(found.id)
    .fetch_optional(&mut *tx)
    .await?;
    // Nothing was written; ending the transaction now frees its connection
    // before the (possibly long) compression.
    tx.commit().await?;
    let exported = Exported {
        catalog,
        scope: CatalogScope { id: found.id, team },
        generation: found.generation,
        files: rows.len(),
    };
    record_export(pool, &exported, audit, now).await?;
    // `move` hands `rows` and `out` to the closure; `ExportFile`s borrow from
    // `rows` inside it, so nothing is copied.
    let caller = crate::okf::caller::Caller::capture();
    let archive = tokio::task::spawn_blocking(move || {
        let _entered = caller.enter();
        let files: Vec<ExportFile<'_>> = rows
            .iter()
            .map(|row| ExportFile {
                path: &row.path,
                content: &row.content,
                executable: row.executable,
                modified: row.updated_at,
            })
            .collect();
        archive::write_tarball(out, &files)
    })
    .await
    // A panic in the blocking task is re-raised here, as if it ran inline
    // (as `import` does).
    .unwrap_or_else(|error| std::panic::resume_unwind(error.into_panic()))?;
    Ok((archive, found.generation))
}

/// How to export.
// `Copy`: a borrow, a timestamp and an id, so `export` copies the fields out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportOptions<'a> {
    /// Who exports, for the `catalog.exported` entry.
    pub audit: &'a AuditActor,
    /// When (server clock).
    pub now: DateTime<Utc>,
    /// Refuse unless the catalog is still this `catalogs.id`, the
    /// incarnation the caller was admitted to; `None` checks nothing (the
    /// command line).
    pub expect_catalog_id: Option<i64>,
}

/// What one export read, as its entry records it: all from the read's
/// snapshot.
struct Exported<'a> {
    /// The catalog's name.
    catalog: &'a str,
    /// Its `catalogs.id` and owner team's audit id when it was read.
    scope: CatalogScope,
    /// The generation the archive holds.
    generation: i64,
    /// How many files it holds.
    files: usize,
}

/// Records `catalog.exported` in a transaction of its own, scoped as the
/// export's snapshot read it. `audit::lock_people` comes first, the lock
/// order every recorder takes; with the team given as its audit id, the
/// entry resolves only the actor (and its credential's owner).
///
/// # Arguments
/// * `pool` - the database.
/// * `exported` - what was exported, and its scope then.
/// * `audit` - who exports.
/// * `now` - when.
///
/// # Errors
/// [`OkfError::Database`].
#[tracing::instrument(level = "trace", skip_all)]
async fn record_export(
    pool: &PgPool,
    exported: &Exported<'_>,
    audit: &AuditActor,
    now: DateTime<Utc>,
) -> Result<(), OkfError> {
    let mut tx = pool.begin().await?;
    audit::lock_people(&mut tx, audit, &People::default()).await?;
    // `into_iter().collect()` turns the `Option` into a zero- or one-item Vec.
    let teams: Vec<i64> = exported.scope.team.into_iter().collect();
    let entry = Entry {
        action: Action::CatalogExported,
        target: Target::Catalog(exported.catalog),
        person: None,
        teams: &teams,
        details: serde_json::json!({
            "generation": exported.generation,
            "files": exported.files,
            "catalog_id": exported.scope.id,
        }),
    };
    audit::record(&mut tx, audit, &entry, now).await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::Files;
    use crate::db::test_db;
    use crate::okf::workspace::StoredFile;
    use crate::store::import::{import, ImportOptions, ImportScope};
    use chrono::{TimeZone, Utc};

    fn stored(content: &[u8], executable: bool) -> StoredFile {
        StoredFile {
            content: content.to_vec(),
            executable,
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_export_holds_the_live_documents_exactly() {
        let db = test_db().await;
        let now = Utc.with_ymd_and_hms(2026, 11, 2, 12, 0, 0).unwrap();
        catalogs::create(
            &db.pool,
            &crate::store::catalogs::NewCatalog {
                name: "pg",
                description: "",
                owner_team: crate::db::TEST_TEAM,
            },
            &crate::store::audit::AuditActor::cli(),
            now,
        )
        .await
        .unwrap();
        let mut files = Files::new();
        files.insert("a.md".into(), stored(b"# A\n", false));
        files.insert("tools/run.sh".into(), stored(b"#!/bin/sh\n\0", true));
        files.insert("gone.md".into(), stored(b"# Gone\n", false));
        let options = ImportOptions {
            replace: false,
            scope: ImportScope::Everything,
            expect_generation: None,
            expect_catalog_id: None,
            actor: "t",
            audit: &crate::store::audit::AuditActor::cli(),
            now,
        };
        import(&db.pool, "pg", files.clone(), &options)
            .await
            .unwrap();
        // A later import that changes `a.md` and deletes `gone.md`:
        // `tools/run.sh` keeps its first `updated_at`.
        let later = Utc.with_ymd_and_hms(2026, 11, 2, 13, 0, 0).unwrap();
        files.remove("gone.md");
        files.insert("a.md".into(), stored(b"# A, again\n", false));
        let options = ImportOptions {
            replace: true,
            scope: ImportScope::Everything,
            expect_generation: None,
            expect_catalog_id: None,
            actor: "t",
            audit: &crate::store::audit::AuditActor::cli(),
            now: later,
        };
        import(&db.pool, "pg", files.clone(), &options)
            .await
            .unwrap();

        let (tarball, generation) = export(
            &db.pool,
            "pg",
            Vec::new(),
            &ExportOptions {
                audit: &AuditActor::cli(),
                now: Utc::now(),
                expect_catalog_id: None,
            },
        )
        .await
        .unwrap();

        // Two imports wrote, so the archive holds generation 2.
        assert_eq!(generation, 2);
        // The tombstone is not exported; everything else reads back exactly.
        assert_eq!(
            archive::read_tarball(tarball.as_slice(), u64::MAX).unwrap(),
            files
        );
        // Each entry's mtime is its document's `updated_at`.
        let mut entries = tar::Archive::new(flate2::read::GzDecoder::new(tarball.as_slice()));
        let mtimes: Vec<(String, u64)> = entries
            .entries()
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                let path = entry.path().unwrap().to_string_lossy().into_owned();
                (path, entry.header().mtime().unwrap())
            })
            .collect();
        let seconds = |time: chrono::DateTime<Utc>| u64::try_from(time.timestamp()).unwrap();
        assert_eq!(
            mtimes,
            [
                ("a.md".to_string(), seconds(later)),
                ("tools/run.sh".to_string(), seconds(now)),
            ]
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_cli_export_is_recorded_as_the_cli() {
        let db = test_db().await;
        let now = Utc.with_ymd_and_hms(2026, 11, 13, 12, 0, 0).unwrap();
        catalogs::create(
            &db.pool,
            &catalogs::NewCatalog {
                name: "pg",
                description: "",
                owner_team: crate::db::TEST_TEAM,
            },
            &AuditActor::cli(),
            now,
        )
        .await
        .unwrap();

        let catalog_id = catalogs::find(&db.pool, "pg").await.unwrap().id;
        let (_, generation) = export(
            &db.pool,
            "pg",
            Vec::new(),
            &ExportOptions {
                audit: &AuditActor::cli(),
                now,
                expect_catalog_id: None,
            },
        )
        .await
        .unwrap();

        let entries = crate::store::audit::testing::entries(&db.pool, "catalog.exported").await;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].source, "cli");
        assert_eq!(entries[0].target.as_deref(), Some("pg"));
        assert_eq!(entries[0].teams, [crate::db::TEST_USER]);
        assert_eq!(
            entries[0].details,
            serde_json::json!({"generation": generation, "files": 2, "catalog_id": catalog_id})
        );
    }

    /// Creates catalog `pg` (owned by the test user's personal team), team
    /// `family` and user `staff`; returns an actor acting as `staff`, whose
    /// user row a test can hold to stop an export between its read and its
    /// entry (the entry's transaction locks the actor's user first).
    async fn export_race_setup(pool: &PgPool) -> AuditActor {
        let cli = AuditActor::cli();
        catalogs::create(
            pool,
            &catalogs::NewCatalog {
                name: "pg",
                description: "",
                owner_team: crate::db::TEST_TEAM,
            },
            &cli,
            Utc::now(),
        )
        .await
        .unwrap();
        crate::store::teams::create(
            pool,
            &crate::store::teams::NewTeam {
                name: "family",
                display_name: "",
                owner: crate::db::TEST_USER,
            },
            &cli,
            Utc::now(),
        )
        .await
        .unwrap();
        let staff = crate::store::users::NewUser {
            name: "staff",
            display_name: "",
        };
        crate::store::users::create(pool, &staff, &cli, Utc::now())
            .await
            .unwrap();
        let staff_id: String = sqlx::query_scalar("SELECT id FROM users WHERE name = 'staff'")
            .fetch_one(pool)
            .await
            .unwrap();
        AuditActor {
            source: crate::store::audit::Source::Console,
            credential: None,
            user_id: Some(staff_id),
            label: "staff".to_string(),
            via: crate::store::audit::Via::Session,
            operator: true,
            provisioner: false,
            ip: None,
        }
    }

    /// Runs an export of `pg` as `actor` while its entry's first lock (the
    /// actor's user row) is held, runs `between` once the export waits on
    /// it, then lets it finish.
    async fn export_across<F, Fut>(pool: &PgPool, actor: AuditActor, between: F)
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        let mut hold = pool.begin().await.unwrap();
        sqlx::query("SELECT 1 FROM users WHERE id = $1 FOR UPDATE")
            .bind(&actor.user_id)
            .execute(&mut *hold)
            .await
            .unwrap();
        let exporting = tokio::spawn({
            let pool = pool.clone();
            async move {
                export(
                    &pool,
                    "pg",
                    Vec::new(),
                    &ExportOptions {
                        audit: &actor,
                        now: Utc::now(),
                        expect_catalog_id: None,
                    },
                )
                .await
            }
        });
        // The read has committed; the entry waits on the held row.
        crate::http::api::testing::wait_for_lock_waiters(pool, 1).await;
        between().await;
        hold.rollback().await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(30), exporting)
            .await
            .expect("the export finishes once the row is released")
            .unwrap()
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_export_belongs_to_the_owner_it_read_even_if_the_catalog_moves_or_goes() {
        let db = test_db().await;
        let actor = export_race_setup(&db.pool).await;
        let pg = catalogs::find(&db.pool, "pg").await.unwrap().id;

        export_across(&db.pool, actor.clone(), || async {
            catalogs::set_owner(&db.pool, "pg", "family", &AuditActor::cli(), Utc::now())
                .await
                .unwrap();
        })
        .await;
        export_across(&db.pool, actor, || async {
            catalogs::delete(&db.pool, "pg", true, &AuditActor::cli(), Utc::now())
                .await
                .unwrap();
        })
        .await;

        let entries = crate::store::audit::testing::entries(&db.pool, "catalog.exported").await;
        let scopes: Vec<(Vec<String>, Option<i64>)> = entries
            .iter()
            .map(|entry| (entry.teams.clone(), entry.details["catalog_id"].as_i64()))
            .collect();
        let family = vec!["family".to_string()];
        assert_eq!(
            scopes,
            [
                (vec![crate::db::TEST_USER.to_string()], Some(pg)),
                (family, Some(pg))
            ],
            "each export with the owner its snapshot saw, the deleted one included"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_export_whose_entry_fails_fails_and_records_nothing() {
        let db = test_db().await;
        catalogs::create(
            &db.pool,
            &catalogs::NewCatalog {
                name: "pg",
                description: "",
                owner_team: crate::db::TEST_TEAM,
            },
            &AuditActor::cli(),
            Utc::now(),
        )
        .await
        .unwrap();
        crate::http::api::testing::fail_on(&db.pool, "BEFORE INSERT ON audit_entries FOR EACH ROW")
            .await;

        let failed = export(
            &db.pool,
            "pg",
            Vec::new(),
            &ExportOptions {
                audit: &AuditActor::cli(),
                now: Utc::now(),
                expect_catalog_id: None,
            },
        )
        .await;

        crate::http::api::testing::allow(&db.pool, "audit_entries").await;
        assert!(
            matches!(failed, Err(OkfError::Database { .. })),
            "{failed:?}"
        );
        assert!(
            crate::store::audit::testing::entries(&db.pool, "catalog.exported")
                .await
                .is_empty()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_new_catalog_exports_its_starter_files_and_unknown_ones_fail() {
        let db = test_db().await;
        catalogs::create(
            &db.pool,
            &crate::store::catalogs::NewCatalog {
                name: "pg",
                description: "",
                owner_team: crate::db::TEST_TEAM,
            },
            &crate::store::audit::AuditActor::cli(),
            Utc::now(),
        )
        .await
        .unwrap();

        let (tarball, generation) = export(
            &db.pool,
            "pg",
            Vec::new(),
            &ExportOptions {
                audit: &AuditActor::cli(),
                now: Utc::now(),
                expect_catalog_id: None,
            },
        )
        .await
        .unwrap();
        let unknown = export(
            &db.pool,
            "nope",
            Vec::new(),
            &ExportOptions {
                audit: &AuditActor::cli(),
                now: Utc::now(),
                expect_catalog_id: None,
            },
        )
        .await;

        // A new catalog holds only its starter files.
        let exported = archive::read_tarball(tarball.as_slice(), u64::MAX).unwrap();
        let paths: Vec<&str> = exported.keys().map(String::as_str).collect();
        assert_eq!(paths, ["index.md", "log.md"]);
        assert_eq!(generation, 0);
        assert!(matches!(unknown, Err(OkfError::CatalogNotSelected { .. })));
    }
}
