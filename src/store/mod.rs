//! The catalog store: async functions over a Postgres pool, one module per
//! group of tables (design "Storage"). Every function that writes takes `now`
//! from the caller, so every stored timestamp comes from the server clock.

use crate::error::OkfError;

pub mod audit;
pub mod catalogs;
pub mod ceiling;
pub mod changesets;
pub mod cleanups;
pub mod documents;
pub mod email_tokens;
pub mod export;
pub mod gates;
pub mod history;
pub mod identities;
pub mod import;
#[cfg(test)]
mod invalidation_tests;
pub mod keys;
pub mod maintenance;
pub mod oauth;
pub mod passkeys;
pub mod passwords;
pub mod publish;
pub mod repair;
pub mod server_keys;
pub mod settings;
pub mod sign_in_events;
pub mod sign_in_methods;
pub mod stores;
pub mod teams;
pub mod unassigned;
pub mod unlocks;
pub mod user_sessions;
pub mod users;

/// Turns the result of parsing a stored column into the column's value, or
/// into [`OkfError::Database`] when the stored text is not a known spelling.
///
/// Every such column has a CHECK constraint, so a failure means a
/// hand-edited or corrupted row. The reader answers an error instead of
/// panicking. The error names the column only, never the stored text, so a
/// bad row's contents do not travel in the error.
///
/// # Arguments
/// * `parsed` - the result of the type's `parse` on the stored text.
/// * `column` - the column as `table.column`, for the error text.
///
/// # Returns
/// The parsed value.
///
/// # Errors
/// [`OkfError::Database`] (a `ColumnDecode`) when `parsed` is `None`.
// `Option<T>` is Rust's "maybe a value"; `ok_or_else` turns `None` into an
// `Err` built only when needed, like Python's `x if x is not None else raise`.
#[cfg_attr(
    dylint_lib = "hyp_tracing",
    allow(
        uninstrumented_fn,
        reason = "trivial accessor or conversion: maps a value, no I/O"
    )
)]
pub(crate) fn decode_column<T>(parsed: Option<T>, column: &'static str) -> Result<T, OkfError> {
    parsed.ok_or_else(|| OkfError::Database {
        source: sqlx::Error::ColumnDecode {
            index: column.to_string(),
            source: format!("unrecognised value in {column}").into(),
        },
    })
}

/// Drops every CHECK constraint on `table` in a throwaway test database, so
/// a test can store a value the schema would refuse (a hand-edited row) and
/// see that the reader answers an error instead of panicking (issue #642).
///
/// # Arguments
/// * `pool` - the throwaway database.
/// * `table` - the table's name.
#[cfg(test)]
pub(crate) async fn drop_check_constraints(pool: &sqlx::PgPool, table: &str) {
    // `contype = 'c'` is a CHECK; `conrelid` names the table.
    let names: Vec<String> = sqlx::query_scalar(
        "SELECT conname FROM pg_constraint WHERE contype = 'c' AND conrelid = $1::regclass",
    )
    .bind(table)
    .fetch_all(pool)
    .await
    .expect("list the CHECK constraints");
    for name in names {
        // Identifiers cannot be bound, so quote it into the statement.
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "ALTER TABLE {table} DROP CONSTRAINT \"{name}\""
        )))
        .execute(pool)
        .await
        .expect("drop a CHECK constraint");
    }
}

/// Asserts `result` is the error [`decode_column`] answers for `column`:
/// [`OkfError::Database`], with the column named and the stored text absent.
#[cfg(test)]
pub(crate) fn assert_decode_error<T: std::fmt::Debug>(
    result: Result<T, OkfError>,
    column: &str,
    stored: &str,
) {
    let error = result.expect_err("a corrupt row is an error");
    assert!(matches!(error, OkfError::Database { .. }), "got {error:?}");
    let text = error.to_string();
    assert!(text.contains(column), "{text}");
    assert!(!text.contains(stored), "the stored text leaked: {text}");
}
