//! `/api/v1/audit` (spec Part 5, "Who reads it, and how"): reading and
//! exporting the audit log. An every-catalog admin reads everything; a
//! team's owner, through a console session and naming the team, reads that
//! team's entries with operator staff redacted; a provisioner reads only
//! the entries it wrote (and may not export: the provisioner gate allows
//! `GET /audit` only). The redaction lives in [`audit::list`], so the CLI
//! (Task 8b) and these routes cannot drift apart.
//!
//! The log is read from the account database and every store, merged by
//! time ([`audit::list`]). An owner's view carries no entry id (issue #430):
//! its records have no `id`, and its `next_after` is a sealed cursor
//! ([`cursor`]), the only `after` it accepts. Admins and provisioners page
//! by the readable cursor of [`audit::AuditCursor::to_plain`], and their
//! records carry the `store` an entry is on.

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
use crate::store::audit::{self, Action, AuditCursor, AuditQuery, AuditRecord, FilterText, Viewer};
use crate::store::{server_keys, teams};

mod cursor;

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
    /// A readable cursor for an admin or provisioner, a sealed one for an
    /// owner ([`AuditParams::cursor`] tells them apart once the viewer is
    /// known).
    after: Option<String>,
    limit: Option<i64>,
}

impl AuditParams {
    /// The filters, checked, without the cursor ([`AuditParams::cursor`]
    /// adds it once the viewer is known).
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
            after: None,
            limit: self.limit,
        })
    }

    /// The `after` cursor as the position it stands for: for an admin or a
    /// provisioner the readable cursor as sent; for an owner the position
    /// sealed in the cursor an earlier page of the same team gave it
    /// ([`cursor::open`]).
    /// An empty value is no cursor.
    ///
    /// # Arguments
    /// * `app` - the application (the cursor key's database).
    /// * `viewer` - who reads.
    ///
    /// # Returns
    /// The position, or `None` for no cursor.
    ///
    /// # Errors
    /// 400 ([`OkfError::InvalidPath`]) for an admin's or provisioner's
    /// value that is not a cursor, and for an owner's that is not one
    /// issued for that team under the current key (a plain one included,
    /// so an owner cannot probe the sequences);
    /// [`OkfError::Database`] reading the key.
    #[tracing::instrument(level = "debug", skip_all)]
    async fn cursor(&self, app: &App, viewer: &Viewer) -> Result<Option<AuditCursor>, OkfError> {
        let Some(after) = self.after.as_deref().filter(|after| !after.is_empty()) else {
            return Ok(None);
        };
        match viewer {
            Viewer::Owner { audit_team, .. } => {
                let key = server_keys::get_or_create(
                    app.accounts().executor(),
                    server_keys::AUDIT_CURSOR,
                )
                .await?;
                cursor::open(&key, *audit_team, after).map(Some)
            }
            Viewer::Admin | Viewer::Credential(_) => AuditCursor::parse_plain(after).map(Some),
        }
    }
}

/// A page's `next_after`: the readable cursor for an admin or a
/// provisioner, a sealed one for an owner. Both are strings the client
/// passes back as `after` and treats as opaque.
// `untagged` serializes each variant as its bare value.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub(crate) enum NextAfter {
    /// The last entry's position ([`AuditCursor::to_plain`]).
    Plain(String),
    /// The last entry's position and the owner's team, sealed
    /// ([`cursor::seal`]).
    Sealed(String),
}

/// `GET /audit`'s answer.
#[derive(Debug, Serialize)]
pub(crate) struct AuditPage {
    /// Newest first.
    pub entries: Vec<AuditRecord>,
    /// The cursor for the next page (`?after=`), or null when this page is
    /// not full. A full last page is followed by an empty one.
    pub next_after: Option<NextAfter>,
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
    let (team_id, audit_team) = teams::owned_by(app.accounts().executor(), user_id, team)
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
    let mut query = params.query()?;
    let viewer = viewer(&app, &principal, query.team).await?;
    query.after = params.cursor(&app, &viewer).await?;
    let entries = audit::list(app.stores(), &viewer, &query, app.labels()).await?;
    // A full page may have more after it. `try_from` converts the length,
    // a `usize`, to the `i64` the page size is.
    let full = i64::try_from(entries.len()).is_ok_and(|len| len >= query.page_size());
    let next_after = match (entries.last().filter(|_| full), &viewer) {
        (None, _) => None,
        (Some(last), Viewer::Owner { audit_team, .. }) => {
            let key =
                server_keys::get_or_create(app.accounts().executor(), server_keys::AUDIT_CURSOR)
                    .await?;
            Some(NextAfter::Sealed(cursor::seal(
                &key,
                *audit_team,
                &last.cursor,
            )?))
        }
        (Some(last), Viewer::Admin | Viewer::Credential(_)) => {
            Some(NextAfter::Plain(last.cursor.to_plain()))
        }
    };
    Ok(Json(AuditPage {
        entries,
        next_after,
        actions: Action::ALL.iter().map(|action| action.as_str()).collect(),
    }))
}

/// `GET /audit/export`: every entry the filters select, as JSON Lines (one
/// record per line, newest first), for a compliance request, built by
/// [`audit::export_lines`] (which `okf-mcp audit export` prints): `after`
/// is a starting cursor (an owner's sealed one, as `GET /audit` gave it)
/// and `limit` is ignored, since the export pages through to the end. An
/// owner's lines have no `id`. Retention bounds the total.
///
/// # Errors
/// As [`list_audit`].
#[tracing::instrument(level = "debug", skip_all)]
async fn export_audit(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Query(params): Query<AuditParams>,
) -> Result<Response, AppError> {
    let mut query = params.query()?;
    let viewer = viewer(&app, &principal, query.team).await?;
    query.after = params.cursor(&app, &viewer).await?;
    let body = audit::export_lines(app.stores(), &viewer, &query, app.labels()).await?;
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
