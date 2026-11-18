//! Test support for the tools: an [`App`] over a throwaway database holding
//! catalogs, and principals and contexts to call tools with.

use std::sync::Arc;

use chrono::{DateTime, Utc};

use crate::app::App;
// Re-exported so tool tests keep calling `testing::files`; it lives in
// `archive`, beside `Files`, so store tests need not reach up into `mcp`.
pub(crate) use crate::archive::files;
use crate::auth::{CredentialKind, Principal, Reach};
use crate::clock::{Clock, SystemClock};
use crate::config::Config;
use crate::db::pg_support::TestDb;
use crate::db::redis_support::TestRedis;
use crate::mcp::context::ToolContext;
use crate::store::audit::AuditActor;
use crate::store::catalogs;
use crate::store::changesets::{self, ChangesetRow, EntryRow};
use crate::store::import::{import, ImportOptions, ImportScope};
use crate::store::keys::Access;
use crate::telemetry::metrics::Metrics;

/// An app and the database under it. Fields drop in declaration order: the
/// app (and its pool) before the container.
pub(crate) struct TestApp {
    /// The shared state, as a server would hold it.
    pub(crate) app: Arc<App>,
    // Keeps a test's local meter provider alive (dropping it shuts it down).
    _meter: Option<opentelemetry_sdk::metrics::SdkMeterProvider>,
    _db: TestDb,
}

/// Creates each catalog, imports its files now, and loads the app with the
/// system clock.
///
/// # Arguments
/// * `catalogs_and_files` - `(catalog name, its files)` pairs, in creation order.
/// * `config` - the server's settings.
///
/// # Returns
/// The loaded app and its database.
///
/// # Panics
/// As [`app_at`].
pub(crate) async fn app_with(
    catalogs_and_files: &[(&str, &[(&str, &str)])],
    config: Config,
) -> TestApp {
    app_at(
        catalogs_and_files,
        config,
        Utc::now(),
        Arc::new(SystemClock),
    )
    .await
}

/// Creates each catalog, imports its files at `imported`, and loads the app
/// with `clock`.
///
/// # Arguments
/// * `catalogs_and_files` - `(catalog name, its files)` pairs, in creation order.
/// * `config` - the server's settings.
/// * `imported` - the creation and import time (each document's revision time).
/// * `clock` - the server's clock.
///
/// # Returns
/// The loaded app and its database.
///
/// # Panics
/// If the database, a create or an import fails: a broken test setup.
pub(crate) async fn app_at(
    catalogs_and_files: &[(&str, &[(&str, &str)])],
    config: Config,
    imported: DateTime<Utc>,
    clock: Arc<dyn Clock>,
) -> TestApp {
    let db = seed(catalogs_and_files, imported).await;
    load(db, config, clock).await
}

/// Starts a database, creates each catalog and imports its files at
/// `imported`.
///
/// # Panics
/// If the database, a create or an import fails: a broken test setup.
async fn seed(catalogs_and_files: &[(&str, &[(&str, &str)])], imported: DateTime<Utc>) -> TestDb {
    let db = crate::db::test_db().await;
    for (name, entries) in catalogs_and_files {
        create_catalog(&db, name, imported).await;
        let options = ImportOptions {
            replace: false,
            scope: ImportScope::Everything,
            expect_generation: None,
            expect_catalog_id: None,
            actor: "tester",
            audit: &AuditActor::cli(),
            now: imported,
        };
        import(&db.pool, name, files(entries), &options)
            .await
            .expect("import the test files");
    }
    db
}

/// Creates each catalog without importing anything, so each holds only its
/// starter files (`index.md` and `log.md`), as a catalog created through
/// `POST /catalogs` does; loads the app with the system clock.
///
/// # Arguments
/// * `names` - the catalogs, in creation order.
/// * `config` - the server's settings.
///
/// # Returns
/// The loaded app and its database.
///
/// # Panics
/// If the database, a create or the load fails: a broken test setup.
pub(crate) async fn app_with_new(names: &[&str], config: Config) -> TestApp {
    let db = crate::db::test_db().await;
    for name in names {
        create_catalog(&db, name, Utc::now()).await;
    }
    load(db, config, Arc::new(SystemClock)).await
}

