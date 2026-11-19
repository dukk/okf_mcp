//! `GET /api/v1/labels`: the labels a caller may see, by slug, so the
//! console can show a person, team or catalog by its label wherever an
//! answer names it by id (display names spec, Part 3).
//!
//! Scope:
//! - an admin that reaches every catalog sees every user and team;
//! - anyone else sees its own user, its teams, and the owner teams of the
//!   catalogs on its page;
//! - the catalogs are one page of those the caller reaches (M18 spec
//!   Decision 7: `?after=&limit=` and `next`, as `GET /catalogs`), every
//!   catalog for an admin that reaches every catalog. Labels shared within
//!   a page are told apart; across pages they are not;
//! - a provisioner never gets here: the route is not in
//!   [`PROVISIONER_ROUTES`](super::PROVISIONER_ROUTES).

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Extension, Json, Router};
use serde::Serialize;

use super::{reachable_page, require_every_catalog, PageQuery};
use crate::app::App;
use crate::auth::Principal;
use crate::error::OkfError;
use crate::http::AppError;
use crate::labels::directory::Wanted;

/// This area's routes.
#[cfg_attr(
    dylint_lib = "hyp_tracing",
    allow(
        uninstrumented_fn,
        reason = "router builder: runs once at startup, outside any request, so a span would be an orphan root"
    )
)]
pub(crate) fn routes() -> Router<Arc<App>> {
    Router::new().route("/labels", get(labels))
}

/// `GET /labels`' answer: the labels the caller may see, by slug.
// `BTreeMap` iterates in key order, so the answer is stable.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub(crate) struct LabelsView {
    /// User slug -> label.
    pub users: BTreeMap<String, String>,
    /// Team slug -> label.
    pub teams: BTreeMap<String, String>,
    /// Catalog slug -> label for one page of the catalogs the caller
    /// reaches, told apart where two on the page share one.
    pub catalogs: BTreeMap<String, String>,
    /// The `after` of the next page of `catalogs`; absent on the last page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
}

/// The labels of `slugs`, as `(slug, label)` pairs, for a team or a user
/// that may not exist (deleted since: skipped).
#[tracing::instrument(level = "trace", skip_all)]
fn known<'a, 'l>(
    slugs: impl IntoIterator<Item = &'a str>,
    label_of: impl Fn(&str) -> Option<&'l str>,
) -> BTreeMap<String, String> {
    slugs
        .into_iter()
        .filter_map(|slug| label_of(slug).map(|label| (slug.to_string(), label.to_string())))
        .collect()
}

/// The labels `principal` may see (see the module docs for the scope),
/// read for exactly the slugs the answer holds.
///
/// # Arguments
/// * `app` - the application, for its label lookup.
/// * `reachable` - the page of catalogs `principal` reaches, which only the
///   caller can name.
/// * `principal` - the caller.
///
/// # Returns
/// The view.
///
/// # Errors
/// [`OkfError::Database`].
#[tracing::instrument(level = "debug", skip_all)]
async fn scoped(
    app: &App,
    reachable: &[&str],
    principal: &Principal,
) -> Result<LabelsView, OkfError> {
    let lookup = app.labels();
    let every = require_every_catalog(principal).is_ok();
    let mut wanted = Wanted::default();
    for catalog in reachable {
        wanted.catalog(catalog);
    }
    // An every-catalog admin sees every user and team: those are read whole
    // below, not named here. Anyone else sees itself and its teams.
    let mut own_teams = Vec::new();
    if let (false, Some(user)) = (every, principal.user.as_deref()) {
        wanted.user(user);
        own_teams = lookup.teams_of(user).await?;
        for team in &own_teams {
            wanted.team(team);
        }
    }
    // Includes the owner teams of the catalogs on the page.
    let labels = lookup.labels_with_owners(&wanted).await?;
    let catalogs = labels
        .catalog_choices(reachable.iter().copied())
        .into_iter()
        .collect();
    if every {
        let everyone = lookup.everyone().await?;
        return Ok(LabelsView {
            users: everyone
                .users()
                .map(|(slug, label)| (slug.to_string(), label.to_string()))
                .collect(),
            teams: everyone
                .teams()
                .map(|(slug, label)| (slug.to_string(), label.to_string()))
                .collect(),
            catalogs,
            next: None,
        });
    }
    let owner_teams = reachable
        .iter()
        .filter_map(|catalog| labels.catalog_owner(catalog));
    Ok(LabelsView {
        users: known(principal.user.as_deref(), |user| labels.user(user)),
        teams: known(
            own_teams.iter().map(String::as_str).chain(owner_teams),
            |team| labels.team(team),
        ),
        catalogs,
        next: None,
    })
}

/// `GET /labels?after=&limit=`.
///
/// # Returns
/// The labels the caller may see, with `next` while more of its catalogs
/// follow.
///
/// # Errors
/// 403 for a provisioner (by the allow-list, before this runs); 400 for a
/// malformed `after`; 500 if the catalog directory cannot be read.
#[tracing::instrument(level = "debug", skip_all)]
async fn labels(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Query(query): Query<PageQuery>,
) -> Result<Json<LabelsView>, AppError> {
    let page = reachable_page(&app, &principal, &query).await?;
    let reachable: Vec<&str> = page.items.iter().map(|row| row.name.as_str()).collect();
    Ok(Json(LabelsView {
        next: page.next,
        ..scoped(&app, &reachable, &principal).await?
    }))
}
