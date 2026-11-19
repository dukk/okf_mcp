//! The catalog directory (M18 spec Decisions 6 and 7, store side): catalog
//! rows by name, with the access facts of Decision 9, so a request can ask
//! "does this catalog exist, who owns it, is it suspended" without loading
//! its content.
//!
//! Two caches back it, because a present row and an absent name live for
//! different times and [`Bounded`] has one lifetime per cache:
//!
//! * `rows` keeps a row for `catalog_ttl` (10 s by default);
//! * `absent` remembers that a name has no catalog for [`ABSENT_TTL`] (5 s).
//!
//! Creating a catalog sends [`Invalidation::CatalogMeta`], which drops both
//! entries of its name. A publish or import sends [`Invalidation::Catalog`],
//! which drops a cached row that is behind the new generation and keeps
//! every load running at that moment out of the cache (the minimum
//! generation of Decision 11, by the cache's fence).
//!
//! # For Python readers
//!
//! The directory is shared by reference (`Arc<CatalogDirectory>` or `&`);
//! every method takes `&self`. `Option<Arc<CatalogRow>>` is "a row or
//! `None`", with the row shared between callers instead of copied.

use std::collections::HashSet;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use sqlx::PgPool;

use crate::cache::{Bounded, CacheName, Limit};
use crate::error::OkfError;
use crate::invalidate::Invalidation;
use crate::store::catalogs::{self, CatalogRow};
use crate::telemetry::metrics::Metrics;

/// How long a name with no catalog is remembered (spec Decision 6).
pub const ABSENT_TTL: Duration = Duration::from_secs(5);

/// What one shared read found, with the invalidation counter it started
/// under. Every caller that shares the read (single flight) judges the
/// answer by that counter, not by one it read itself later: a follower that
/// joined after a create was applied would otherwise take an overtaken
/// "no such catalog" for a current one.
#[derive(Debug, Clone)]
struct Loaded {
    row: Option<Arc<CatalogRow>>,
    seen: u64,
}

/// Catalog rows by name, with absent names remembered briefly.
pub struct CatalogDirectory {
    pool: PgPool,
    // `None` is a name with no catalog. It is dropped from here at once (see
    // `remember_absent`) and kept in `absent` instead, for the shorter time.
    rows: Bounded<String, Loaded>,
    // Names known to have no catalog. Not reported as `okf.cache.*`: its
    // requests are the row cache's misses, already counted there.
    absent: Bounded<String, ()>,
    // Moved by every `CatalogMeta` message, so a lookup that read "no such
    // catalog" before a create can tell it must not remember that answer.
    meta_changes: AtomicU64,
}

impl std::fmt::Debug for CatalogDirectory {
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(uninstrumented_fn, reason = "a Debug impl")
    )]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CatalogDirectory")
            .field("rows", &self.rows)
            .finish_non_exhaustive()
    }
}

impl CatalogDirectory {
    /// An empty directory.
    ///
    /// # Arguments
    /// * `pool` - the database.
    /// * `entries` - the most rows (and, separately, absent names) kept
    ///   (`catalog_directory_entries`).
    /// * `ttl` - how long a row is served (`catalog_ttl`).
    /// * `metrics` - the instruments `okf.cache.*` is recorded on.
    ///
    /// # Returns
    /// The directory.
    #[tracing::instrument(level = "trace", skip_all)]
    pub fn new(pool: PgPool, entries: u64, ttl: Duration, metrics: Metrics) -> Self {
        Self::with_absent_ttl(pool, entries, ttl, ABSENT_TTL, metrics)
    }

    /// [`CatalogDirectory::new`] with the lifetime of an absent name given:
    /// the seam a test uses to see one expire without waiting 5 s.
    #[tracing::instrument(level = "trace", skip_all)]
    fn with_absent_ttl(
        pool: PgPool,
        entries: u64,
        ttl: Duration,
        absent_ttl: Duration,
        metrics: Metrics,
    ) -> Self {
        Self {
            pool,
            rows: Bounded::new(CacheName::Directory, Limit::Entries(entries), ttl)
                .with_metrics(metrics),
            absent: Bounded::new(CacheName::Directory, Limit::Entries(entries), absent_ttl),
            meta_changes: AtomicU64::new(0),
        }
    }

