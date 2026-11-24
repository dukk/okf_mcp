//! Transaction helpers shared by maintenance store mutations.

/// SQL for a finding's current disposition: its last appended one, else
/// `open`. The finding row must be aliased `f`. A macro rather than a
/// `const`, so `concat!` can splice it into `&'static str` queries at
/// compile time (a `const &str` cannot be concatenated that way). A
/// `macro_rules!` macro is visible only to code after it in the file, child
/// modules included, so it stays above the `mod` lines.
macro_rules! current_disposition {
    () => {
        "COALESCE((SELECT d.disposition FROM maintenance_dispositions d \
         WHERE d.catalog_id=f.catalog_id AND d.finding_id=f.id \
         ORDER BY d.id DESC LIMIT 1),'open')"
    };
}

pub mod candidates;
pub mod evidence;
pub mod reports;
pub mod runs;

#[cfg(test)]
mod report_tests;
#[cfg(test)]
mod submission_tests;

use chrono::{DateTime, Duration, Utc};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;
use sqlx::{PgConnection, Row};

use crate::error::OkfError;
use crate::maintenance::types::Actor;
use crate::okf::analysis::secrets::scan_secrets;
use crate::store::audit::{self, Action, CatalogEntry, RunCredential};
use crate::store::changesets;
use crate::stores::CatalogDb;

#[cfg_attr(
    dylint_lib = "hyp_tracing",
    allow(
        uninstrumented_fn,
        reason = "trivial conversion: builds an error value, no I/O"
    )
)]
fn refused(reason: &str) -> OkfError {
    OkfError::Refused {
        reason: reason.into(),
    }
}

#[tracing::instrument(level = "trace", skip_all)]
fn validate(actor: &Actor, key: &str) -> Result<(), OkfError> {
    if actor.key_id.trim().is_empty() || actor.actor.trim().is_empty() {
        return Err(refused(
            "maintenance actor and credential ID must not be blank",
        ));
    }
    if key.is_empty()
        || key.len() > 128
        || !key
            .bytes()
            .all(|b| b.is_ascii() && !b.is_ascii_whitespace() && !b.is_ascii_control())
    {
        return Err(refused(
            "idempotency key must be 1..=128 non-whitespace ASCII characters",
        ));
    }
    if let Some(hit) = scan_secrets("maintenance/idempotency_key", key).first() {
        return Err(OkfError::SecretDetected {
            path: "maintenance/idempotency_key".into(),
            line: hit.line,
            rule: hit.rule.clone(),
            message: hit.message.clone(),
        });
    }
    Ok(())
}

#[tracing::instrument(level = "trace", skip_all, fields(db.system.name = "postgresql", db.operation.name = "maintenance.lock", catalog_id = catalog_id))]
async fn lock(
    conn: &mut PgConnection,
    catalog_id: i64,
    now: DateTime<Utc>,
) -> Result<(), OkfError> {
    // Match publish/delete order: advisory lock, catalog identity, maintenance state,
    // run/work rows, then path-ordered source rows. Imports write before deleting.
    crate::store::catalogs::lock(conn, catalog_id).await?;
    changesets::lock_catalog(conn, catalog_id).await?;
    sqlx::query("INSERT INTO maintenance_state (catalog_id, updated_at) VALUES ($1,$2) ON CONFLICT (catalog_id) DO NOTHING")
        .bind(catalog_id).bind(now).execute(&mut *conn).await?;
    sqlx::query("SELECT catalog_id FROM maintenance_state WHERE catalog_id=$1 FOR UPDATE")
        .bind(catalog_id)
        .fetch_one(conn)
        .await?;
    Ok(())
}

/// The runs of one catalog that were running when a maintenance transaction
/// took the catalog's locks, so the transaction can tell, before it commits,
/// which of them it ended. A run that leaves `running` is recorded in the
/// audit log as `catalog.maintenance_run` (M14 spec Part 6), in the same
/// store transaction that ended it ([`RunWatch::commit`]), so the entry and
/// the end of the run commit or roll back together.
///
/// Every maintenance transaction is serialized per catalog by [`lock`], so
/// no other transaction ends a run between the watch and the commit.
pub(super) struct RunWatch {
    catalog_id: i64,
    running: Vec<String>,
    target: AuditTarget,
}

/// What a maintenance entry needs from the account database, read BEFORE
/// the store transaction begins and held until it ends: the catalog's name
/// and owner team, and the account transaction that resolves the ended
/// runs' credential ids when the entry is written.
///
/// The account connection comes first and is kept while the store
/// transaction runs, the acquisition order of plan Task 11 (issue #970,
/// `CatalogDb::begin`): the store connection never waits for an account
/// connection or row. The transaction only reads, and the store commits
/// before it ends. An early return drops both, which rolls both back.
pub(super) struct AuditTarget {
    account_tx: sqlx::Transaction<'static, sqlx::Postgres>,
    name: String,
    owner_team_id: Option<String>,
}