/// [`app_with`] with metric instruments built on a local meter provider
/// whose only reader is Prometheus, so a test can assert what a real `App`
/// recorded (Tasks 3-6 use it).
///
/// # Arguments
/// * `catalogs_and_files` - `(catalog name, its files)` pairs, in creation order.
/// * `config` - the server's settings.
///
/// # Returns
/// The loaded app, and the registry its instruments render into; read it
/// with [`render`].
///
/// # Panics
/// As [`app_at`].
pub(crate) async fn app_with_metrics(
    catalogs_and_files: &[(&str, &[(&str, &str)])],
    config: Config,
) -> (TestApp, prometheus::Registry) {
    let (provider, metrics, registry) = local_metrics();
    let db = seed(catalogs_and_files, Utc::now()).await;
    let mut config = config;
    config.mail.worker = false;
    let app = App::load_with_metrics(db.pool.clone(), config, Arc::new(SystemClock), metrics)
        .await
        .expect("load the app");
    (
        TestApp {
            app,
            _meter: Some(provider),
            _db: db,
        },
        registry,
    )
}

/// The Prometheus text a scrape of `registry` would return.
///
/// # Panics
/// If the encoder fails: a broken test setup.
pub(crate) fn render(registry: &prometheus::Registry) -> String {
    prometheus::TextEncoder::new()
        .encode_to_string(&registry.gather())
        .expect("encode the metrics")
}

/// The instruments on a local meter provider that renders into its own
/// registry, shaped as `serve` builds them. Keep the provider alive: dropping
/// it shuts the instruments down.
///
/// # Panics
/// If the Prometheus reader cannot be built: a broken test setup.
pub(crate) fn local_metrics() -> (
    opentelemetry_sdk::metrics::SdkMeterProvider,
    Metrics,
    prometheus::Registry,
) {
    let registry = prometheus::Registry::new();
    let reader = opentelemetry_prometheus::exporter()
        .with_registry(registry.clone())
        .without_target_info()
        .scope_info_enabled(false)
        .build()
        .expect("the Prometheus reader");
    let provider = opentelemetry_sdk::metrics::SdkMeterProvider::builder()
        .with_reader(reader)
        .build();
    let meter = opentelemetry::metrics::MeterProvider::meter(&provider, "okf-mcp");
    let metrics = Metrics::new(&meter);
    crate::telemetry::metrics::observe_export_failures(
        &meter,
        Arc::new(crate::telemetry::otlp::ExportFailures::default()),
    );
    (provider, metrics, registry)
}

/// The value of one rendered series, as [`render`] prints it.
///
/// # Arguments
/// * `rendered` - the exposition text.
/// * `series` - the series exactly as rendered, labels included, in
///   Prometheus's sorted label order (`okf_auth_attempts_total{method="password",outcome="ok"}`).
///
/// # Returns
/// The series' value, or `None` when it is not there.
pub(crate) fn sample_of(rendered: &str, series: &str) -> Option<f64> {
    rendered
        .lines()
        .find_map(|line| line.strip_prefix(series)?.strip_prefix(' ')?.parse().ok())
}

/// Creates catalog `name`, owned by the fixture team, at `at`.
async fn create_catalog(db: &TestDb, name: &str, at: DateTime<Utc>) {
    let new = catalogs::NewCatalog {
        name,
        display_name: &crate::labels::DisplayName::default(),
        description: "",
        owner_team: crate::db::TEST_TEAM,
    };
    catalogs::create(&db.pool, &new, &AuditActor::cli(), at)
        .await
        .expect("create the test catalog");
}

