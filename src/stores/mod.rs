//! The two kinds of database handle: [`AccountDb`] (users, teams, keys,
//! sessions, the catalog registry) and [`CatalogDb`] (one store's catalog
//! data), and [`StoreName`], a store's name.
//!
//! Both handles wrap a `PgPool`. They are separate types so a function that
//! takes one cannot be handed the other: the compiler, not a code review,
//! catches a query sent to the wrong database. Release 1 of the catalog
//! stores milestone only introduces the types; in it both reach the same
//! database. (A Python reader: this is the "newtype" pattern, two classes
//! that wrap the same object so the type checker can tell them apart.)

use sqlx::postgres::PgPool;
use sqlx::{Postgres, Transaction};

mod credentials;
pub mod placement;
mod registry;

pub(crate) use credentials::{is_auth_failure, signal_auth_refused};
pub use registry::{PoolSettings, Stores, STORE_SEARCH_PATH};

use crate::error::OkfError;

/// The name of the store every existing catalog is on after migration 0036.
const MAIN_STORE: &str = "main";

/// The longest store name accepted: a bound so a name fits a metric label
/// and a log line.
const MAX_STORE_NAME_LEN: usize = 63;

/// A store's name, as on its `stores` row: lowercase letters, digits and
/// hyphens (`^[a-z0-9-]+$`), 1 to 63 characters.
// The field is private (no `pub`), so the only way to make one is `parse`
// or `main`: a `StoreName` in hand is always valid.
// `try_from = "String"` makes serde read a string and then call `TryFrom<String>`
// (below), so deserializing goes through `parse` like any other constructor.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "String")]
pub struct StoreName(String);

impl StoreName {
    /// Checks a store name.
    ///
    /// # Arguments
    /// * `name` - the text to check.
    ///
    /// # Returns
    /// The name, if it matches `^[a-z0-9-]+$` and is at most 63 characters.
    ///
    /// # Errors
    /// [`OkfError::Refused`] naming the rule (never the rejected text, which
    /// is caller input) when it does not.
    #[tracing::instrument(level = "trace", skip_all)]
    pub fn parse(name: &str) -> Result<Self, OkfError> {
        let valid = !name.is_empty()
            && name.len() <= MAX_STORE_NAME_LEN
            && name
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
        if valid {
            Ok(Self(name.to_string()))
        } else {
            Err(OkfError::Refused {
                reason: format!(
                    "a store name is 1 to {MAX_STORE_NAME_LEN} characters: lowercase letters, \
                     digits and hyphens"
                ),
            })
        }
    }

    /// The name as text.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(uninstrumented_fn, reason = "one-line accessor")
    )]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// `main`, the store every existing catalog is on after migration 0036.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(uninstrumented_fn, reason = "returns a constant")
    )]
    pub fn main() -> Self {
        Self(MAIN_STORE.to_string())
    }
}

impl TryFrom<String> for StoreName {
    type Error = OkfError;

    /// [`StoreName::parse`] for an owned string, so deserialization validates.
    ///
    /// # Arguments
    /// * `name` - the text to check.
    ///
    /// # Returns
    /// The name, if valid.
    ///
    /// # Errors
    /// [`OkfError::Refused`], as [`StoreName::parse`].
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(uninstrumented_fn, reason = "delegates to the instrumented `parse`")
    )]
    fn try_from(name: String) -> Result<Self, Self::Error> {
        Self::parse(&name)
    }
}

impl std::fmt::Display for StoreName {
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "trivial accessor or conversion: returns or maps a value, a span would record nothing new"
        )
    )]
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// The account database. Cloning is cheap (the pool is reference-counted).
#[derive(Debug, Clone)]
pub struct AccountDb {
    pool: PgPool,
}

impl AccountDb {
    /// Wraps a pool as the account database.
    ///
    /// # Arguments
    /// * `pool` - a pool on the account database.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(uninstrumented_fn, reason = "one-line constructor")
    )]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Begins a transaction on the account database.
    ///
    /// # Returns
    /// The transaction, which owns its connection (`'static`).
    ///
    /// # Errors
    /// [`OkfError::Database`] (or `Busy`) when no connection is available.
    #[tracing::instrument(level = "trace", skip_all)]
    pub async fn begin(&self) -> Result<Transaction<'static, Postgres>, OkfError> {
        Ok(self.pool.begin().await?)
    }

    /// The pool, for store-layer functions that take an executor. Routes and
    /// tools pass the [`AccountDb`] itself, not this.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(uninstrumented_fn, reason = "one-line accessor")
    )]
    pub fn executor(&self) -> &PgPool {
        &self.pool
    }

    /// The account database over a test pool: the one-line handle the
    /// tests build around the pool they hold (the account side of
    /// [`CatalogDb::main_of`]).
    #[cfg(test)]
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(uninstrumented_fn, reason = "one-line test constructor")
    )]
    pub(crate) fn of(pool: &PgPool) -> Self {
        Self::new(pool.clone())
    }
}

/// One store's database: the catalog data of the catalogs placed on it.
#[derive(Debug, Clone)]
pub struct CatalogDb {
    store: StoreName,
    pool: PgPool,
}

