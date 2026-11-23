//! A document's history: its rows in `revisions`, newest first.

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::error::OkfError;

/// One revision of a document (its content left out).
// `sqlx::FromRow` derives the code that builds this struct from a result
// row, matching columns to fields by name.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct RevisionRow {
    /// The revision number, 1 for the first.
    pub revision: i32,
    /// `create`, `update`, `delete` or `import`.
    pub op: String,
    /// Who made it.
    pub actor: String,
    /// The publish message (`import` for an import).
    pub message: String,
    /// When (server clock).
    pub created_at: DateTime<Utc>,
}

/// The newest `limit` revisions of one path.
///
/// # Arguments
/// * `pool` - the database.
/// * `catalog_id` - the catalog.
/// * `path` - catalog-relative path, as stored (no leading `/`).
/// * `limit` - at most this many rows.
///
/// # Returns
/// The rows, newest first; empty if the path never existed.
///
/// # Errors
/// [`OkfError::Database`].
#[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "history.revisions", catalog_id = catalog_id))]
pub async fn revisions(
    pool: &PgPool,
    catalog_id: i64,
    path: &str,
    limit: i64,
) -> Result<Vec<RevisionRow>, OkfError> {
    // The primary key (catalog_id, path, revision), read backwards, serves
    // both the filter and the order, so the query reads `limit` index entries.
    let rows = sqlx::query_as::<_, RevisionRow>(
        "SELECT revision, op, actor, message, created_at FROM revisions \
         WHERE catalog_id = $1 AND path = $2 ORDER BY revision DESC LIMIT $3",
    )
    .bind(catalog_id)
    .bind(path)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// One revision's content, for `revert`.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct RevisionContent {
    /// `create`, `update`, `delete` or `import`.
    pub op: String,
    /// The bytes; `None` for a `delete`.
    pub content: Option<Vec<u8>>,
}

/// One path's content at one revision.
///
/// # Arguments
/// * `pool` - the database.
/// * `catalog_id` - the catalog.
/// * `path` - catalog-relative path, as stored (no leading `/`).
/// * `revision` - the revision number.
///
/// # Returns
/// The revision's content, or `None` if the path has no such revision.
///
/// # Errors
/// [`OkfError::Database`].
#[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "history.content_at", catalog_id = catalog_id))]
pub async fn content_at(
    pool: &PgPool,
    catalog_id: i64,
    path: &str,
    revision: i32,
) -> Result<Option<RevisionContent>, OkfError> {
    // One primary-key lookup: (catalog_id, path, revision).
    let row = sqlx::query_as::<_, RevisionContent>(
        "SELECT op, content FROM revisions \
         WHERE catalog_id = $1 AND path = $2 AND revision = $3",
    )
    .bind(catalog_id)
    .bind(path)
    .bind(revision)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// The most revisions one page of [`recent`] holds.
pub const RECENT_MAX: u32 = 200;

/// One revision in a catalog's recent changes: [`RevisionRow`] with its path.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct RecentRow {
    /// Catalog-relative path, as stored (no leading `/`).
    pub path: String,
    /// The revision number of `path`, 1 for the first.
    pub revision: i32,
    /// `create`, `update`, `delete` or `import`.
    pub op: String,
    /// Who made it.
    pub actor: String,
    /// The publish message (`import` for an import).
    pub message: String,
    /// When (server clock).
    pub created_at: DateTime<Utc>,
}

/// Where a page of [`recent`] continues: the last row of the page before.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentCursor {
    /// That row's path, as stored.
    pub path: String,
    /// That row's revision.
    pub revision: i32,
}

/// One page of [`recent`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentPage {
    /// The rows, newest first.
    pub rows: Vec<RecentRow>,
    /// The last row's place when more rows follow it, else `None`.
    pub next: Option<RecentCursor>,
}

/// The cursor row's time; the primary key finds it.
const RECENT_CURSOR_SQL: &str =
    "SELECT created_at FROM revisions WHERE catalog_id = $1 AND path = $2 AND revision = $3";

/// The newest page of a catalog's revisions. The index
/// `revisions_catalog_recent` (migration 0020), read backwards, gives the
/// rows in this order, so the query stops after one page. A test plans it.
pub(crate) const RECENT_SQL: &str = "SELECT path, revision, op, actor, message, created_at \
     FROM revisions WHERE catalog_id = $1 \
     ORDER BY created_at DESC, path DESC, revision DESC LIMIT $2";