/// Two apps over one database and one Redis, as two replicas: `app_with`'s
/// catalogs, `config` with `[redis]` and `poll_interval = 1h` (so a test
/// that passes was carried by Redis, not by a poll).
///
/// # Arguments
/// * `catalogs_and_files` - `(catalog name, its files)` pairs, in creation order.
/// * `config` - the server's settings (its `[redis]` and `poll_interval` are
///   replaced).
///
/// # Returns
/// The first app (it owns the database), the second app (its own pool on the
/// same database) and the Redis both share.
///
/// # Panics
/// If the database, Redis or a load fails: a broken test setup.
pub(crate) async fn replica_pair(
    catalogs_and_files: &[(&str, &[(&str, &str)])],
    config: Config,
) -> (TestApp, Arc<App>, TestRedis) {
    replica_pair_on(catalogs_and_files, config, TestRedis::start().await).await
}

/// [`replica_pair`] over a Redis the test already started (a cluster, for
/// one).
///
/// # Arguments
/// * `catalogs_and_files` - `(catalog name, its files)` pairs, in creation order.
/// * `config` - the server's settings (its `[redis]` and `poll_interval` are
///   replaced).
/// * `redis` - the server both apps connect to.
///
/// # Returns
/// As [`replica_pair`].
///
/// # Panics
/// If the database, Redis or a load fails: a broken test setup.
pub(crate) async fn replica_pair_on(
    catalogs_and_files: &[(&str, &[(&str, &str)])],
    config: Config,
    redis: TestRedis,
) -> (TestApp, Arc<App>, TestRedis) {
    let mut config = redis_config(config, &redis);
    config.poll_interval = std::time::Duration::from_secs(3600);
    let db = seed(catalogs_and_files, Utc::now()).await;
    let first = load_shared(&db, config.clone(), &redis).await;
    let second_pool = crate::db::connect(&db.url, config.database_max_connections)
        .await
        .expect("a second pool on the test database");
    let second = App::load_with_redis(
        second_pool,
        config,
        Arc::new(SystemClock),
        Metrics::global(),
        Some(crate::shared::testing::connect(&redis).await),
    )
    .await
    .expect("load the second app");
    (
        TestApp {
            app: first,
            _meter: None,
            _db: db,
        },
        second,
        redis,
    )
}

/// One app with Redis: [`app_with`] plus `[redis]`.
///
/// # Arguments
/// * `config` - the server's settings (its `[redis]` is replaced).
///
/// # Returns
/// The app and its Redis, which the test holds (or stops).
///
/// # Panics
/// If the database, Redis or the load fails: a broken test setup.
pub(crate) async fn app_with_redis(config: Config) -> (TestApp, TestRedis) {
    app_on_redis(config, TestRedis::start().await).await
}

/// [`app_with_redis`] over a Redis the test already started (a cluster, for
/// one).
///
/// # Arguments
/// * `config` - the server's settings (its `[redis]` is replaced).
/// * `redis` - the server the app connects to.
///
/// # Returns
/// The app and `redis`.
///
/// # Panics
/// If the database or the load fails: a broken test setup.
pub(crate) async fn app_on_redis(config: Config, redis: TestRedis) -> (TestApp, TestRedis) {
    app_on_redis_with(config, redis, &Metrics::global()).await
}

/// [`app_on_redis`] with the Redis handle counting on `metrics` (a test's
/// [`local_metrics`]), so a test can read `okf.redis.errors`.
///
/// # Arguments
/// * `config` - the server's settings (its `[redis]` is replaced).
/// * `redis` - the server the app connects to.
/// * `metrics` - where the app's Redis handle counts its errors.
///
/// # Returns
/// The app and `redis`.
///
/// # Panics
/// If the database, Redis or the load fails: a broken test setup.
pub(crate) async fn app_on_redis_with(
    config: Config,
    redis: TestRedis,
    metrics: &Metrics,
) -> (TestApp, TestRedis) {
    let config = redis_config(config, &redis);
    let db = seed(&[], Utc::now()).await;
    let handle = crate::shared::Redis::connect(
        config.redis.as_ref().expect("[redis] was just set"),
        &redis.env(),
        metrics,
    )
    .await
    .expect("connect to the test redis");
    let app = App::load_with_redis(
        db.pool.clone(),
        config,
        Arc::new(SystemClock),
        Metrics::global(),
        Some(handle),
    )
    .await
    .expect("load the app");
    (
        TestApp {
            app,
            _meter: None,
            _db: db,
        },
        redis,
    )
}

