//! `GET /api/v1/labels`: the labels a caller may see, by slug, so the
//! console can show a person, team or catalog by its label wherever an
//! answer names it by id (display names spec, Part 3).
//!
//! Scope of `GET /labels`, for every caller (M18 issue #749: an
//! every-catalog admin included, so no answer grows with the number of
//! users or teams):
//! - the caller's own user and its teams, and the owner teams of the
//!   catalogs on its page;
//! - the catalogs are one page of those the caller reaches (M18 spec
//!   Decision 7: `?after=&limit=` and `next`, as `GET /catalogs`). Labels
//!   shared within a page are told apart; across pages they are not.
//!
//! `GET /labels/named?users=&teams=` answers the labels of the users and
//! teams a view is about to show, at most [`NAMED_MAX`] of each: every one
//! that exists for an admin (any admin may list every user and team, with
//! their labels: `GET /users`, `GET /teams`), and for anyone else only its
//! own user and its own teams, so no caller receives a label it could not
//! read before.
//!
//! A provisioner never gets here: neither route is in
//! [`PROVISIONER_ROUTES`](super::PROVISIONER_ROUTES).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Extension, Json, Router};
use serde::{Deserialize, Serialize};

use super::{reachable_page, require_reader, PageQuery};
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
    Router::new()
        .route("/labels", get(labels))
        .route("/labels/named", get(named))
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
/// read for exactly the slugs the answer holds: its own user and teams, and
/// the page's catalogs with their owner teams, whoever the caller is.
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
    let mut wanted = Wanted::default();
    for catalog in reachable {
        wanted.catalog(catalog);
    }
    let mut own_teams = Vec::new();
    if let Some(user) = principal.user.as_deref() {
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

/// The most users, and the most teams, one `GET /labels/named` may name.
pub(crate) const NAMED_MAX: usize = 100;

/// `GET /labels/named`' query: comma-separated slugs.
#[derive(Debug, Default, Deserialize)]
// An unknown parameter (`?user=`, a typo) is a 400, never an empty answer
// that would leave the console showing slugs without saying why.
#[serde(deny_unknown_fields)]
struct NamedQuery {
    /// User slugs, `alice,bob`.
    users: Option<String>,
    /// Team slugs.
    teams: Option<String>,
}

/// `GET /labels/named`' answer: the labels of the named users and teams
/// the caller may see. A slug that does not exist, or that the caller may
/// not see, is absent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub(crate) struct NamedView {
    /// User slug -> label.
    pub users: BTreeMap<String, String>,
    /// Team slug -> label.
    pub teams: BTreeMap<String, String>,
}

/// The distinct slugs of one comma-separated parameter, empty items
/// skipped.
///
/// # Arguments
/// * `list` - the parameter as sent, if any.
/// * `field` - its name, for the refusal.
///
/// # Returns
/// The slugs, sorted.
///
/// # Errors
/// [`OkfError::InvalidPath`] (400) for more than [`NAMED_MAX`] slugs.
#[tracing::instrument(level = "trace", skip_all)]
fn slugs(list: Option<&str>, field: &str) -> Result<BTreeSet<String>, OkfError> {
    let slugs: BTreeSet<String> = list
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|slug| !slug.is_empty())
        .map(str::to_string)
        .collect();
    if slugs.len() > NAMED_MAX {
        return Err(OkfError::InvalidPath {
            path: field.to_string(),
            reason: format!("name at most {NAMED_MAX} at a time"),
        });
    }
    Ok(slugs)
}

/// `GET /labels/named?users=&teams=`: the labels of the users and teams a
/// view shows (see the module docs for who sees which).
///
/// # Returns
/// The labels the caller may see, of the slugs it named.
///
/// # Errors
/// 403 for a provisioner (by the allow-list, before this runs); 400 for an
/// unknown parameter or more than [`NAMED_MAX`] slugs of one kind; 500 if
/// the labels cannot be read.
#[tracing::instrument(level = "debug", skip_all)]
async fn named(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Query(query): Query<NamedQuery>,
) -> Result<Json<NamedView>, AppError> {
    let users = slugs(query.users.as_deref(), "users")?;
    let teams = slugs(query.teams.as_deref(), "teams")?;
    let lookup = app.labels();
    // An admin may list every user and team with their labels (`GET
    // /users` and `GET /teams` take the same check), so it may see any
    // label; anyone else only its own user's and its own teams'.
    let wanted = if require_reader(&principal).is_ok() {
        Wanted {
            users,
            teams,
            ..Wanted::default()
        }
    } else {
        let mut wanted = Wanted::default();
        if let Some(user) = principal.user.as_deref() {
            if users.contains(user) {
                wanted.user(user);
            }
            if !teams.is_empty() {
                for team in lookup.teams_of(user).await? {
                    if teams.contains(&team) {
                        wanted.team(&team);
                    }
                }
            }
        }
        wanted
    };
    let labels = lookup.labels_for(&wanted).await?;
    Ok(Json(NamedView {
        users: known(wanted.users.iter().map(String::as_str), |user| {
            labels.user(user)
        }),
        teams: known(wanted.teams.iter().map(String::as_str), |team| {
            labels.team(team)
        }),
    }))
}