    /// The row of catalog `name`, read from the database once however many
    /// callers ask together and then served until it expires or is
    /// invalidated.
    ///
    /// # Arguments
    /// * `name` - the catalog's name; a malformed one is the same as an
    ///   unknown one, and reaches neither the cache nor the database.
    ///
    /// # Returns
    /// The row, or `None` if there is no such catalog.
    ///
    /// # Errors
    /// [`OkfError::Database`] if the read fails; [`OkfError::Busy`] for a
    /// caller that shared another caller's failed read (retry).
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn resolve(&self, name: &str) -> Result<Option<Arc<CatalogRow>>, OkfError> {
        self.resolve_by(name, || async {
            Ok(catalogs::row_by_name(&self.pool, name).await?.map(Arc::new))
        })
        .await
    }

    /// [`CatalogDirectory::resolve`] with the read given, so a test can hold
    /// a read open while it sends an invalidation.
    #[tracing::instrument(level = "trace", skip_all)]
    async fn resolve_by<F, Fut>(
        &self,
        name: &str,
        load: F,
    ) -> Result<Option<Arc<CatalogRow>>, OkfError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Option<Arc<CatalogRow>>, OkfError>>,
    {
        if !catalogs::is_valid_name(name) {
            return Ok(None);
        }
        let key = name.to_owned();
        if self.absent.peek(&key).await.is_some() {
            return Ok(None);
        }
        let loaded = self
            .rows
            .get_or_load(key.clone(), || self.read(load))
            .await?;
        if loaded.row.is_none() {
            self.remember_absent(key, loaded.seen).await;
        }
        Ok(loaded.row)
    }

    /// One read of the database, stamped with the counter as it was before
    /// the read started: a create that lands after moves the counter, so
    /// every caller sharing this result sees the answer was overtaken.
    #[tracing::instrument(level = "trace", skip_all)]
    async fn read<F, Fut>(&self, load: F) -> Result<Loaded, OkfError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Option<Arc<CatalogRow>>, OkfError>>,
    {
        let seen = self.meta_changes.load(Ordering::SeqCst);
        let row = load().await?;
        Ok(Loaded { row, seen })
    }

    /// Remembers that `key` has no catalog, unless a `CatalogMeta` message
    /// arrived since `seen` (a create may have committed after the read that
    /// found nothing).
    #[tracing::instrument(level = "trace", skip_all)]
    async fn remember_absent(&self, key: String, seen: u64) {
        // The row cache does not keep an absence: the shorter-lived cache does.
        self.rows.invalidate(&key).await;
        if self
            .absent
            .get_or_load(key.clone(), || async { Ok(()) })
            .await
            .is_err()
        {
            return;
        }
        // The message bumps the counter before it drops the entry, so either
        // it drops this entry or this check sees its bump.
        if self.meta_changes.load(Ordering::SeqCst) != seen {
            self.absent.invalidate(&key).await;
        }
    }

    /// The rows of `names`: those cached are served from the cache, the rest
    /// are read together in one query. The rows read are not cached, so a
    /// listing never adds entries (it cannot evict what requests use). A row
    /// already cached that the listing visits is counted as recently used
    /// (moka has no read that does not); that is bounded by the page size and
    /// is acceptable, since a catalog a principal can list is one it uses.
    ///
    /// # Arguments
    /// * `names` - catalog names; duplicates, malformed names and names with
    ///   no catalog add nothing.
    ///
    /// # Returns
    /// The rows found, ordered by name.
    ///
    /// # Errors
    /// [`OkfError::Database`] if the read fails.
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn resolve_many(&self, names: &[String]) -> Result<Vec<Arc<CatalogRow>>, OkfError> {
        let mut seen: HashSet<&str> = HashSet::with_capacity(names.len());
        let mut found: Vec<Arc<CatalogRow>> = Vec::new();
        let mut missing: Vec<String> = Vec::new();
        for name in names {
            if !seen.insert(name.as_str()) {
                continue;
            }
            match self.rows.peek_fresh(name).await {
                Some(Loaded { row: Some(row), .. }) => found.push(row),
                // Cached as no catalog (only ever briefly) is not trusted
                // here: the query below is the authority for a bulk read.
                Some(Loaded { row: None, .. }) | None => missing.push(name.clone()),
            }
        }
        if !missing.is_empty() {
            let rows = catalogs::rows_by_names(&self.pool, &missing).await?;
            found.extend(rows.into_iter().map(Arc::new));
        }
        found.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(found)
    }