/// `config` with `[redis]` pointing at `redis` and no mail worker.
fn redis_config(mut config: Config, redis: &TestRedis) -> Config {
    config.redis = Some(crate::shared::testing::config(redis));
    config.mail.worker = false;
    config
}

/// Loads an app over `db` holding a fresh handle on `redis`.
async fn load_shared(db: &TestDb, config: Config, redis: &TestRedis) -> Arc<App> {
    App::load_with_redis(
        db.pool.clone(),
        config,
        Arc::new(SystemClock),
        Metrics::global(),
        Some(crate::shared::testing::connect(redis).await),
    )
    .await
    .expect("load the app")
}

/// Loads the app over `db` with `clock`.
async fn load(db: TestDb, config: Config, clock: Arc<dyn Clock>) -> TestApp {
    // Test apps only enqueue (Decision 19); the investigator test serves one.
    let mut config = config;
    config.mail.worker = false;
    let app = App::load(db.pool.clone(), config, clock)
        .await
        .expect("load the app");
    TestApp {
        app,
        _meter: None,
        _db: db,
    }
}

/// A key with this id and access, reaching `reach` (every catalog when
/// empty), with a default.
///
/// # Arguments
/// * `key_id` - its id (also its actor).
/// * `access` - its access level.
/// * `reach` - the catalogs it is granted; empty for every catalog.
/// * `default` - its default catalog.
///
/// # Returns
/// The principal, as the auth layer would put it in a request.
pub(crate) fn principal_as(
    key_id: &str,
    access: Access,
    reach: &[&str],
    default: Option<&str>,
) -> Arc<Principal> {
    principal_with_reporting(key_id, access, reach, default, false)
}

/// Builds a test principal, explicitly opting into maintenance reporting.
pub(crate) fn principal_with_reporting(
    key_id: &str,
    access: Access,
    reach: &[&str],
    default: Option<&str>,
    maintenance_report: bool,
) -> Arc<Principal> {
    Arc::new(Principal {
        key_id: key_id.to_string(),
        access,
        maintenance_report,
        provisioner: false,
        reach: if reach.is_empty() {
            Reach::All
        } else {
            // Each catalog at the principal's own `access`, as before levels.
            Reach::Catalogs(
                reach
                    .iter()
                    .map(|name| (name.to_string(), access))
                    .collect(),
            )
        },
        default_catalog: default.map(str::to_string),
        actor: key_id.to_string(),
        kind: CredentialKind::ApiKey,
        user: None,
        user_id: None,
        gates: Default::default(),
        suspended: Default::default(),
    })
}

/// A `read` key `tester` reaching `reach` (every catalog when empty), with
/// a default.
///
/// # Arguments
/// * `reach` - the catalogs it is granted; empty for every catalog.
/// * `default` - its default catalog.
///
/// # Returns
/// The principal, as the auth layer would put it in a request.
pub(crate) fn principal(reach: &[&str], default: Option<&str>) -> Arc<Principal> {
    principal_as("tester", Access::Read, reach, default)
}

/// Stores a key row, so changesets can name it (`changesets.key_id` is a
/// foreign key). Its stored access does not matter: tests build the
/// principal with [`principal_as`].
///
/// # Arguments
/// * `test` - the app.
/// * `key_id` - the key's id.
///
/// # Panics
/// If the store refuses it.
pub(crate) async fn store_key(test: &TestApp, key_id: &str) {
    crate::store::keys::insert_legacy_test_key(
        test.app.pool(),
        key_id,
        Access::Admin,
        key_id,
        Utc::now(),
    )
    .await;
}

