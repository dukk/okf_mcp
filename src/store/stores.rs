//! The `stores` table (migration `0036_stores.sql`): where each catalog
//! store's database is, read by [`crate::stores::Stores`] when it opens a
//! store's pool.
//!
//! A row holds a *reference* to a credential, never the credential itself
//! (plan Conventions, "Privacy rules"), so a row is safe to log apart from
//! the host, which the callers keep out of any reply.

use sqlx::PgExecutor;

use crate::error::OkfError;
use crate::store::decode_column;
use crate::stores::StoreName;

/// How a store's credential is found (`stores.credential_kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn main_is_the_account_database_row() {
        let db = crate::db::test_db().await;

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
