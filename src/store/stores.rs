//! The `stores` table (migration `0036_stores.sql`): where each catalog
//! store's database is, read by [`crate::stores::Stores`] when it opens a
//! store's pool.
//!
//! A row holds a *reference* to a credential, never the credential itself
//! (plan Conventions, "Privacy rules"), so a row is safe to log apart from
//! the host, which the callers keep out of any reply.

use chrono::{DateTime, Utc};
use sqlx::{PgExecutor, PgPool};

use crate::error::OkfError;
use crate::invalidate::{invalidate, Invalidation};
use crate::store::audit::{self, Action, AuditActor, Entry, People, Target};
use crate::store::decode_column;
use crate::stores::{placement, StoreName, Stores};

/// How a store's credential is found (`stores.credential_kind`).
// `rename_all = "kebab-case"` makes serde read and write the spellings the
// table stores (`iam`, `secrets-manager`, `env`), for the admin API's body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CredentialKind {
    /// An RDS IAM authentication token, signed from the server's AWS
    /// credentials.
    Iam,
    /// A secret in a secrets manager, named by its ARN.
    SecretsManager,
    /// An environment variable holding the password.
    Env,
}

impl CredentialKind {
    /// The kind as stored.
    ///
    /// # Returns
    /// `"iam"`, `"secrets-manager"` or `"env"`.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(uninstrumented_fn, reason = "a pure match over the variants")
    )]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Iam => "iam",
            Self::SecretsManager => "secrets-manager",
            Self::Env => "env",
        }
    }

    /// Reads a stored spelling.
    ///
    /// # Arguments
    /// * `text` - the column's value.
    ///
    /// # Returns
    /// The kind, or `None` for a spelling the table's CHECK does not allow.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(uninstrumented_fn, reason = "a pure match over the spellings")
    )]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "iam" => Some(Self::Iam),
            "secrets-manager" => Some(Self::SecretsManager),
            "env" => Some(Self::Env),
            _ => None,
        }
    }
}

/// Where a store's database is (`stores.location_kind`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreLocation {
    /// The account database itself.
    AccountDatabase,
    /// A database on a host of its own.
    Host {
        /// The server's host name.
        host: String,
        /// Its port.
        port: u16,
        /// The database's name.
        database: String,
        /// How the credential is found.
        credential_kind: CredentialKind,
        /// What names the credential: a variable name, a secret ARN or an
        /// IAM identity. Never the credential.
        credential_ref: String,
    },
}

/// One row of `stores`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreRow {
    /// The store's name.
    pub name: StoreName,
    /// Where its database is.
    pub location: StoreLocation,
}

/// [`StoreRow`] as the query returns it: every column of the location,
/// nullable.
#[derive(sqlx::FromRow)]
struct StoreRowColumns {
    name: String,
    location_kind: String,
    host: Option<String>,
    port: Option<i32>,
    database: Option<String>,
    credential_kind: Option<String>,
    credential_ref: Option<String>,
}

impl TryFrom<StoreRowColumns> for StoreRow {
    type Error = OkfError;

    /// Checks the columns the table's CHECKs already constrain, so a
    /// hand-edited row is an error instead of a guess.
    ///
    /// # Errors
    /// [`OkfError::Database`] (a `ColumnDecode`, naming the column) for a
    /// value the table would not have accepted.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(uninstrumented_fn, reason = "maps a value, no I/O")
    )]
    fn try_from(columns: StoreRowColumns) -> Result<Self, Self::Error> {
        let name = decode_column(StoreName::parse(&columns.name).ok(), "stores.name")?;
        let location = match columns.location_kind.as_str() {
            "account-database" => StoreLocation::AccountDatabase,
            _ => {
                let host = decode_column(columns.host, "stores.host")?;
                let port = decode_column(
                    columns.port.and_then(|port| u16::try_from(port).ok()),
                    "stores.port",
                )?;
                let database = decode_column(columns.database, "stores.database")?;
                let credential_kind = decode_column(
                    columns
                        .credential_kind
                        .as_deref()
                        .and_then(CredentialKind::parse),
                    "stores.credential_kind",
                )?;
                let credential_ref =
                    decode_column(columns.credential_ref, "stores.credential_ref")?;
                StoreLocation::Host {
                    host,
                    port,
                    database,
                    credential_kind,
                    credential_ref,
                }
            }
        };
        Ok(Self { name, location })
    }
}