/// A tool context for a `write` key reaching every catalog: the key's row
/// is stored first (a changeset's `key_id` is a foreign key).
///
/// # Arguments
/// * `test` - the app.
/// * `key_id` - the key's id, also its actor (call once per key and test).
/// * `default` - its default catalog.
///
/// # Returns
/// The context a write tool would build for that key. A second connection
/// of the same key is `context(test, principal_as(...), None)`.
///
/// # Panics
/// If the key row cannot be stored (already stored in this test, say).
pub(crate) async fn writer(test: &TestApp, key_id: &str, default: &str) -> ToolContext {
    store_key(test, key_id).await;
    context(
        test,
        principal_as(key_id, Access::Write, &[], Some(default)),
        None,
    )
}

/// Opens a changeset for `ctx`'s key in its write target (the first
/// selected catalog), as `open_changeset` would.
///
/// # Arguments
/// * `ctx` - the connection.
///
/// # Returns
/// The new changeset's handle.
///
/// # Panics
/// If the tool refuses (a key below `write`, say).
pub(crate) async fn open(ctx: &ToolContext) -> String {
    let params = crate::mcp::changesets::OpenParams { catalog: None };
    crate::mcp::changesets::open_changeset(ctx, params)
        .await
        .expect("open a changeset")
        .changeset
}

/// Opens a changeset for `ctx`'s key in `catalog` at `at` (its creation and
/// last-change time), in the lock order.
///
/// # Arguments
/// * `test` - the app.
/// * `ctx` - the connection: its key and actor own the changeset.
/// * `catalog` - the catalog's name.
/// * `at` - the time to record.
///
/// # Returns
/// The new changeset.
///
/// # Panics
/// If the catalog is not loaded or the store fails.
pub(crate) async fn open_at(
    test: &TestApp,
    ctx: &ToolContext,
    catalog: &str,
    at: DateTime<Utc>,
) -> ChangesetRow {
    let state = test.app.catalog(catalog).expect("a loaded catalog");
    let principal = ctx.principal();
    let mut tx = test.app.pool().begin().await.expect("begin");
    changesets::lock_catalog(&mut tx, state.id())
        .await
        .expect("lock the catalog");
    let changeset = changesets::open(&mut tx, state.id(), &principal.key_id, &principal.actor, at)
        .await
        .expect("open the changeset");
    tx.commit().await.expect("commit");
    changeset
}

/// A changeset by its handle.
///
/// # Arguments
/// * `test` - the app.
/// * `handle` - the handle.
///
/// # Returns
/// The row, in whatever state it is.
///
/// # Panics
/// If there is no such changeset or the store fails.
pub(crate) async fn changeset_row(test: &TestApp, handle: &str) -> ChangesetRow {
    changesets::by_handle(test.app.pool(), handle)
        .await
        .expect("read the changeset")
        .expect("the changeset exists")
}

/// One pending edit, for [`pending_entry`].
#[derive(Debug, Clone, Copy)]
pub(crate) struct PendingEdit<'a> {
    /// Catalog-relative path.
    pub(crate) path: &'a str,
    /// The new text; `None` deletes the path.
    pub(crate) text: Option<&'a str>,
    /// The time of the edit.
    pub(crate) at: DateTime<Utc>,
}

/// Stores an edit in the changeset `handle` names, in the lock order, as a
/// write tool would (without the write rules).
///
/// # Arguments
/// * `test` - the app.
/// * `handle` - the changeset.
/// * `edit` - the path, text and time.
///
/// # Returns
/// The changeset after the edit.
///
/// # Panics
/// If there is no such changeset or the store fails.
pub(crate) async fn pending_entry(
    test: &TestApp,
    handle: &str,
    edit: &PendingEdit<'_>,
) -> ChangesetRow {
    let changeset = changeset_row(test, handle).await;
    let entry = EntryRow {
        path: edit.path.to_string(),
        base_revision: None,
        content: edit.text.map(|text| text.as_bytes().to_vec()),
        executable: false,
        replay: None,
    };
    let mut tx = test.app.pool().begin().await.expect("begin");
    changesets::lock_catalog(&mut tx, changeset.catalog_id)
        .await
        .expect("lock the catalog");
    changesets::upsert_entry(&mut tx, changeset.id, &entry, edit.at)
        .await
        .expect("store the entry");
    tx.commit().await.expect("commit");
    changeset_row(test, handle).await
}