/// A later page: the rows after the cursor row `($2, $3, $4)`. Every column
/// is ordered descending, so the row comparison `<` continues exactly after
/// the cursor, and the same backward index scan starts there. A separate
/// statement rather than an `OR` on a NULL cursor, which would keep a
/// generic plan from using the comparison as an index bound.
pub(crate) const RECENT_AFTER_SQL: &str = "SELECT path, revision, op, actor, message, created_at \
     FROM revisions WHERE catalog_id = $1 AND (created_at, path, revision) < ($2, $3, $4) \
     ORDER BY created_at DESC, path DESC, revision DESC LIMIT $5";

/// A page of the revisions published in one catalog, across its paths,
/// newest first: every create, update, delete and import, each revision of
/// a path on its own row. Content is left out. Rows that share a time are
/// ordered by path, then revision (both descending), so the order is total
/// and a page continues exactly where the one before stopped.
///
/// # Arguments
/// * `pool` - the database.
/// * `catalog_id` - the catalog.
/// * `after` - the previous page's `next`, or `None` for the newest page.
/// * `limit` - the page size, 1..=[`RECENT_MAX`].
///
/// # Returns
/// The page, with `next` set when more rows follow it.
///
/// # Errors
/// [`OkfError::Refused`] for a `limit` outside 1..=[`RECENT_MAX`];
/// [`OkfError::NotFound`] for a cursor naming no revision of this catalog;
/// [`OkfError::Database`].
#[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "history.recent", catalog_id = catalog_id))]
pub async fn recent(
    pool: &PgPool,
    catalog_id: i64,
    after: Option<&RecentCursor>,
    limit: u32,
) -> Result<RecentPage, OkfError> {
    if !(1..=RECENT_MAX).contains(&limit) {
        return Err(OkfError::Refused {
            reason: format!("the page size must be 1..={RECENT_MAX}"),
        });
    }
    // Revisions are only ever added (a catalog's go only with the catalog),
    // so the cursor row read here is still there for the page query.
    let since = match after {
        Some(cursor) => Some(
            sqlx::query_scalar::<_, DateTime<Utc>>(RECENT_CURSOR_SQL)
                .bind(catalog_id)
                .bind(&cursor.path)
                .bind(cursor.revision)
                .fetch_optional(pool)
                .await?
                .ok_or_else(|| OkfError::NotFound {
                    path: format!("{} (revision {})", cursor.path, cursor.revision),
                })?,
        ),
        None => None,
    };
    // One row past the page says whether another page follows.
    let fetch = i64::from(limit) + 1;
    // `zip` pairs the cursor with its time: both are `Some` or both `None`.
    let mut rows = match after.zip(since) {
        Some((cursor, since)) => {
            sqlx::query_as::<_, RecentRow>(RECENT_AFTER_SQL)
                .bind(catalog_id)
                .bind(since)
                .bind(&cursor.path)
                .bind(cursor.revision)
                .bind(fetch)
                .fetch_all(pool)
                .await?
        }
        None => {
            sqlx::query_as::<_, RecentRow>(RECENT_SQL)
                .bind(catalog_id)
                .bind(fetch)
                .fetch_all(pool)
                .await?
        }
    };
    // `usize::try_from` cannot fail for a value up to 200; `unwrap_or`
    // keeps the code free of a panic path anyway.
    let next = if rows.len() > usize::try_from(limit).unwrap_or(usize::MAX) {
        rows.pop();
        rows.last().map(|row| RecentCursor {
            path: row.path.clone(),
            revision: row.revision,
        })
    } else {
        None
    };
    Ok(RecentPage { rows, next })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::files;
    use crate::db::test_db;
    use crate::store::catalogs;
    use crate::store::import::{import, ImportOptions, ImportScope};
    use chrono::Utc;

    #[tokio::test(flavor = "multi_thread")]
    async fn revisions_come_newest_first_and_outlive_a_delete() {
        let db = test_db().await;
        let catalog = catalogs::create(
            &db.pool,
            &crate::store::catalogs::NewCatalog {
                name: "pg",
                display_name: &crate::labels::DisplayName::default(),
                description: "",
                owner_team: crate::db::TEST_TEAM,
            },
            &crate::store::audit::AuditActor::cli(),
            Utc::now(),
        )
        .await
        .unwrap();
        for (version, replace) in [("one", false), ("two", true)] {
            let options = ImportOptions {
                replace,
                scope: ImportScope::Everything,
                expect_generation: None,
                expect_catalog_id: None,
                actor: "tester",
                audit: &crate::store::audit::AuditActor::cli(),
                now: Utc::now(),
            };
            let text = format!("# {version}\n");
            let entries = files(&[("a.md", &text), ("b.md", "# B\n")]);
            import(
                &crate::stores::CatalogDb::new(crate::stores::StoreName::main(), db.pool.clone()),
                "pg",
                entries,
                &options,
            )
            .await
            .unwrap();
        }
        let options = ImportOptions {
            replace: true,
            scope: ImportScope::Everything,
            expect_generation: None,
            expect_catalog_id: None,
            actor: "tester",
            audit: &crate::store::audit::AuditActor::cli(),
            now: Utc::now(),
        };
        import(
            &crate::stores::CatalogDb::new(crate::stores::StoreName::main(), db.pool.clone()),
            "pg",
            files(&[("b.md", "# B\n")]),
            &options,
        )
        .await
        .unwrap();

        let all = revisions(&db.pool, catalog.id, "a.md", 20).await.unwrap();
        let one = revisions(&db.pool, catalog.id, "a.md", 1).await.unwrap();
        let none = revisions(&db.pool, catalog.id, "c.md", 20).await.unwrap();

        let numbers: Vec<i32> = all.iter().map(|row| row.revision).collect();
        assert_eq!(numbers, [3, 2, 1]);
        // Import writes `import` for every added or changed file (M3,
        // `src/store/import.rs`) and `delete` for a file it drops, both
        // with the message `import`.
        assert_eq!(all[0].op, "delete");
        assert_eq!(all[1].op, "import");
        assert_eq!(all[0].message, "import");
        assert_eq!(all[2].actor, "tester");
        assert_eq!(one, all[..1]);
        assert!(none.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn content_at_reads_one_revision_or_its_deletion() {
        let db = test_db().await;
        let catalog = catalogs::create(
            &db.pool,
            &crate::store::catalogs::NewCatalog {
                name: "pg",
                display_name: &crate::labels::DisplayName::default(),
                description: "",
                owner_team: crate::db::TEST_TEAM,
            },
            &crate::store::audit::AuditActor::cli(),
            Utc::now(),
        )
        .await
        .unwrap();
        let rounds: [(&[(&str, &str)], bool); 2] = [
            (&[("a.md", "# One\n"), ("b.md", "# B\n")], false),
            (&[("b.md", "# B\n")], true),
        ];
        for (entries, replace) in rounds {
            let options = ImportOptions {
                replace,
                scope: ImportScope::Everything,
                expect_generation: None,
                expect_catalog_id: None,
                actor: "tester",
                audit: &crate::store::audit::AuditActor::cli(),
                now: Utc::now(),
            };
            import(
                &crate::stores::CatalogDb::new(crate::stores::StoreName::main(), db.pool.clone()),
                "pg",
                files(entries),
                &options,
            )
            .await
            .unwrap();
        }

        let first = content_at(&db.pool, catalog.id, "a.md", 1).await.unwrap();
        let deleted = content_at(&db.pool, catalog.id, "a.md", 2).await.unwrap();
        let missing = content_at(&db.pool, catalog.id, "a.md", 3).await.unwrap();

        let first = first.expect("revision 1");
        assert_eq!(
            (first.op.as_str(), first.content.as_deref()),
            ("import", Some(&b"# One\n"[..]))
        );
        let deleted = deleted.expect("revision 2");
        assert_eq!((deleted.op.as_str(), deleted.content), ("delete", None));
        assert!(missing.is_none());
    }

    /// Creates catalog `name` with no history, returning its id. Creating a
    /// catalog writes a first `index.md` and `log.md`; they are removed, so
    /// each test sees only the revisions it writes.
    async fn catalog(pool: &PgPool, name: &str) -> i64 {
        let id = catalogs::create(
            pool,
            &crate::store::catalogs::NewCatalog {
                name,
                display_name: &crate::labels::DisplayName::default(),
                description: "",
                owner_team: crate::db::TEST_TEAM,
            },
            &crate::store::audit::AuditActor::cli(),
            Utc::now(),
        )
        .await
        .unwrap()
        .id;
        sqlx::query("DELETE FROM revisions WHERE catalog_id = $1")
            .bind(id)
            .execute(pool)
            .await
            .unwrap();
        id
    }

    /// Writes one revision row as a publish or an import would.
    async fn write(
        pool: &PgPool,
        catalog_id: i64,
        (path, revision, op): (&str, i32, &str),
        actor: &str,
        at: DateTime<Utc>,
    ) {
        let content: Option<&[u8]> = (op != "delete").then_some(b"# Text\n");
        sqlx::query(
            "INSERT INTO revisions (catalog_id, path, revision, op, content, executable, \
             actor, message, created_at) VALUES ($1, $2, $3, $4, $5, false, $6, $7, $8)",
        )
        .bind(catalog_id)
        .bind(path)
        .bind(revision)
        .bind(op)
        .bind(content)
        .bind(actor)
        .bind(format!("{op} {path}"))
        .bind(at)
        .execute(pool)
        .await
        .unwrap();
    }

    /// A fixed instant, `seconds` after a base time.
    fn at(seconds: i64) -> DateTime<Utc> {
        use chrono::TimeZone;
        Utc.with_ymd_and_hms(2026, 11, 18, 12, 0, 0).unwrap() + chrono::TimeDelta::seconds(seconds)
    }

    /// Every page of `recent` at `limit`, followed to the end.
    async fn every_page(pool: &PgPool, catalog_id: i64, limit: u32) -> Vec<Vec<(String, i32)>> {
        let mut pages = Vec::new();
        let mut after = None;
        loop {
            let page = recent(pool, catalog_id, after.as_ref(), limit)
                .await
                .unwrap();
            pages.push(
                page.rows
                    .iter()
                    .map(|row| (row.path.clone(), row.revision))
                    .collect(),
            );
            match page.next {
                Some(next) => after = Some(next),
                None => return pages,
            }
            assert!(pages.len() < 100, "paging never ends");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn recent_lists_every_revision_of_one_catalog_newest_first() {
        let db = test_db().await;
        let pg = catalog(&db.pool, "pg").await;
        let other = catalog(&db.pool, "other").await;
        write(&db.pool, pg, ("a.md", 1, "import"), "importer", at(0)).await;
        write(&db.pool, pg, ("a.md", 2, "update"), "alice", at(10)).await;
        write(&db.pool, pg, ("b.md", 1, "create"), "bob", at(20)).await;
        write(&db.pool, pg, ("a.md", 3, "delete"), "bob", at(30)).await;
        write(&db.pool, other, ("secret.md", 1, "create"), "eve", at(40)).await;

        let page = recent(&db.pool, pg, None, 50).await.unwrap();

        let got: Vec<(&str, i32, &str, &str)> = page
            .rows
            .iter()
            .map(|row| {
                (
                    row.path.as_str(),
                    row.revision,
                    row.op.as_str(),
                    row.actor.as_str(),
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                ("a.md", 3, "delete", "bob"),
                ("b.md", 1, "create", "bob"),
                ("a.md", 2, "update", "alice"),
                ("a.md", 1, "import", "importer"),
            ],
            "every op, every revision of a.md, nothing from `other`"
        );
        assert_eq!(page.rows[0].message, "delete a.md");
        assert_eq!(page.rows[0].created_at, at(30));
        assert_eq!(page.next, None);
        let elsewhere = recent(&db.pool, other, None, 50).await.unwrap();
        assert_eq!(elsewhere.rows.len(), 1);
        let empty = catalog(&db.pool, "empty").await;
        assert_eq!(
            recent(&db.pool, empty, None, 50).await.unwrap(),
            RecentPage {
                rows: Vec::new(),
                next: None
            }
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn recent_pages_through_equal_times_without_repeating_or_skipping() {
        let db = test_db().await;
        let pg = catalog(&db.pool, "pg").await;
        // Eleven rows at one instant (an import of many files, or several
        // revisions of one path), between two rows at other times.
        write(&db.pool, pg, ("z.md", 1, "create"), "alice", at(0)).await;
        for path in ["c.md", "a.md", "b.md", "e/f.md", "d.md"] {
            write(&db.pool, pg, (path, 1, "import"), "importer", at(5)).await;
        }
        for revision in 2..=7 {
            write(&db.pool, pg, ("a.md", revision, "update"), "bob", at(5)).await;
        }
        write(&db.pool, pg, ("a.md", 8, "update"), "bob", at(9)).await;

        let whole: Vec<(String, i32)> = every_page(&db.pool, pg, RECENT_MAX).await.concat();
        let mut sizes = Vec::new();
        for limit in 1..=6 {
            let pages = every_page(&db.pool, pg, limit).await;
            sizes.push(pages.len());
            assert_eq!(pages.concat(), whole, "pages of {limit}");
        }

        let tied: Vec<(&str, i32)> = whole[1..12]
            .iter()
            .map(|(path, revision)| (path.as_str(), *revision))
            .collect();
        assert_eq!(whole.len(), 13);
        assert_eq!(whole[0], ("a.md".to_string(), 8));
        assert_eq!(
            tied,
            [
                ("e/f.md", 1),
                ("d.md", 1),
                ("c.md", 1),
                ("b.md", 1),
                ("a.md", 7),
                ("a.md", 6),
                ("a.md", 5),
                ("a.md", 4),
                ("a.md", 3),
                ("a.md", 2),
                ("a.md", 1),
            ],
            "ties by path, then revision, both descending"
        );
        assert_eq!(whole[12], ("z.md".to_string(), 1));
        // 13 rows: 13 pages of 1 ... 3 pages of 6 (the last page holds the
        // remainder, and an exact fit ends without an empty page).
        assert_eq!(sizes, [13, 7, 5, 4, 3, 3]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn recent_refuses_a_bad_page_size_and_an_unknown_cursor() {
        let db = test_db().await;
        let pg = catalog(&db.pool, "pg").await;
        let other = catalog(&db.pool, "other").await;
        write(&db.pool, pg, ("a.md", 1, "create"), "alice", at(0)).await;
        write(&db.pool, other, ("b.md", 1, "create"), "alice", at(0)).await;
        let foreign = RecentCursor {
            path: "b.md".into(),
            revision: 1,
        };
        let missing = RecentCursor {
            path: "a.md".into(),
            revision: 2,
        };

        let zero = recent(&db.pool, pg, None, 0).await;
        let over = recent(&db.pool, pg, None, RECENT_MAX + 1).await;
        let most = recent(&db.pool, pg, None, RECENT_MAX).await;
        let elsewhere = recent(&db.pool, pg, Some(&foreign), 10).await;
        let unknown = recent(&db.pool, pg, Some(&missing), 10).await;

        assert!(matches!(zero, Err(OkfError::Refused { .. })), "{zero:?}");
        assert!(matches!(over, Err(OkfError::Refused { .. })), "{over:?}");
        assert_eq!(most.unwrap().rows.len(), 1);
        assert!(
            matches!(elsewhere, Err(OkfError::NotFound { .. })),
            "another catalog's row is no cursor here: {elsewhere:?}"
        );
        assert!(
            matches!(unknown, Err(OkfError::NotFound { .. })),
            "{unknown:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn recent_reads_one_page_through_the_catalog_time_index() {
        let db = test_db().await;
        let pg = catalog(&db.pool, "pg").await;
        let other = catalog(&db.pool, "other").await;
        // A long history in two catalogs: a page must not read either whole.
        sqlx::query(
            "INSERT INTO revisions (catalog_id, path, revision, op, content, executable, \
             actor, message, created_at) \
             SELECT c, 'p' || (n % 500) || '.md', n / 500 + 1, 'update', NULL, false, 'a', 'm', \
             $3 + n * interval '1 second' FROM generate_series(0, 19999) AS n, \
             unnest(ARRAY[$1, $2]) AS c",
        )
        .bind(pg)
        .bind(other)
        .bind(at(0))
        .execute(&db.pool)
        .await
        .unwrap();
        sqlx::query("ANALYZE revisions")
            .execute(&db.pool)
            .await
            .unwrap();

        // The statements `recent` runs, planned for the newest page and for
        // a continued one.
        // `AssertSqlSafe`: the texts are constants of this module.
        let first: Vec<String> =
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("EXPLAIN {RECENT_SQL}")))
                .bind(pg)
                .bind(51_i64)
                .fetch_all(&db.pool)
                .await
                .unwrap();
        let later: Vec<String> =
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("EXPLAIN {RECENT_AFTER_SQL}")))
                .bind(pg)
                .bind(at(10_000))
                .bind("p0.md")
                .bind(21_i32)
                .bind(51_i64)
                .fetch_all(&db.pool)
                .await
                .unwrap();

        for plan in [first.join("\n"), later.join("\n")] {
            assert!(
                plan.contains("Index Scan Backward using revisions_catalog_recent"),
                "{plan}"
            );
            assert!(!plan.contains("Sort"), "the index gives the order: {plan}");
            assert!(!plan.contains("Seq Scan"), "{plan}");
        }
        let bound = later.iter().any(|line| {
            line.contains("Index Cond") && line.contains("ROW(created_at, path, revision)")
        });
        assert!(bound, "the cursor bounds the scan: {later:?}");
    }
}