/// The row of the store called `name`.
///
/// # Arguments
/// * `executor` - the account database.
/// * `name` - the store's name.
///
/// # Returns
/// Its row, or `None` if there is no such store.
///
/// # Errors
/// [`OkfError::Database`].
#[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "stores.by_name"))]
pub async fn by_name<'e>(
    executor: impl PgExecutor<'e>,
    name: &StoreName,
) -> Result<Option<StoreRow>, OkfError> {
    let columns = sqlx::query_as::<_, StoreRowColumns>(
        "SELECT name, location_kind, host, port, database, credential_kind, \
             credential_ref FROM stores WHERE name = $1",
    )
    .bind(name.as_str())
    .fetch_optional(executor)
    .await?;
    columns.map(StoreRow::try_from).transpose()
}

/// The name of every store, in name order.
///
/// # Arguments
/// * `executor` - the account database.
///
/// # Returns
/// The names.
///
/// # Errors
/// [`OkfError::Database`], or a `ColumnDecode` for a stored name that is not
/// a valid store name.
#[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "stores.names"))]
pub async fn names<'e>(executor: impl PgExecutor<'e>) -> Result<Vec<StoreName>, OkfError> {
    let names: Vec<String> = sqlx::query_scalar("SELECT name FROM stores ORDER BY name")
        .fetch_all(executor)
        .await?;
    names
        .into_iter()
        .map(|name| decode_column(StoreName::parse(&name).ok(), "stores.name"))
        .collect()
}

/// One store as the admin surface lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreSummary {
    /// The row.
    pub row: StoreRow,
    /// When it was added (server clock).
    pub created_at: DateTime<Utc>,
    /// Whether new catalogs go here unless their team is pinned elsewhere.
    pub is_default: bool,
    /// How many teams are pinned to it.
    pub pinned_teams: i64,
}

/// [`StoreSummary`] as the query returns it.
#[derive(sqlx::FromRow)]
struct SummaryColumns {
    #[sqlx(flatten)]
    row: StoreRowColumns,
    created_at: DateTime<Utc>,
    pinned_teams: i64,
}

/// Every store with its creation time and pin count, in name order, and
/// which one is the default.
///
/// # Arguments
/// * `executor` - the account database.
///
/// # Returns
/// The summaries.
///
/// # Errors
/// [`OkfError::Database`], or a `ColumnDecode` for a hand-edited row.
#[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "stores.summaries"))]
pub async fn summaries(executor: &PgPool) -> Result<Vec<StoreSummary>, OkfError> {
    let default = placement::default_store(executor).await?;
    let columns = sqlx::query_as::<_, SummaryColumns>(
        "SELECT s.name, s.location_kind, s.host, s.port, s.database, s.credential_kind, \
                s.credential_ref, s.created_at, \
                (SELECT count(*) FROM teams t WHERE t.store = s.name) AS pinned_teams \
         FROM stores s ORDER BY s.name",
    )
    .fetch_all(executor)
    .await?;
    columns
        .into_iter()
        .map(|columns| {
            let row = StoreRow::try_from(columns.row)?;
            Ok(StoreSummary {
                is_default: row.name == default,
                row,
                created_at: columns.created_at,
                pinned_teams: columns.pinned_teams,
            })
        })
        .collect()
}