impl AuditTarget {
    /// Opens the account transaction and reads the catalog's name and owner
    /// team.
    ///
    /// # Errors
    /// [`OkfError::Refused`] if the catalog no longer exists;
    /// [`OkfError::Database`].
    #[tracing::instrument(level = "trace", skip_all, fields(db.system.name = "postgresql", db.operation.name = "maintenance.audit_target", catalog_id = catalog_id))]
    pub(super) async fn read(db: &CatalogDb, catalog_id: i64) -> Result<Self, OkfError> {
        let mut account_tx = db.accounts().begin().await?;
        let row: Option<(String, Option<String>)> =
            sqlx::query_as("SELECT name, owner_team_id FROM catalogs WHERE id = $1")
                .bind(catalog_id)
                .fetch_optional(&mut *account_tx)
                .await?;
        let (name, owner_team_id) = row.ok_or_else(|| refused("the catalog no longer exists"))?;
        Ok(Self {
            account_tx,
            name,
            owner_team_id,
        })
    }
}

/// A run that ended, with what its audit entry says.
#[derive(Debug, sqlx::FromRow)]
struct EndedRun {
    id: String,
    state: String,
    at: DateTime<Utc>,
    credential_id: String,
    key_id: Option<String>,
    findings: i64,
}

impl RunWatch {
    /// Notes the catalog's running runs. Call it right after [`lock`].
    ///
    /// # Errors
    /// [`OkfError::Database`].
    #[tracing::instrument(level = "trace", skip_all, fields(db.system.name = "postgresql", db.operation.name = "maintenance.watch_runs", catalog_id = catalog_id))]
    pub(super) async fn open(
        conn: &mut PgConnection,
        catalog_id: i64,
        target: AuditTarget,
    ) -> Result<Self, OkfError> {
        let running = sqlx::query_scalar(
            "SELECT id FROM maintenance_runs WHERE catalog_id=$1 AND state='running'",
        )
        .bind(catalog_id)
        .fetch_all(conn)
        .await?;
        Ok(Self {
            catalog_id,
            running,
            target,
        })
    }

    /// Records an audit entry for every run this transaction ended (and for
    /// `inserted`, a run it created already ended), in the transaction
    /// itself, then commits it and ends the account transaction.
    ///
    /// # Arguments
    /// * `tx` - the maintenance transaction; consumed.
    /// * `inserted` - the id of a run this transaction inserted, if any.
    ///
    /// # Errors
    /// [`OkfError::Database`]. An entry that cannot be written fails the
    /// whole change: nothing commits.
    #[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "maintenance.commit", catalog_id = self.catalog_id))]
    pub(super) async fn commit(
        self,
        mut tx: sqlx::Transaction<'static, sqlx::Postgres>,
        inserted: Option<&str>,
    ) -> Result<(), OkfError> {
        let Self {
            catalog_id,
            running,
            mut target,
        } = self;
        let mut watched = running;
        watched.extend(inserted.map(str::to_string));
        let ended: Vec<EndedRun> = sqlx::query_as(
            "SELECT r.id, r.state, COALESCE(r.ended_at, r.deadline) AS at, r.credential_id, \
                    r.key_id, \
                    (SELECT count(*) FROM maintenance_findings f \
                      WHERE f.catalog_id=r.catalog_id AND f.run_id=r.id) AS findings \
             FROM maintenance_runs r \
             WHERE r.catalog_id=$1 AND r.id = ANY($2) AND r.state <> 'running' \
             ORDER BY r.created_at, r.id",
        )
        .bind(catalog_id)
        .bind(&watched)
        .fetch_all(&mut *tx)
        .await?;
        for run in &ended {
            // The run's credential ids come from the account transaction
            // opened before this one began; the entry goes to the store.
            let credential = RunCredential {
                credential_id: run.credential_id.clone(),
                key_id: run.key_id.clone(),
            };
            let resolved = audit::resolve_run(
                &mut target.account_tx,
                &credential,
                target.owner_team_id.as_deref(),
            )
            .await?;
            let entry = CatalogEntry {
                action: Action::CatalogMaintenanceRun,
                catalog_id,
                catalog: &target.name,
                details: serde_json::json!({
                    "run": run.id,
                    "state": run.state,
                    "findings": run.findings,
                }),
            };
            audit::record_catalog(&mut tx, &resolved, &entry, run.at).await?;
        }
        tx.commit().await?;
        // Read-only: ending it releases the connection.
        target.account_tx.commit().await?;
        Ok(())
    }
}

#[tracing::instrument(level = "trace", skip_all, fields(db.system.name = "postgresql", db.operation.name = "maintenance.replay"))]
async fn replay<T: DeserializeOwned>(
    conn: &mut PgConnection,
    catalog: i64,
    actor: &Actor,
    operation: &str,
    key: &str,
    payload: &Value,
) -> Result<Option<T>, OkfError> {
    let row = sqlx::query("SELECT request, response FROM maintenance_requests WHERE catalog_id=$1 AND credential_id=$2 AND operation=$3 AND idempotency_key=$4")
        .bind(catalog).bind(&actor.key_id).bind(operation).bind(key).fetch_optional(conn).await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let request: Value = row.try_get("request")?;
    if request != *payload {
        return Err(refused("idempotency key reused with a different request"));
    }
    let response: Value = row.try_get("response")?;
    Ok(Some(serde_json::from_value(response).map_err(|_| {
        refused("stored maintenance response is invalid")
    })?))
}