/// A tool context for `principal` on a connection naming `connection`.
///
/// # Arguments
/// * `test` - the app.
/// * `principal` - the calling key.
/// * `connection` - the connection's catalogs (`X-OKF-Catalogs`), if any.
///
/// # Returns
/// The context a tool would build from such a request.
pub(crate) fn context(
    test: &TestApp,
    principal: Arc<Principal>,
    connection: Option<&[&str]>,
) -> ToolContext {
    let connection = connection.map(|names| names.iter().map(|name| name.to_string()).collect());
    ToolContext::new(Arc::clone(&test.app), principal, connection)
}

/// The team [`suspend_catalogs`] moves catalogs to and suspends.
pub(crate) const SUSPENDED_TEAM: &str = "family";

/// The notice [`suspend_catalogs`] gives the suspension.
pub(crate) const SUSPENSION_NOTICE: &str = "Paused for billing.";

/// Moves `catalogs` to a new team, [`SUSPENDED_TEAM`] (owned by
/// [`crate::db::TEST_USER`]), suspends it with [`SUSPENSION_NOTICE`], and
/// polls, so the key cache sees the suspension. Every other catalog stays
/// with [`crate::db::TEST_TEAM`], active.
///
/// # Arguments
/// * `test` - the app.
/// * `catalogs` - the catalogs to suspend (call once per test).
///
/// # Panics
/// If the team exists already or a store call fails.
pub(crate) async fn suspend_catalogs(test: &TestApp, catalogs: &[&str]) {
    let pool = test.app.pool();
    let now = Utc::now();
    let team = crate::store::teams::NewTeam {
        name: SUSPENDED_TEAM,
        display_name: &crate::labels::DisplayName::parse("Family").unwrap(),
        owner: crate::db::TEST_USER,
    };
    let actor = AuditActor::cli();
    crate::store::teams::create(pool, &team, &actor, now)
        .await
        .expect("create the team to suspend");
    for catalog in catalogs {
        catalogs::set_owner(pool, catalog, SUSPENDED_TEAM, &actor, now)
            .await
            .expect("move the catalog to the team");
    }
    crate::store::teams::suspend(pool, SUSPENDED_TEAM, Some(SUSPENSION_NOTICE), &actor, now)
        .await
        .expect("suspend the team");
    test.app.poll().await.expect("poll the suspension in");
}

/// Lifts [`suspend_catalogs`]'s suspension and polls it in.
///
/// # Panics
/// If the store or the poll fails.
pub(crate) async fn lift_suspension(test: &TestApp) {
    crate::store::teams::unsuspend(
        test.app.pool(),
        SUSPENDED_TEAM,
        &AuditActor::cli(),
        Utc::now(),
    )
    .await
    .expect("lift the suspension");
    test.app.poll().await.expect("poll the lift in");
}

/// Stores a live, admin-issued key and polls it in.
///
/// # Arguments
/// * `test` - the app.
/// * `name` - its name, also its actor (once per test).
/// * `access` - its level.
/// * `reach` - its catalogs; empty for every catalog.
/// * `maintenance_report` - whether it may submit maintenance reports.
///
/// # Returns
/// Its secret: look its principal up with [`cached_principal`] after each
/// poll, as a request would.
///
/// # Panics
/// If the store refuses the key or the poll fails.
pub(crate) async fn stored_secret(
    test: &TestApp,
    name: &str,
    access: Access,
    reach: &[&str],
    maintenance_report: bool,
) -> String {
    let names: Vec<String> = reach.iter().map(|catalog| catalog.to_string()).collect();
    let new = crate::store::keys::NewKey {
        name,
        access,
        maintenance_report,
        grants: if names.is_empty() {
            crate::store::keys::Grants::All
        } else {
            crate::store::keys::Grants::Catalogs(&names)
        },
        default_catalog: reach.first().copied(),
        actor: name,
        user: crate::db::TEST_USER,
        self_issued: false,
        expires_at: None,
        levels: &[],
    };
    let secret = crate::store::keys::create_with_id(
        test.app.pool(),
        &new,
        &crate::store::audit::AuditActor::cli(),
        Utc::now(),
    )
    .await
    .expect("create the key")
    .key;
    test.app.poll().await.expect("poll the key in");
    secret
}