/// Adds a store: checks that it can be opened and is a database of its own,
/// migrates it, then writes its row (`insert`, below).
///
/// The connection check runs first and holds no transaction (a connection
/// on the account pool is never held while a store's pool connects). It
/// refuses the account database under another host name (it holds the
/// account tables) and a database whose identity row names another store (a
/// second host name for a registered store). Until Task 20 a `host` store is
/// then refused ([`placement::refuse_host_stores_until_task_20`]). Last, the
/// store's schema is migrated, which writes its identity row, so the
/// database answers to this name before the row exists.
///
/// # Arguments
/// * `pool` - the account database.
/// * `stores` - the registry, whose open path does the connection check.
/// * `row` - the store to add.
/// * `actor` - who, for the audit log.
/// * `now` - the server clock.
///
/// # Returns
/// The new store.
///
/// # Errors
/// [`OkfError::Refused`] when the store cannot be reached with these
/// settings (the reply names no host, port or credential; the cause is
/// logged locally), when it is the account database or another store's
/// database, when host stores are not enabled yet, when its schema cannot be
/// migrated, or when the name or the host, port and database is already
/// used; [`OkfError::Database`], a failed audit write included.
#[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "stores.add"))]
pub async fn add(
    pool: &PgPool,
    stores: &Stores,
    row: StoreRow,
    actor: &AuditActor,
    now: DateTime<Utc>,
) -> Result<StoreSummary, OkfError> {
    stores
        .check(row.clone())
        .await
        .map_err(unreachable_refused)?;
    placement::refuse_host_stores_until_task_20(&row.location)?;
    stores
        .prepare(row.clone())
        .await
        .map_err(|error| match unreachable_refused(error) {
            refused @ OkfError::Refused { .. } => refused,
            other => {
                tracing::warn!(error = %other, "a new store's schema could not be migrated");
                OkfError::Refused {
                    reason: "the store's schema could not be migrated".to_string(),
                }
            }
        })?;
    insert(pool, row, actor, now).await
}

/// A store that could not be opened, as [`add`] refuses it: without the
/// host, port or credential.
///
/// # Arguments
/// * `error` - what the open returned.
///
/// # Returns
/// [`OkfError::Refused`] for `StoreUnavailable`, anything else unchanged.
#[cfg_attr(
    dylint_lib = "hyp_tracing",
    allow(uninstrumented_fn, reason = "a pure mapping of one error variant")
)]
fn unreachable_refused(error: OkfError) -> OkfError {
    match error {
        OkfError::StoreUnavailable { .. } => OkfError::Refused {
            reason: "the store cannot be reached with these settings; check the host, port, \
                     database and credential"
                .to_string(),
        },
        other => other,
    }
}

/// Writes a store's row, records `store.added` and sends the store
/// invalidation, in one transaction: [`add`] without its checks.
///
/// # Arguments
/// * `pool` - the account database.
/// * `row` - the store, which must be a `host` store.
/// * `actor` - who, for the audit log.
/// * `now` - the server clock.
///
/// # Errors
/// [`OkfError::Refused`] for the account-database location or a name or
/// host, port and database already in use; [`OkfError::Database`], a failed
/// audit write included.
#[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "stores.insert"))]
async fn insert(
    pool: &PgPool,
    row: StoreRow,
    actor: &AuditActor,
    now: DateTime<Utc>,
) -> Result<StoreSummary, OkfError> {
    let StoreLocation::Host {
        host,
        port,
        database,
        credential_kind,
        credential_ref,
    } = &row.location
    else {
        // Only `main` is inside the account database, and it exists.
        return Err(OkfError::Refused {
            reason: "the account database already has its store, main".to_string(),
        });
    };
    let mut tx = pool.begin().await?;
    audit::lock_people(&mut tx, actor, &People::default()).await?;
    let inserted = sqlx::query(
        "INSERT INTO stores (name, location_kind, host, port, database, credential_kind, \
                             credential_ref, created_at) \
         VALUES ($1, 'host', $2, $3, $4, $5, $6, $7)",
    )
    .bind(row.name.as_str())
    .bind(host)
    .bind(i32::from(*port))
    .bind(database)
    .bind(credential_kind.as_str())
    .bind(credential_ref)
    .bind(now)
    .execute(&mut *tx)
    .await;
    if let Err(error) = inserted {
        return Err(
            match error.as_database_error().and_then(|db| db.constraint()) {
                Some("stores_pkey") => OkfError::Refused {
                    reason: format!("store {} already exists", row.name),
                },
                Some("stores_one_row_per_database") => OkfError::Refused {
                    reason: "another store already uses that host, port and database".to_string(),
                },
                _ => error.into(),
            },
        );
    }
    audit::record(
        &mut tx,
        actor,
        &Entry {
            action: Action::StoreAdded,
            target: Target::Store(row.name.as_str()),
            person: None,
            teams: &[],
            // Never the host or the credential reference.
            details: serde_json::json!({
                "location": "host",
                "credential_kind": credential_kind.as_str(),
            }),
        },
        now,
    )
    .await?;
    invalidate(&mut tx, &[Invalidation::store(&row.name)]).await?;
    tx.commit().await?;
    Ok(StoreSummary {
        is_default: false,
        row,
        created_at: now,
        pinned_teams: 0,
    })
}

