//! `/api/v1/audit` (spec Part 5, "Who reads it, and how"): reading and
//! exporting the audit log. An every-catalog admin reads everything; a
//! team's owner, through a console session and naming the team, reads that
//! team's entries with operator staff redacted; a provisioner reads only
//! the entries it wrote (and may not export: the provisioner gate allows
//! `GET /audit` only). The redaction lives in [`audit::list`], so the CLI
//! (Task 8b) and these routes cannot drift apart.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Extension, Json, Router};
use serde::{Deserialize, Serialize};

use crate::app::App;
use crate::auth::{CredentialKind, Principal};
use crate::error::OkfError;
use crate::http::api::require_every_catalog;
use crate::http::AppError;
use crate::store::audit::{self, Action, AuditQuery, AuditRecord, FilterText, Viewer};
use crate::store::teams;

/// `GET /audit` and `/audit/export`'s query string.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct AuditParams {
    team: Option<String>,
    action: Option<String>,
    actor: Option<String>,
    target: Option<String>,
    since: Option<String>,
    until: Option<String>,
    after: Option<i64>,
    limit: Option<i64>,
}

impl AuditParams {
    /// The filters, checked.
    ///
    /// # Errors
    /// As [`audit::parse_filters`]: 400 for an unknown action or a time
    /// that is not RFC 3339.
    #[tracing::instrument(level = "trace", skip_all)]
    fn query(&self) -> Result<AuditQuery<'_>, OkfError> {
        audit::parse_filters(&FilterText {
            team: self.team.as_deref(),
            action: self.action.as_deref(),
            actor: self.actor.as_deref(),
            target: self.target.as_deref(),
            since: self.since.as_deref(),
            until: self.until.as_deref(),
            after: self.after,
            limit: self.limit,
        })
    }
}

/// `GET /audit`'s answer.
#[derive(Debug, Serialize)]
pub(crate) struct AuditPage {
    /// Newest first.
    pub entries: Vec<AuditRecord>,
    /// The cursor for the next page (`?after=`), or null when this page is
    /// not full. A full last page is followed by an empty one.
    pub next_after: Option<i64>,
    /// Every action name, for the console's filter.
    pub actions: Vec<&'static str>,
}

/// This area's routes.
#[cfg_attr(
    dylint_lib = "hyp_tracing",
    allow(
        uninstrumented_fn,
        reason = "router builder: runs once at startup, outside any request, so a span would be an orphan root"
    )
)]
pub(crate) fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/audit", get(list_audit))
        .route("/audit/export", get(export_audit))
}

/// Who may read, as what: a provisioner its own entries; an every-catalog
/// admin everything; a team's owner, through a console session and naming
/// the team, that team's entries.
///
/// # Arguments
/// * `app` - the application.
/// * `principal` - the caller.
/// * `team` - the `team` filter, if any.
///
/// # Returns
/// The viewer [`audit::list`] redacts for.
///
/// # Errors
/// [`OkfError::Forbidden`] for anyone else (a member who is not an owner,
/// an owner's key, a team the session's user does not own, a malformed
/// team name); [`OkfError::Database`].
#[tracing::instrument(level = "debug", skip_all)]
async fn viewer(app: &App, principal: &Principal, team: Option<&str>) -> Result<Viewer, OkfError> {
    if principal.provisioner {
        return Ok(Viewer::Credential(principal.key_id.clone()));
    }
    if require_every_catalog(principal).is_ok() {
        return Ok(Viewer::Admin);
    }
    let refused = || OkfError::Forbidden {
        reason: "the audit log needs an every-catalog admin, or a team owner's console \
                 session naming the team (?team=)"
            .to_string(),
    };
    // `let ... else` binds both or returns: a session and a named team.
    let (CredentialKind::Session { user_id }, Some(team)) = (&principal.kind, team) else {
        return Err(refused());
    };
    let (team_id, audit_team) = teams::owned_by(app.pool(), user_id, team)
        .await?
        .ok_or_else(refused)?;
    Ok(Viewer::Owner {
        team_id,
        audit_team,
    })
}

/// `GET /audit`: entries newest first, cursor-paged.
///
/// # Errors
/// 400 for a bad filter; 403 for a caller that may not read (see
/// [`viewer`]); 500 for a database failure.
#[tracing::instrument(level = "debug", skip_all)]
async fn list_audit(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Query(params): Query<AuditParams>,
) -> Result<Json<AuditPage>, AppError> {
    let query = params.query()?;
    let viewer = viewer(&app, &principal, query.team).await?;
    let entries = audit::list(app.pool(), &viewer, &query, &app.labels()).await?;
    // A full page may have more after it. `try_from` converts the length,
    // a `usize`, to the `i64` the page size is.
    let full = i64::try_from(entries.len()).is_ok_and(|len| len >= query.page_size());
    let next_after = entries.last().filter(|_| full).map(|entry| entry.id);
    Ok(Json(AuditPage {
        entries,
        next_after,
        actions: Action::ALL.iter().map(|action| action.as_str()).collect(),
    }))
}

/// `GET /audit/export`: every entry the filters select, as JSON Lines (one
/// record per line, newest first), for a compliance request, built by
/// [`audit::export_lines`] (which `okf-mcp audit export` prints): `after`
/// is a starting cursor and `limit` is ignored, since the export pages
/// through to the end. Retention bounds the total.
///
/// # Errors
/// As [`list_audit`].
#[tracing::instrument(level = "debug", skip_all)]
async fn export_audit(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Query(params): Query<AuditParams>,
) -> Result<Response, AppError> {
    let query = params.query()?;
    let viewer = viewer(&app, &principal, query.team).await?;
    let body = audit::export_lines(app.pool(), &viewer, &query, &app.labels()).await?;
    let headers = [
        (header::CONTENT_TYPE, "application/x-ndjson"),
        (
            header::CONTENT_DISPOSITION,
            "attachment; filename=\"audit.jsonl\"",
        ),
    ];
    Ok((headers, body).into_response())
}

#[cfg(test)]
mod tests;