/// The principal the key cache holds now for `secret`, as the auth layer
/// would put it in a request.
///
/// # Panics
/// If the cache does not know the key.
pub(crate) fn cached_principal(test: &TestApp, secret: &str) -> Arc<Principal> {
    test.app.keys().find(secret).expect("the key is cached")
}

/// Creates user `staff` (display name "Staff Member", outside every test
/// catalog's owner team) and an every-catalog admin key of it, and polls
/// it in: an operator whose reads of the test catalogs are admin reads.
///
/// # Arguments
/// * `test` - the app (once per test: the names cannot be taken twice).
///
/// # Returns
/// The key's secret.
///
/// # Panics
/// If the store refuses the user or the key, or the poll fails.
pub(crate) async fn staff_root(test: &TestApp) -> String {
    let pool = test.app.pool();
    let new_user = crate::store::users::NewUser {
        name: "staff",
        display_name: &crate::labels::DisplayName::parse("Staff Member").unwrap(),
    };
    crate::store::users::create(pool, &new_user, &AuditActor::cli(), Utc::now())
        .await
        .expect("create user staff");
    let new_key = crate::store::keys::NewKey {
        name: "staff-root",
        access: Access::Admin,
        maintenance_report: false,
        grants: crate::store::keys::Grants::All,
        default_catalog: None,
        actor: "staff-root",
        user: "staff",
        self_issued: false,
        expires_at: None,
        levels: &[],
    };
    let secret = crate::store::keys::create_with_id(pool, &new_key, &AuditActor::cli(), Utc::now())
        .await
        .expect("create the staff key")
        .key;
    test.app.poll().await.expect("poll the staff key in");
    secret
}

/// A key of user `staff` (made by [`staff_root`] first), reaching every
/// catalog: an operator credential outside every test catalog's owner
/// team, at any level, with or without the reporting flag (the
/// maintenance runner is a non-admin reporting key).
///
/// # Arguments
/// * `test` - the app.
/// * `name` - the key's name and actor (once per test).
/// * `access` - its level.
/// * `maintenance_report` - whether it may report.
///
/// # Returns
/// The key's secret, polled in.
///
/// # Panics
/// If the store refuses the key or the poll fails.
pub(crate) async fn staff_key(
    test: &TestApp,
    name: &str,
    access: Access,
    maintenance_report: bool,
) -> String {
    let new = crate::store::keys::NewKey {
        name,
        access,
        maintenance_report,
        grants: crate::store::keys::Grants::All,
        default_catalog: None,
        actor: name,
        user: "staff",
        self_issued: false,
        expires_at: None,
        levels: &[],
    };
    let secret =
        crate::store::keys::create_with_id(test.app.pool(), &new, &AuditActor::cli(), Utc::now())
            .await
            .expect("create the staff key")
            .key;
    test.app.poll().await.expect("poll the staff key in");
    secret
}

/// Turns the `require_unlock` setting on and polls it in (M14 Task 9b).
///
/// # Panics
/// If the setting cannot be stored or the poll fails.
pub(crate) async fn require_unlock(test: &TestApp) {
    use crate::store::settings::{self, SettingKey};
    settings::set(
        test.app.pool(),
        SettingKey::RequireUnlock,
        "on",
        &AuditActor::cli(),
        Utc::now(),
    )
    .await
    .expect("turn require_unlock on");
    test.app.poll().await.expect("poll the setting in");
}