#[tracing::instrument(level = "trace", skip_all, fields(db.system.name = "postgresql", db.operation.name = "maintenance.receipt"))]
async fn receipt<T: Serialize>(
    conn: &mut PgConnection,
    catalog: i64,
    actor: &Actor,
    operation: &str,
    key: &str,
    payload: &Value,
    response: &T,
) -> Result<(), OkfError> {
    sqlx::query("INSERT INTO maintenance_requests (catalog_id, credential_id, operation, idempotency_key, request, response) VALUES ($1,$2,$3,$4,$5,$6)")
        .bind(catalog).bind(&actor.key_id).bind(operation).bind(key).bind(payload)
        .bind(serde_json::to_value(response).map_err(|_| refused("maintenance response cannot be serialized"))?)
        .execute(conn).await?;
    Ok(())
}

#[tracing::instrument(level = "trace", skip_all, fields(db.system.name = "postgresql", db.operation.name = "maintenance.recover"))]
async fn recover(
    conn: &mut PgConnection,
    catalog: i64,
    now: DateTime<Utc>,
) -> Result<(), OkfError> {
    // Runs are locked before work, and exhausted deadlines win over expired leases.
    let expired: Vec<String> = sqlx::query_scalar("SELECT id FROM maintenance_runs WHERE catalog_id=$1 AND state='running' AND deadline <= $2 ORDER BY created_at,id FOR UPDATE")
        .bind(catalog).bind(now).fetch_all(&mut *conn).await?;
    for run in expired {
        sqlx::query("UPDATE maintenance_work SET outcome='failed', assessment='not_assessed', reason='lease_expired', ended_at=$3 WHERE catalog_id=$1 AND run_id=$2 AND outcome IS NULL")
            .bind(catalog).bind(&run).bind(now).execute(&mut *conn).await?;
        sqlx::query("UPDATE maintenance_runs SET state='failed', ended_at=$3, end_reason='budget_exhausted' WHERE catalog_id=$1 AND id=$2")
            .bind(catalog).bind(&run).bind(now).execute(&mut *conn).await?;
    }
    let _active_runs: Vec<String> = sqlx::query_scalar("SELECT id FROM maintenance_runs WHERE catalog_id=$1 AND state='running' ORDER BY created_at,id FOR UPDATE")
        .bind(catalog).fetch_all(&mut *conn).await?;
    let expiring: Vec<String> = sqlx::query_scalar("SELECT id FROM maintenance_work WHERE catalog_id=$1 AND outcome IS NULL AND lease_expires_at <= $2 ORDER BY run_id,id FOR UPDATE")
        .bind(catalog).bind(now).fetch_all(&mut *conn).await?;
    for id in expiring {
        sqlx::query("UPDATE maintenance_work SET outcome='failed', assessment='not_assessed', reason='lease_expired', ended_at=$3 WHERE catalog_id=$1 AND id=$2 AND outcome IS NULL")
            .bind(catalog).bind(&id).bind(now).execute(&mut *conn).await?;
    }
    // If a source changed, end only the old revision attempt.
    let stale: Vec<String> = sqlx::query_scalar("SELECT w.id FROM maintenance_work w LEFT JOIN documents d ON d.catalog_id=w.catalog_id AND d.path=w.path WHERE w.catalog_id=$1 AND w.outcome IS NULL AND (d.revision IS DISTINCT FROM w.revision OR d.deleted_at IS NOT NULL) ORDER BY w.run_id,w.id FOR UPDATE OF w")
        .bind(catalog).fetch_all(&mut *conn).await?;
    for id in stale {
        sqlx::query("UPDATE maintenance_work SET outcome='superseded', reason='source_changed', ended_at=$3 WHERE catalog_id=$1 AND id=$2 AND outcome IS NULL")
            .bind(catalog).bind(&id).bind(now).execute(&mut *conn).await?;
    }
    sqlx::query("UPDATE maintenance_runs r SET state='completed', ended_at=$2 WHERE r.catalog_id=$1 AND r.state='running' AND NOT EXISTS (SELECT 1 FROM maintenance_work w WHERE w.catalog_id=$1 AND w.run_id=r.id AND w.outcome IS NULL)")
        .bind(catalog).bind(now).execute(conn).await?;
    Ok(())
}

#[cfg_attr(
    dylint_lib = "hyp_tracing",
    allow(
        uninstrumented_fn,
        reason = "trivial conversion: pure arithmetic or formatting on its arguments, no I/O"
    )
)]
fn lease_end(now: DateTime<Utc>, deadline: DateTime<Utc>) -> DateTime<Utc> {
    (now + Duration::seconds(900)).min(deadline)
}

#[cfg(test)]
mod final_tests;
#[cfg(test)]
mod nul_tests;
#[cfg(test)]
mod tests;