/// Makes `name` the store new catalogs go on (unless their team is pinned),
/// records `setting.default_store_changed` and sends the settings
/// invalidation, in one transaction. Catalogs that exist stay where they
/// are. The same store again is no change: nothing is written or recorded.
///
/// Lock order (`tools/agentic/lessons.md`): the actor's user rows, the
/// store's row (`FOR SHARE`, so it cannot go while it is the default), the
/// generation row, then the setting.
///
/// # Arguments
/// * `pool` - the account database.
/// * `name` - the store.
/// * `actor` - who, for the audit log.
/// * `now` - the server clock.
///
/// # Errors
/// [`OkfError::UnknownName`] for a store with no row; [`OkfError::Database`],
/// a failed audit write included.
#[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "stores.set_default"))]
pub async fn set_default(
    pool: &PgPool,
    name: &StoreName,
    actor: &AuditActor,
    now: DateTime<Utc>,
) -> Result<(), OkfError> {
    let mut tx = pool.begin().await?;
    audit::lock_people(&mut tx, actor, &People::default()).await?;
    lock_store(&mut tx, name).await?;
    invalidate(&mut tx, &[Invalidation::Settings]).await?;
    let previous: Option<serde_json::Value> =
        sqlx::query_scalar("SELECT value FROM server_settings WHERE key = $1 FOR UPDATE")
            .bind(placement::DEFAULT_STORE_KEY)
            .fetch_optional(&mut *tx)
            .await?;
    let json = serde_json::json!(name.as_str());
    if previous.as_ref() == Some(&json) {
        // Unchanged: dropping `tx` rolls the generation bump back, and its
        // `NOTIFY` is never sent.
        return Ok(());
    }
    sqlx::query(
        "INSERT INTO server_settings (key, value, updated_at, updated_by) \
         VALUES ($1, $2, $3, $4) \
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, \
             updated_at = EXCLUDED.updated_at, updated_by = EXCLUDED.updated_by",
    )
    .bind(placement::DEFAULT_STORE_KEY)
    .bind(&json)
    .bind(now)
    .bind(crate::store::settings::updated_by(actor))
    .execute(&mut *tx)
    .await?;
    audit::record(
        &mut tx,
        actor,
        &Entry {
            action: Action::SettingDefaultStoreChanged,
            target: Target::Setting(placement::DEFAULT_STORE_KEY),
            person: None,
            teams: &[],
            details: serde_json::json!({"from": previous, "to": json}),
        },
        now,
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Locks a store's row `FOR SHARE`, so a change that names it cannot race
/// the row's removal.
///
/// # Errors
/// [`OkfError::UnknownName`] for a store with no row; [`OkfError::Database`].
#[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "stores.lock_store"))]
pub(crate) async fn lock_store(
    conn: &mut sqlx::PgConnection,
    name: &StoreName,
) -> Result<(), OkfError> {
    let found: Option<i32> = sqlx::query_scalar("SELECT 1 FROM stores WHERE name = $1 FOR SHARE")
        .bind(name.as_str())
        .fetch_optional(&mut *conn)
        .await?;
    found.map(|_| ()).ok_or_else(|| OkfError::UnknownName {
        noun: "store",
        name: name.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::invalidate::testing::invalidations_of;

    #[tokio::test]
    async fn main_is_the_account_database_row() {
        let db = crate::db::test_db().await;
        crate::db::main_in_the_account_database(&db.pool).await;

        let row = by_name(&db.pool, &StoreName::main())
            .await
            .unwrap()
            .unwrap();

        assert_eq!(row.name, StoreName::main());
        assert_eq!(row.location, StoreLocation::AccountDatabase);
        assert_eq!(names(&db.pool).await.unwrap(), [StoreName::main()]);
        let unknown = StoreName::parse("shared-1").unwrap();
        assert_eq!(by_name(&db.pool, &unknown).await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_host_row_reads_back_with_its_credential_reference() {
        let db = crate::db::test_db().await;
        sqlx::query(
            "INSERT INTO stores (name, location_kind, host, port, database, \
             credential_kind, credential_ref, created_at) \
             VALUES ('shared-1', 'host', 'db.example.internal', 5432, 'okf', 'env', \
             'SHARED_1_PASSWORD', now())",
        )
        .execute(&db.pool)
        .await
        .unwrap();

        let name = StoreName::parse("shared-1").unwrap();
        let row = by_name(&db.pool, &name).await.unwrap().unwrap();

        assert_eq!(
            row.location,
            StoreLocation::Host {
                host: "db.example.internal".to_string(),
                port: 5432,
                database: "okf".to_string(),
                credential_kind: CredentialKind::Env,
                credential_ref: "SHARED_1_PASSWORD".to_string(),
            }
        );
        assert_eq!(names(&db.pool).await.unwrap(), [StoreName::main(), name]);
    }

    /// Registry settings for these tests: a short connect timeout.
    fn pool_settings() -> crate::stores::PoolSettings {
        crate::stores::PoolSettings {
            max_connections: 2,
            idle_timeout: std::time::Duration::from_secs(60),
            connect_timeout: std::time::Duration::from_secs(2),
        }
    }

    /// A `host` row for store `name` that points at `host:port/database`,
    /// with an `env` credential in `STORE_PASSWORD`.
    fn host_row(name: &str, host: &str, port: u16, database: &str) -> StoreRow {
        StoreRow {
            name: StoreName::parse(name).unwrap(),
            location: StoreLocation::Host {
                host: host.to_string(),
                port,
                database: database.to_string(),
                credential_kind: CredentialKind::Env,
                credential_ref: "STORE_PASSWORD".to_string(),
            },
        }
    }

    /// A registry whose environment holds the test database's password, and
    /// a row for store `name` that reaches the test database itself.
    fn reachable(db: &crate::db::pg_support::TestDb, name: &str) -> (Stores, StoreRow) {
        reachable_on(db, db, name)
    }

    /// A registry on `db`'s account database whose environment holds
    /// `target`'s password, and a row for store `name` that reaches `target`
    /// as `host`.
    fn reachable_as(
        db: &crate::db::pg_support::TestDb,
        target: &crate::db::pg_support::TestDb,
        name: &str,
        host: &str,
    ) -> (Stores, StoreRow) {
        let (stores, row) = reachable_on(db, target, name);
        let StoreLocation::Host { port, database, .. } = &row.location else {
            unreachable!("a host row")
        };
        let row = host_row(name, host, *port, database);
        (stores, row)
    }

    /// A registry on `db`'s account database whose environment holds
    /// `target`'s password, and a row for store `name` that reaches `target`.
    fn reachable_on(
        db: &crate::db::pg_support::TestDb,
        target: &crate::db::pg_support::TestDb,
        name: &str,
    ) -> (Stores, StoreRow) {
        let after_scheme = target.url.split("://").nth(1).expect("a URL");
        let password = after_scheme
            .split('@')
            .next()
            .and_then(|credentials| credentials.split(':').nth(1))
            .unwrap_or_default()
            .to_string();
        let stores = Stores::with_environment(
            db.pool.clone(),
            pool_settings(),
            std::sync::Arc::new(move |variable| {
                (variable == "STORE_PASSWORD").then(|| password.clone())
            }),
        );
        let options = target.pool.connect_options();
        let row = host_row(
            name,
            options.get_host(),
            options.get_port(),
            options.get_database().expect("a database"),
        );
        (stores, row)
    }

    async fn store_names(pool: &PgPool) -> Vec<String> {
        names(pool)
            .await
            .unwrap()
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn adding_an_unreachable_store_is_refused() {
        let db = crate::db::test_db().await;
        // A port nothing listens on: bind one, then close it.
        let closed = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let (stores, _) = reachable(&db, "shared-1");
        let row = host_row("shared-1", "127.0.0.1", closed, "okf");

        let refused = add(&db.pool, &stores, row, &AuditActor::cli(), Utc::now())
            .await
            .unwrap_err();

        assert!(matches!(refused, OkfError::Refused { .. }), "{refused:?}");
        // The reply names no host, port or credential.
        let reply = refused.to_json().to_string();
        for private in ["127.0.0.1", &closed.to_string(), "STORE_PASSWORD"] {
            assert!(!reply.contains(private), "{private} leaked into {reply}");
        }
        assert_eq!(store_names(&db.pool).await, ["main"]);
        assert!(audit::testing::entries(&db.pool, "store.added")
            .await
            .is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn adding_a_host_store_is_refused_until_enabled() {
        let db = crate::db::test_db().await;
        // An empty database of its own, so it is the P8 refusal and not the
        // connection or identity check.
        let target = crate::db::pg_support::TestDb::start().await;
        let (stores, row) = reachable_on(&db, &target, "shared-1");

        let refused = add(&db.pool, &stores, row, &AuditActor::cli(), Utc::now())
            .await
            .unwrap_err();

        assert!(
            matches!(&refused, OkfError::Refused { reason } if reason.contains("not enabled")),
            "{refused:?}"
        );
        assert_eq!(store_names(&db.pool).await, ["main"]);
        // Refused before the migration: the database is left as it was.
        let mut connection = target.pool.acquire().await.unwrap();
        assert_eq!(crate::db::store_owner(&mut connection).await.unwrap(), None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn adding_the_account_database_as_a_host_store_is_refused() {
        let db = crate::db::test_db().await;
        // A `host` row that reaches the account database itself.
        let (stores, row) = reachable(&db, "shared-1");

        let refused = add(&db.pool, &stores, row, &AuditActor::cli(), Utc::now())
            .await
            .unwrap_err();

        assert!(
            matches!(&refused, OkfError::Refused { reason } if reason.contains("account database")),
            "{refused:?}"
        );
        assert_eq!(store_names(&db.pool).await, ["main"]);
        assert!(audit::testing::entries(&db.pool, "store.added")
            .await
            .is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn adding_a_store_that_aliases_another_is_refused() {
        let db = crate::db::test_db().await;
        let target = crate::db::pg_support::TestDb::start().await;
        // Store `shared-1` is registered on the target and has been opened,
        // so the target's identity row names it.
        let (stores, first) = reachable_on(&db, &target, "shared-1");
        insert(&db.pool, first.clone(), &AuditActor::cli(), Utc::now())
            .await
            .unwrap();
        stores.get(&first.name).await.unwrap();
        // The same server under a second host name, which the unique index on
        // host, port and database cannot see.
        let StoreLocation::Host { host, .. } = &first.location else {
            unreachable!("a host row")
        };
        let alias = match host.as_str() {
            "localhost" => "127.0.0.1",
            _ => "localhost",
        };
        let (_, second) = reachable_as(&db, &target, "shared-2", alias);

        let refused = add(&db.pool, &stores, second, &AuditActor::cli(), Utc::now())
            .await
            .unwrap_err();

        assert!(
            matches!(&refused, OkfError::Refused { reason }
                if reason.contains("already belongs to store shared-1")),
            "{refused:?}"
        );
        assert_eq!(store_names(&db.pool).await, ["main", "shared-1"]);
        let mut connection = target.pool.acquire().await.unwrap();
        assert_eq!(
            crate::db::store_owner(&mut connection)
                .await
                .unwrap()
                .as_deref(),
            Some("shared-1")
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_store_row_is_refused_for_a_taken_name_or_database() {
        let db = crate::db::test_db().await;
        let (_, row) = reachable(&db, "shared-1");
        insert(&db.pool, row.clone(), &AuditActor::cli(), Utc::now())
            .await
            .unwrap();

        let same_database = insert(
            &db.pool,
            StoreRow {
                name: StoreName::parse("shared-2").unwrap(),
                ..row.clone()
            },
            &AuditActor::cli(),
            Utc::now(),
        )
        .await
        .unwrap_err();
        let same_name = insert(
            &db.pool,
            host_row("shared-1", "db.example.internal", 5432, "okf"),
            &AuditActor::cli(),
            Utc::now(),
        )
        .await
        .unwrap_err();
        let main = insert(
            &db.pool,
            StoreRow {
                name: StoreName::parse("elsewhere").unwrap(),
                location: StoreLocation::AccountDatabase,
            },
            &AuditActor::cli(),
            Utc::now(),
        )
        .await
        .unwrap_err();

        for refused in [same_database, same_name, main] {
            assert!(matches!(refused, OkfError::Refused { .. }), "{refused:?}");
        }
        assert_eq!(store_names(&db.pool).await, ["main", "shared-1"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn every_store_change_is_audited() {
        let db = crate::db::test_db().await;
        let (_, row) = reachable(&db, "shared-1");
        let name = row.name.clone();
        let actor = AuditActor::cli();

        insert(&db.pool, row, &actor, Utc::now()).await.unwrap();
        set_default(&db.pool, &name, &actor, Utc::now())
            .await
            .unwrap();
        crate::store::teams::set_store(
            &db.pool,
            crate::db::TEST_TEAM,
            Some(&name),
            &actor,
            Utc::now(),
        )
        .await
        .unwrap();
        crate::store::teams::set_store(&db.pool, crate::db::TEST_TEAM, None, &actor, Utc::now())
            .await
            .unwrap();

        let added = audit::testing::entries(&db.pool, "store.added").await;
        let default = audit::testing::entries(&db.pool, "setting.default_store_changed").await;
        let pinned = audit::testing::entries(&db.pool, "team.store_pinned").await;
        let unpinned = audit::testing::entries(&db.pool, "team.store_unpinned").await;
        assert_eq!(added.len(), 1);
        assert_eq!(added[0].target_kind, "store");
        assert_eq!(added[0].target.as_deref(), Some("shared-1"));
        // The details name neither the host nor the credential reference.
        assert_eq!(
            added[0].details,
            serde_json::json!({"location": "host", "credential_kind": "env"})
        );
        assert_eq!(default.len(), 1);
        assert_eq!(default[0].target.as_deref(), Some("default_store"));
        assert_eq!(
            default[0].details,
            serde_json::json!({"from": "main", "to": "shared-1"})
        );
        assert_eq!(pinned.len(), 1);
        assert_eq!(
            pinned[0].details,
            serde_json::json!({"from": null, "to": "shared-1"})
        );
        assert_eq!(unpinned.len(), 1);
        assert_eq!(
            unpinned[0].details,
            serde_json::json!({"from": "shared-1", "to": null})
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_store_change_reaches_every_replica_through_the_change_feed() {
        let db = crate::db::test_db().await;
        let (_, row) = reachable(&db, "shared-1");
        let name = row.name.clone();
        let actor = AuditActor::cli();

        let added = invalidations_of(&db.pool, async {
            insert(&db.pool, row, &actor, Utc::now()).await.unwrap();
        })
        .await;
        let default = invalidations_of(&db.pool, async {
            set_default(&db.pool, &name, &actor, Utc::now())
                .await
                .unwrap();
        })
        .await;
        let pinned = invalidations_of(&db.pool, async {
            crate::store::teams::set_store(
                &db.pool,
                crate::db::TEST_TEAM,
                Some(&name),
                &actor,
                Utc::now(),
            )
            .await
            .unwrap();
        })
        .await;

        assert_eq!(added, [Invalidation::store(&name)]);
        assert_eq!(default, [Invalidation::Settings]);
        assert_eq!(pinned.len(), 1);
        assert!(matches!(pinned[0], Invalidation::Team { .. }), "{pinned:?}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_unchanged_default_or_pin_records_and_sends_nothing() {
        let db = crate::db::test_db().await;
        let actor = AuditActor::cli();

        let sent = invalidations_of(&db.pool, async {
            set_default(&db.pool, &StoreName::main(), &actor, Utc::now())
                .await
                .unwrap();
            let changed = crate::store::teams::set_store(
                &db.pool,
                crate::db::TEST_TEAM,
                None,
                &actor,
                Utc::now(),
            )
            .await
            .unwrap();
            assert!(!changed);
        })
        .await;

        assert!(sent.is_empty(), "{sent:?}");
        for action in [
            "setting.default_store_changed",
            "team.store_unpinned",
            "team.store_pinned",
        ] {
            assert!(audit::testing::entries(&db.pool, action).await.is_empty());
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_unknown_store_or_team_is_refused_and_changes_nothing() {
        let db = crate::db::test_db().await;
        let actor = AuditActor::cli();
        let nowhere = StoreName::parse("nowhere").unwrap();

        let default = set_default(&db.pool, &nowhere, &actor, Utc::now())
            .await
            .unwrap_err();
        let pin = crate::store::teams::set_store(
            &db.pool,
            crate::db::TEST_TEAM,
            Some(&nowhere),
            &actor,
            Utc::now(),
        )
        .await
        .unwrap_err();
        let team = crate::store::teams::set_store(
            &db.pool,
            "no-such-team",
            Some(&StoreName::main()),
            &actor,
            Utc::now(),
        )
        .await
        .unwrap_err();

        for refused in [default, pin, team] {
            assert!(
                matches!(refused, OkfError::UnknownName { .. }),
                "{refused:?}"
            );
        }
        assert_eq!(
            placement::default_store(&db.pool).await.unwrap(),
            StoreName::main()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_listing_marks_the_default_and_counts_pins() {
        let db = crate::db::test_db().await;
        let (_, row) = reachable(&db, "shared-1");
        let name = row.name.clone();
        let actor = AuditActor::cli();
        insert(&db.pool, row, &actor, Utc::now()).await.unwrap();
        set_default(&db.pool, &name, &actor, Utc::now())
            .await
            .unwrap();
        crate::store::teams::set_store(
            &db.pool,
            crate::db::TEST_TEAM,
            Some(&name),
            &actor,
            Utc::now(),
        )
        .await
        .unwrap();

        let listed = summaries(&db.pool).await.unwrap();

        let shape: Vec<_> = listed
            .iter()
            .map(|summary| {
                (
                    summary.row.name.to_string(),
                    summary.is_default,
                    summary.pinned_teams,
                )
            })
            .collect();
        assert_eq!(
            shape,
            [
                ("main".to_string(), false, 0),
                ("shared-1".to_string(), true, 1)
            ]
        );
    }

    #[test]
    fn credential_kinds_round_trip_through_their_spelling() {
        for kind in [
            CredentialKind::Iam,
            CredentialKind::SecretsManager,
            CredentialKind::Env,
        ] {
            assert_eq!(CredentialKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(CredentialKind::parse("password"), None);
    }
}