impl CatalogDb {
    /// Wraps a pool as a store's database.
    ///
    /// # Arguments
    /// * `store` - the store's name.
    /// * `pool` - a pool on the store's database.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(uninstrumented_fn, reason = "one-line constructor")
    )]
    pub fn new(store: StoreName, pool: PgPool) -> Self {
        Self { store, pool }
    }

    /// The store's name.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(uninstrumented_fn, reason = "one-line accessor")
    )]
    pub fn store(&self) -> &StoreName {
        &self.store
    }

    /// Begins a transaction on the store's database.
    ///
    /// # Returns
    /// The transaction, which owns its connection (`'static`).
    ///
    /// # Errors
    /// [`OkfError::StoreUnavailable`] when no connection can be had within
    /// `store_connect_timeout` (the pool is exhausted, or the store cannot be
    /// reached to open another, or refuses the login); [`OkfError::Database`] or `Busy` otherwise.
    #[tracing::instrument(level = "trace", skip_all)]
    pub async fn begin(&self) -> Result<Transaction<'static, Postgres>, OkfError> {
        self.pool
            .begin()
            .await
            .map_err(|error| self.classify(error))
    }

    /// Reads a driver error from this store's pool: a failure to get a
    /// connection, or a refused login, is [`OkfError::StoreUnavailable`] (which never names the
    /// store to the client), anything else the usual conversion. Account
    /// database errors never pass through here.
    ///
    /// # Arguments
    /// * `error` - what the driver returned for an operation on this store.
    ///
    /// # Returns
    /// The error to hand back.
    #[tracing::instrument(level = "trace", skip_all)]
    pub fn classify(&self, error: sqlx::Error) -> OkfError {
        // A refused login is the store being unreachable to this server (a
        // rotated secret, an expired token), not a fault in the request;
        // the credential tasks are told to fetch a fresh login.
        if credentials::is_auth_failure(&error) {
            credentials::signal_auth_refused(&error);
            return OkfError::store_unavailable(&self.store);
        }
        match error {
            sqlx::Error::PoolTimedOut
            | sqlx::Error::PoolClosed
            | sqlx::Error::Io(_)
            | sqlx::Error::Tls(_)
            | sqlx::Error::WorkerCrashed => OkfError::store_unavailable(&self.store),
            other => OkfError::from(other),
        }
    }

    /// The pool, for store-layer functions that take an executor. Routes and
    /// tools pass the [`CatalogDb`] itself, not this.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(uninstrumented_fn, reason = "one-line accessor")
    )]
    pub fn executor(&self) -> &PgPool {
        &self.pool
    }

    /// The release-1 `main` store over a test pool: the one-line handle the
    /// tests build around the pool they hold.
    #[cfg(test)]
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(uninstrumented_fn, reason = "one-line test constructor")
    )]
    pub(crate) fn main_of(pool: &PgPool) -> Self {
        Self::new(StoreName::main(), pool.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn classify_turns_connection_failures_into_store_unavailable() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://u:p@127.0.0.1:1/x")
            .unwrap();
        let db = CatalogDb::new(StoreName::parse("shared-1").unwrap(), pool);

        for failure in [
            sqlx::Error::PoolTimedOut,
            sqlx::Error::PoolClosed,
            sqlx::Error::WorkerCrashed,
            sqlx::Error::Io(std::io::Error::other("connection refused")),
        ] {
            let error = db.classify(failure);
            assert!(
                matches!(error, OkfError::StoreUnavailable { .. }),
                "{error:?}"
            );
        }
        let other = db.classify(sqlx::Error::RowNotFound);
        assert!(matches!(other, OkfError::Database { .. }), "{other:?}");
    }

    #[test]
    fn store_name_parse_accepts_slugs_and_refuses_the_rest() {
        for good in [
            "main",
            "shared-1",
            "acme",
            "0",
            "a-b-c",
            "-",
            &"a".repeat(63),
        ] {
            assert_eq!(StoreName::parse(good).unwrap().as_str(), good);
        }
        let too_long = "a".repeat(64);
        for bad in [
            "",
            "Main",
            "shared_1",
            "a b",
            "a.b",
            "a/b",
            "caf\u{e9}",
            "nul\0",
            &too_long,
        ] {
            let error = StoreName::parse(bad).unwrap_err();
            assert_eq!(error.kind(), "Refused", "{bad:?}");
            // The rejected text is caller input: the message never echoes it.
            if !bad.is_empty() {
                assert!(!error.to_string().contains(bad), "{bad:?}");
            }
        }
        assert_eq!(StoreName::main().as_str(), "main");
        assert_eq!(StoreName::main().to_string(), "main");
    }

    #[test]
    fn store_name_deserialize_validates_like_parse() {
        let too_long = format!("\"{}\"", "a".repeat(64));
        for bad in [
            r#""Main/invalid""#,
            r#""""#,
            r#""shared_1""#,
            too_long.as_str(),
            "5",
            "null",
        ] {
            assert!(serde_json::from_str::<StoreName>(bad).is_err(), "{bad}");
        }
        // The refusal is the fixed message: it does not echo the input.
        let error = serde_json::from_str::<StoreName>(r#""Main/invalid""#).unwrap_err();
        assert!(!error.to_string().contains("Main/invalid"));

        let name = StoreName::parse("shared-1").unwrap();
        let json = serde_json::to_string(&name).unwrap();
        assert_eq!(json, r#""shared-1""#);
        assert_eq!(serde_json::from_str::<StoreName>(&json).unwrap(), name);
    }

    #[tokio::test]
    async fn main_store_and_accounts_reach_the_same_database_in_release_one() {
        let db = crate::db::test_db().await;
        let app = crate::app::App::load(
            db.pool.clone(),
            crate::config::Config::default(),
            std::sync::Arc::new(crate::clock::SystemClock),
        )
        .await
        .unwrap();

        let from_accounts: String = sqlx::query_scalar("SELECT current_database()")
            .fetch_one(app.accounts().executor())
            .await
            .unwrap();
        let from_store: String = sqlx::query_scalar("SELECT current_database()")
            .fetch_one(app.main_db().await.executor())
            .await
            .unwrap();
        assert_eq!(from_accounts, from_store);
        assert_eq!(app.main_db().await.store(), &StoreName::main());
    }
}