/// Waits, at most 30 seconds, for every background access write the app has
/// started ([`App::settle_access`]).
///
/// # Panics
/// If the writes do not finish in time (a held lock, or a hung write).
pub(crate) async fn settle_access(test: &TestApp) {
    tokio::time::timeout(std::time::Duration::from_secs(30), test.app.settle_access())
        .await
        .expect("the background access writes finish within 30 seconds");
}

/// A minimal MCP client over HTTP, for tests that need a real session
/// through the real router (`http::serve` on a loopback socket): one
/// `initialize`, then any number of tool calls.
pub(crate) struct McpProbe {
    client: reqwest::Client,
    endpoint: String,
    key: String,
    session: String,
}

impl McpProbe {
    /// Opens a session: `initialize`, then the `initialized` notification.
    ///
    /// # Arguments
    /// * `endpoint` - the `/mcp` URL.
    /// * `key` - the bearer key.
    ///
    /// # Panics
    /// If the server refuses: a broken test setup.
    pub(crate) async fn open(endpoint: &str, key: &str) -> Self {
        let client = reqwest::Client::new();
        let init = serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": {"name": "probe", "version": "1"}
            }
        });
        let response = Self::post(&client, endpoint, key, None, &init).await;
        let session = response
            .headers()
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
            .expect("a session id")
            .to_string();
        let _ = response.text().await.expect("the initialize answer");
        let probe = Self {
            client,
            endpoint: endpoint.to_string(),
            key: key.to_string(),
            session,
        };
        let initialized =
            serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
        let accepted = Self::post(
            &probe.client,
            endpoint,
            key,
            Some(&probe.session),
            &initialized,
        )
        .await;
        assert!(accepted.status().is_success(), "{}", accepted.status());
        probe
    }

    async fn post(
        client: &reqwest::Client,
        endpoint: &str,
        key: &str,
        session: Option<&str>,
        body: &serde_json::Value,
    ) -> reqwest::Response {
        let mut request = client
            .post(endpoint)
            .bearer_auth(key)
            .header("accept", "application/json, text/event-stream")
            .json(body);
        if let Some(session) = session {
            request = request.header("mcp-session-id", session);
        }
        request.send().await.expect("the server answers")
    }

    /// Calls a tool.
    ///
    /// # Arguments
    /// * `tool` - its name (any text: an unknown one is a protocol error).
    /// * `arguments` - its arguments.
    ///
    /// # Returns
    /// The JSON-RPC answer (`result` or `error`).
    ///
    /// # Panics
    /// If the answer is not a JSON-RPC message: a broken test setup.
    pub(crate) async fn call(&self, tool: &str, arguments: serde_json::Value) -> serde_json::Value {
        let body = serde_json::json!({
            "jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": {"name": tool, "arguments": arguments}
        });
        let text = Self::post(
            &self.client,
            &self.endpoint,
            &self.key,
            Some(&self.session),
            &body,
        )
        .await
        .text()
        .await
        .expect("the answer's body");
        // A JSON answer, or an event stream whose `data:` lines carry it.
        std::iter::once(text.as_str())
            .chain(text.lines().filter_map(|line| line.strip_prefix("data:")))
            .filter_map(|candidate| {
                serde_json::from_str::<serde_json::Value>(candidate.trim()).ok()
            })
            .find(|message| message.get("id") == Some(&serde_json::json!(2)))
            .unwrap_or_else(|| panic!("no JSON-RPC answer in {text}"))
    }

    /// Ends the session (`DELETE /mcp`).
    pub(crate) async fn close(self) {
        let response = self
            .client
            .delete(&self.endpoint)
            .bearer_auth(&self.key)
            .header("mcp-session-id", &self.session)
            .send()
            .await
            .expect("the server answers");
        assert!(response.status().is_success(), "{}", response.status());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn an_app_with_metrics_renders_into_its_own_registry() {
        let (test_app, registry) = app_with_metrics(&[], Config::default()).await;

        let text = render(&registry);

        assert!(text.contains("# TYPE okf_build_info gauge"), "{text}");
        // The app holds the handle `app_with_metrics` built, not the global one.
        let _ = test_app.app.metrics();
    }
}