    /// Applies a change-feed message. Only [`Invalidation::CatalogMeta`] and
    /// [`Invalidation::Catalog`] touch the directory.
    ///
    /// # Arguments
    /// * `invalidation` - what changed.
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn apply(&self, invalidation: &Invalidation) {
        match invalidation {
            Invalidation::CatalogMeta { name, .. } => {
                // Counter first (see `remember_absent`). The row is keyed by
                // name and the message names it: no scan by id is needed.
                self.meta_changes.fetch_add(1, Ordering::SeqCst);
                self.rows.invalidate(name).await;
                self.absent.invalidate(name).await;
            }
            Invalidation::Catalog { id, generation } => {
                // Drops a row behind `generation`, and (the cache's fence)
                // keeps every read running now from being cached: one that
                // read an older generation must not outlive this message.
                // A read that starts later reads after the writer's commit.
                // The scan is bounded by the cache's size.
                self.rows
                    .invalidate_where(|_, row| {
                        row.row
                            .as_ref()
                            .is_some_and(|row| row.id == *id && row.generation < *generation)
                    })
                    .await;
            }
            Invalidation::Credential { .. }
            | Invalidation::User { .. }
            | Invalidation::Team { .. }
            | Invalidation::Settings => {}
        }
    }

    /// After the change feed's connection was lost, messages were missed:
    /// every row is reread on its next use, and no absence is trusted.
    #[tracing::instrument(level = "debug", skip_all)]
    pub async fn mark_all_stale(&self) {
        self.meta_changes.fetch_add(1, Ordering::SeqCst);
        self.rows.mark_all_stale();
        self.absent.invalidate_where(|_, _| true).await;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use chrono::Utc;
    use tokio::sync::oneshot;
    use tokio::time::timeout;

    use super::*;
    use crate::db::{test_db, TEST_TEAM};
    use crate::labels::DisplayName;
    use crate::mcp::testing::{local_metrics, render};
    use crate::store::audit::AuditActor;
    use crate::store::catalogs::{create, NewCatalog};

    /// Every wait in these tests is bounded by this.
    const BOUND: Duration = Duration::from_secs(30);
    const LONG: Duration = Duration::from_secs(3600);

    fn directory(pool: &PgPool) -> CatalogDirectory {
        CatalogDirectory::new(pool.clone(), 100, LONG, Metrics::global())
    }

    async fn seed(pool: &PgPool, name: &str) -> i64 {
        let new = NewCatalog {
            name,
            display_name: &DisplayName::default(),
            description: "first",
            owner_team: TEST_TEAM,
        };
        create(pool, &new, &AuditActor::cli(), Utc::now())
            .await
            .unwrap()
            .id
    }

    async fn set_description(pool: &PgPool, name: &str, text: &str) {
        sqlx::query("UPDATE catalogs SET description = $2 WHERE name = $1")
            .bind(name)
            .bind(text)
            .execute(pool)
            .await
            .unwrap();
    }

    fn row(id: i64, generation: i64) -> Option<Arc<CatalogRow>> {
        Some(Arc::new(CatalogRow {
            id,
            name: "pg".to_owned(),
            display_name: String::new(),
            description: String::new(),
            generation,
            owner_team: None,
            owner_team_id: None,
            suspended: None,
            maintenance_off: false,
            audit_team: None,
        }))
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn resolve_reads_once_then_serves_from_cache() {
        let db = test_db().await;
        seed(&db.pool, "pg").await;
        let (_provider, metrics, registry) = local_metrics();
        let directory = CatalogDirectory::new(db.pool.clone(), 100, LONG, metrics);

        let first = directory.resolve("pg").await.unwrap().unwrap();
        // Changed behind the directory's back: only a read would see it.
        set_description(&db.pool, "pg", "second").await;
        let second = directory.resolve("pg").await.unwrap().unwrap();

        assert_eq!(first.description, "first");
        assert_eq!(second.description, "first", "served from the cache");
        let text = render(&registry);
        assert!(
            text.contains(r#"okf_cache_requests_total{cache="directory",result="miss"} 1"#),
            "one read in\n{text}"
        );
        assert!(
            text.contains(r#"okf_cache_requests_total{cache="directory",result="hit"} 1"#),
            "one hit in\n{text}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_unknown_name_is_remembered_briefly_and_forgotten_by_create() {
        let db = test_db().await;
        let directory = directory(&db.pool);
        assert!(directory.resolve("x").await.unwrap().is_none());

        // Created behind the directory's back: the absence is remembered.
        let id = seed(&db.pool, "x").await;
        assert!(directory.resolve("x").await.unwrap().is_none());

        // What `create` sends once Task 2 lands: the message names the catalog.
        directory
            .apply(&Invalidation::CatalogMeta {
                id,
                name: "x".to_owned(),
            })
            .await;
        let found = directory.resolve("x").await.unwrap().expect("now found");
        assert_eq!(found.id, id);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_absent_name_is_forgotten_after_its_lifetime() {
        let db = test_db().await;
        let directory = CatalogDirectory::with_absent_ttl(
            db.pool.clone(),
            100,
            LONG,
            Duration::from_millis(100),
            Metrics::global(),
        );
        assert!(directory.resolve("x").await.unwrap().is_none());
        seed(&db.pool, "x").await;
        assert!(directory.resolve("x").await.unwrap().is_none());

        // moka reads the real clock, so this waits in real time.
        tokio::time::sleep(Duration::from_millis(300)).await;

        assert!(directory.resolve("x").await.unwrap().is_some());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_create_that_lands_while_the_absence_is_read_is_not_remembered() {
        let db = test_db().await;
        let directory = Arc::new(directory(&db.pool));
        let (release, released) = oneshot::channel::<()>();
        let (reading, started) = oneshot::channel::<()>();
        let task = {
            let directory = Arc::clone(&directory);
            tokio::spawn(async move {
                directory
                    .resolve_by("x", || async {
                        reading.send(()).unwrap();
                        released.await.unwrap();
                        // What the read saw before the create committed.
                        Ok(None)
                    })
                    .await
            })
        };
        timeout(BOUND, started)
            .await
            .expect("the read started")
            .unwrap();
        let id = seed(&db.pool, "x").await;
        directory
            .apply(&Invalidation::CatalogMeta {
                id,
                name: "x".to_owned(),
            })
            .await;
        release.send(()).unwrap();
        let answered = timeout(BOUND, task).await.expect("finished").unwrap();
        assert!(answered.unwrap().is_none(), "the caller saw the old answer");

        let found = directory.resolve("x").await.unwrap();

        assert!(found.is_some(), "the old answer was not remembered");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_reader_that_joins_an_overtaken_read_does_not_remember_its_absence() {
        let db = test_db().await;
        let directory = Arc::new(directory(&db.pool));
        // A: the leader of a shared read that will find nothing. It parks
        // after it has stamped the counter, and runs no cleanup of its own
        // (it is a bare cache call), so only B's judgement is under test.
        let (release, released) = oneshot::channel::<()>();
        let (reading, started) = oneshot::channel::<()>();
        let leader = {
            let directory = Arc::clone(&directory);
            tokio::spawn(async move {
                directory
                    .rows
                    .get_or_load("x".to_owned(), || {
                        directory.read(|| async {
                            reading.send(()).unwrap();
                            released.await.unwrap();
                            Ok(None)
                        })
                    })
                    .await
            })
        };
        timeout(BOUND, started)
            .await
            .expect("the read started")
            .unwrap();
        // The create commits and its message is applied in full.
        let id = seed(&db.pool, "x").await;
        directory
            .apply(&Invalidation::CatalogMeta {
                id,
                name: "x".to_owned(),
            })
            .await;
        // B starts now, after the apply, and joins A's read in flight.
        let follower = {
            let directory = Arc::clone(&directory);
            tokio::spawn(async move { directory.resolve("x").await })
        };
        // B has reached the shared read once the cache counts it as a waiter;
        // there is no signal for that, so give it a moment to join.
        tokio::time::sleep(Duration::from_millis(200)).await;
        release.send(()).unwrap();
        timeout(BOUND, leader)
            .await
            .expect("A finished")
            .unwrap()
            .unwrap();
        let answered = timeout(BOUND, follower).await.expect("B finished").unwrap();
        assert!(answered.unwrap().is_none(), "B shared the old answer");

        // The old answer was not remembered: the next resolve finds it.
        assert!(directory.absent.peek(&"x".to_owned()).await.is_none());
        assert!(directory.resolve("x").await.unwrap().is_some());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_catalog_meta_invalidation_drops_the_row() {
        let db = test_db().await;
        let id = seed(&db.pool, "pg").await;
        let directory = directory(&db.pool);
        directory.resolve("pg").await.unwrap().unwrap();
        set_description(&db.pool, "pg", "second").await;

        directory
            .apply(&Invalidation::CatalogMeta {
                id,
                name: "pg".to_owned(),
            })
            .await;

        let row = directory.resolve("pg").await.unwrap().unwrap();
        assert_eq!(row.description, "second");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_deleted_catalog_is_unknown_after_its_meta_message() {
        let db = test_db().await;
        let id = seed(&db.pool, "pg").await;
        let directory = directory(&db.pool);
        directory.resolve("pg").await.unwrap().unwrap();
        sqlx::query("DELETE FROM catalogs WHERE id = $1")
            .bind(id)
            .execute(&db.pool)
            .await
            .unwrap();

        directory
            .apply(&Invalidation::CatalogMeta {
                id,
                name: "pg".to_owned(),
            })
            .await;

        assert!(directory.resolve("pg").await.unwrap().is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_catalog_invalidation_raises_the_minimum_generation() {
        let db = test_db().await;
        let directory = Arc::new(directory(&db.pool));

        // A read that saw generation 8 is running when generation 9 is sent.
        let (release, released) = oneshot::channel::<()>();
        let (reading, started) = oneshot::channel::<()>();
        let task = {
            let directory = Arc::clone(&directory);
            tokio::spawn(async move {
                directory
                    .resolve_by("pg", || async {
                        reading.send(()).unwrap();
                        released.await.unwrap();
                        Ok(row(1, 8))
                    })
                    .await
            })
        };
        timeout(BOUND, started)
            .await
            .expect("the read started")
            .unwrap();
        directory
            .apply(&Invalidation::Catalog {
                id: 1,
                generation: 9,
            })
            .await;
        release.send(()).unwrap();
        let served = timeout(BOUND, task).await.expect("finished").unwrap();

        // The caller got what it read, but the older row was not kept.
        assert_eq!(served.unwrap().unwrap().generation, 8);
        assert!(directory.rows.peek(&"pg".to_owned()).await.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_catalog_invalidation_drops_only_a_row_that_is_behind() {
        let db = test_db().await;
        let directory = directory(&db.pool);
        for (name, id, generation) in [("a", 1, 8), ("b", 2, 8), ("c", 3, 9)] {
            directory
                .resolve_by(name, || async { Ok(row(id, generation)) })
                .await
                .unwrap();
        }

        directory
            .apply(&Invalidation::Catalog {
                id: 1,
                generation: 9,
            })
            .await;
        // The same message for a row already at that generation changes nothing.
        directory
            .apply(&Invalidation::Catalog {
                id: 3,
                generation: 9,
            })
            .await;

        assert!(directory.rows.peek(&"a".to_owned()).await.is_none());
        assert!(directory.rows.peek(&"b".to_owned()).await.is_some());
        assert!(directory.rows.peek(&"c".to_owned()).await.is_some());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn marking_stale_rereads_rows_and_forgets_absences() {
        let db = test_db().await;
        seed(&db.pool, "pg").await;
        let directory = directory(&db.pool);
        directory.resolve("pg").await.unwrap().unwrap();
        assert!(directory.resolve("x").await.unwrap().is_none());
        set_description(&db.pool, "pg", "second").await;
        seed(&db.pool, "x").await;

        directory.mark_all_stale().await;

        let row = directory.resolve("pg").await.unwrap().unwrap();
        assert_eq!(row.description, "second");
        assert!(directory.resolve("x").await.unwrap().is_some());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn resolve_many_serves_cached_rows_and_reads_the_rest_together() {
        let db = test_db().await;
        for name in ["a", "b", "c"] {
            seed(&db.pool, name).await;
        }
        let directory = directory(&db.pool);
        directory.resolve("a").await.unwrap().unwrap();
        // Changed behind the directory's back: `a` must come from the cache.
        set_description(&db.pool, "a", "second").await;
        set_description(&db.pool, "b", "second").await;
        let ask: Vec<String> = ["c", "a", "nope", "b", "a", "b\0"]
            .iter()
            .map(|name| (*name).to_owned())
            .collect();

        let rows = directory.resolve_many(&ask).await.unwrap();

        let seen: Vec<(&str, &str)> = rows
            .iter()
            .map(|row| (row.name.as_str(), row.description.as_str()))
            .collect();
        assert_eq!(seen, [("a", "first"), ("b", "second"), ("c", "first")]);
        // A bulk read fills nothing: `c` is still not cached.
        assert!(directory.rows.peek(&"c".to_owned()).await.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_malformed_name_is_unknown_and_never_cached_or_read() {
        let db = test_db().await;
        let directory = directory(&db.pool);
        let reads = AtomicUsize::new(0);

        let found = directory
            .resolve_by("p\0g", || async {
                reads.fetch_add(1, Ordering::SeqCst);
                Ok(None)
            })
            .await
            .unwrap();

        assert!(found.is_none());
        assert_eq!(reads.load(Ordering::SeqCst), 0);
        assert!(directory.resolve("P G").await.unwrap().is_none());
    }
}
