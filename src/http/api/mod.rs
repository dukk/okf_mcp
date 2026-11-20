//! The JSON REST API under `/api/v1` (web console design, "API"). Handlers
//! are thin adapters: they build the same [`ToolContext`] the MCP tools build
//! and call the same functions, or the `store` functions the CLI calls, so an
//! endpoint behaves exactly like its tool or command.

use std::sync::Arc;

use crate::http::AppError;
use axum::extract::{MatchedPath, Request};
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::middleware;
use axum::middleware::Next;
use axum::response::IntoResponse;
use axum::response::Response;
use axum::Router;
use serde::Deserialize;

use crate::app::App;
use crate::auth::{Principal, Reach};
use crate::cache::directory::{page_size, Page};
use crate::error::OkfError;
use crate::http::console_session::require_principal;
use crate::http::requester::Requester;
use crate::mcp::context::ToolContext;
use crate::store::catalogs::CatalogRow;
use crate::store::keys::Access;

#[cfg(test)]
mod access_tests;
mod audit;
mod browse;
mod catalogs;
mod changesets;
pub(crate) mod feedback;
mod health;
mod identity;
mod keys;
mod labels;
#[cfg(test)]
mod labels_tests;
#[cfg(test)]
mod listing_tests;
mod mail;
mod maintenance;
#[cfg(test)]
mod malformed_name_tests;
mod me;
mod oauth;
mod people;
#[cfg(test)]
mod provisioner_tests;
mod search;
mod settings;
#[cfg(test)]
pub(crate) mod testing;
mod unlocks;

/// Every API route, for nesting at `/api/v1`, behind `require_principal`
/// (a bearer key or grant, or a console session cookie).
///
/// # Arguments
/// * `app` - the application (`require_principal`'s state; the import
///   route's body limit comes from its config).
///
/// # Returns
/// A router whose responses all carry `Cache-Control: no-store` and
/// `nosniff`, the 401 for a missing or unknown credential included.
#[cfg_attr(
    dylint_lib = "hyp_tracing",
    allow(
        uninstrumented_fn,
        reason = "router builder: runs once at startup, outside any request, so a span would be an orphan root"
    )
)]
pub(crate) fn routes(app: &Arc<App>) -> Router<Arc<App>> {
    Router::new()
        .merge(identity::routes())
        .merge(keys::routes())
        .merge(labels::routes())
        .merge(health::routes())
        .merge(browse::routes())
        .merge(search::routes())
        .merge(catalogs::routes(app))
        .merge(maintenance::routes())
        .merge(oauth::routes())
        .merge(changesets::routes())
        .merge(people::routes())
        .merge(settings::routes())
        .merge(mail::routes())
        .merge(me::routes())
        .merge(feedback::routes())
        .merge(audit::routes())
        .merge(unlocks::routes())
        .merge(crate::http::passkeys::api_routes())
        // `POST /me/identities/{provider}/link` (Task 9); it answers 404
        // itself without providers.
        .merge(crate::http::oidc::api_routes())
        // `route_layer` covers the routes merged above and runs only when one
        // matched, so an unknown API path stays 404 rather than 401.
        .route_layer(middleware::from_fn(provisioner_gate))
        .route_layer(middleware::from_fn_with_state(
            Arc::clone(app),
            require_principal,
        ))
        // Its own fallback, so an unknown API path is answered here (404,
        // without a key check) and gets the headers below; without one, the
        // outer router's fallback would answer it.
        .fallback(|| async { StatusCode::NOT_FOUND })
        // A plain `layer` wraps everything above, `require_principal` and
        // the fallback included, so the 401 and the 404 get these headers too.
        .layer(middleware::map_response(api_headers))
}

/// Runs the handler, collecting the invalidations its writes queued, and
/// applies them to this replica's directory and credential caches before the response
/// goes out ([`App::apply_own_writes`]): in-process cache drops only, so
/// nothing after the commit waits on I/O.
///
/// # Arguments
/// * `app` - the application.
/// * `request` - the request.
/// * `next` - the handler.
///
/// # Returns
/// The handler's response, unchanged.
#[cfg_attr(
    dylint_lib = "hyp_tracing",
    allow(
        uninstrumented_fn,
        reason = "wraps every HTTP request, an unmatched path included, which must make no span"
    )
)]
pub(crate) async fn apply_own_writes(
    axum::extract::State(app): axum::extract::State<Arc<App>>,
    request: Request,
    next: Next,
) -> Response {
    let (response, written) = crate::invalidate::capturing(next.run(request)).await;
    // Most requests write nothing: then nothing runs (and no span opens).
    if !written.is_empty() {
        app.apply_own_writes(&written).await;
    }
    response
}

/// Adds the headers every API response carries: never cached (keys and
/// catalog content are private), never content-sniffed. Also reused by the
/// open passkey routes (Task 8), which sit outside `/api/v1`.
///
/// # Arguments
/// * `response` - the response on its way out.
///
/// # Returns
/// The response with the two headers.
#[tracing::instrument(level = "debug", skip_all)]
pub(crate) async fn api_headers(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

/// The tool context for a request, as the MCP layer builds it, with no
/// connection catalogs: each handler passes its catalog as the tool's
/// `catalog` parameter, which wins over every other selection.
///
/// # Arguments
/// * `app` - the application.
/// * `principal` - the caller, from `require_principal`.
/// * `requester` - the request's address and source, for the audit log.
///
/// # Returns
/// A context acting as `principal`.
#[tracing::instrument(level = "debug", skip_all)]
pub(crate) fn context(
    app: &Arc<App>,
    principal: Arc<Principal>,
    requester: Requester,
) -> ToolContext {
    ToolContext::new(Arc::clone(app), principal, None).with_requester(requester)
}

/// Refuses a key below `admin`, for the admin routes that name no catalog
/// (keys, catalog creation).
///
/// # Arguments
/// * `principal` - the calling key.
///
/// # Errors
/// [`OkfError::Forbidden`] for a read or write key.
#[tracing::instrument(level = "debug", skip_all)]
pub(crate) fn require_admin(principal: &Principal) -> Result<(), OkfError> {
    if principal.access >= Access::Admin {
        Ok(())
    } else {
        Err(OkfError::Forbidden {
            reason: "this needs an admin key".into(),
        })
    }
}

/// Refuses all but an admin key that reaches every catalog: the rule for the
/// routes that are not about one catalog's content (catalog creation,
/// ownership, users and teams; plan Decision 3).
///
/// # Arguments
/// * `principal` - the calling key.
///
/// # Errors
/// [`OkfError::Forbidden`] below `admin`, or for a key granted named catalogs.
#[tracing::instrument(level = "debug", skip_all)]
pub(crate) fn require_every_catalog(principal: &Principal) -> Result<(), OkfError> {
    require_admin(principal)?;
    match &principal.reach {
        Reach::All => Ok(()),
        // A ceiling is never admin, so `require_admin` refused it already.
        Reach::Catalogs(_) | Reach::Ceiling => Err(OkfError::Forbidden {
            reason: "this needs an admin key that reaches every catalog".to_string(),
        }),
    }
}

/// Every route a provisioner key may call, as `(method, matched template)`
/// (spec Part 4). The one list: [`provisioner_gate`] refuses every other
/// route before its handler runs, so adding a route never widens a
/// provisioner by accident.
pub(crate) const PROVISIONER_ROUTES: &[(&str, &str)] = &[
    ("GET", "/api/v1/users"),
    ("GET", "/api/v1/users/{name}"),
    ("POST", "/api/v1/users"),
    ("PUT", "/api/v1/users/{name}/email"),
    ("POST", "/api/v1/users/{name}/invite"),
    ("PATCH", "/api/v1/users/{name}"),
    ("DELETE", "/api/v1/users/{name}"),
    ("GET", "/api/v1/teams"),
    ("GET", "/api/v1/teams/{name}"),
    ("POST", "/api/v1/teams"),
    ("PUT", "/api/v1/teams/{name}/members/{user}"),
    ("DELETE", "/api/v1/teams/{name}/members/{user}"),
    ("PATCH", "/api/v1/teams/{name}"),
    ("DELETE", "/api/v1/teams/{name}"),
    ("PUT", "/api/v1/teams/{name}/suspension"),
    ("POST", "/api/v1/catalogs"),
    ("PATCH", "/api/v1/catalogs/{catalog}"),
    ("DELETE", "/api/v1/catalogs/{catalog}"),
    ("PUT", "/api/v1/catalogs/{catalog}/member-access"),
    ("GET", "/api/v1/audit"),
];

/// Whether a provisioner may call this route.
///
/// # Arguments
/// * `method` - the request's method.
/// * `template` - its matched route (`MatchedPath`), never the literal path.
///
/// # Returns
/// `true` for an allow-listed pair.
#[cfg_attr(
    dylint_lib = "hyp_tracing",
    allow(
        uninstrumented_fn,
        reason = "trivial accessor or conversion: returns or maps a value, a span would record nothing new"
    )
)]
pub(crate) fn provisioner_may(method: &Method, template: &str) -> bool {
    PROVISIONER_ROUTES
        .iter()
        .any(|(allowed, route)| method.as_str() == *allowed && template == *route)
}

/// axum middleware, inside `require_principal`: refuses a provisioner on
/// every route that is not in [`PROVISIONER_ROUTES`].
///
/// # Returns
/// The route's response, or 403.
#[tracing::instrument(level = "debug", skip_all)]
async fn provisioner_gate(request: Request, next: Next) -> Response {
    let provisioner = request
        .extensions()
        .get::<Arc<Principal>>()
        .is_some_and(|principal| principal.provisioner);
    // `MatchedPath` holds the template with the `/api/v1` prefix (verified facts).
    let allowed = {
        request
            .extensions()
            .get::<MatchedPath>()
            .is_some_and(|path| provisioner_may(request.method(), path.as_str()))
    };
    if provisioner && !allowed {
        return AppError(OkfError::Forbidden {
            reason: "a provisioner key may not call this route".to_string(),
        })
        .into_response();
    }
    next.run(request).await
}

/// Refuses all but a provisioner or an admin that reaches every catalog:
/// the writes in [`PROVISIONER_ROUTES`].
///
/// # Errors
/// As [`require_every_catalog`].
#[cfg_attr(
    dylint_lib = "hyp_tracing",
    allow(
        uninstrumented_fn,
        reason = "trivial accessor or conversion: returns or maps a value, a span would record nothing new"
    )
)]
pub(crate) fn require_manager(principal: &Principal) -> Result<(), OkfError> {
    if principal.provisioner {
        return Ok(());
    }
    require_every_catalog(principal)
}

/// Refuses all but a provisioner or an admin: the reads in
/// [`PROVISIONER_ROUTES`].
///
/// # Errors
/// As [`require_admin`].
#[cfg_attr(
    dylint_lib = "hyp_tracing",
    allow(
        uninstrumented_fn,
        reason = "trivial accessor or conversion: returns or maps a value, a span would record nothing new"
    )
)]
pub(crate) fn require_reader(principal: &Principal) -> Result<(), OkfError> {
    if principal.provisioner {
        return Ok(());
    }
    require_admin(principal)
}

/// `?after=&limit=` of a paginated catalog listing (M18 spec Decision 7):
/// `GET /catalogs`, `GET /whoami` and `GET /labels`.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct PageQuery {
    /// The `next` of the previous page; absent or empty for the first.
    after: Option<String>,
    /// The page size: default 100, at most 1000 (a larger one is cut).
    limit: Option<u32>,
}

/// One page of the catalogs `principal` reaches, for `query`.
///
/// # Arguments
/// * `app` - the application, for its catalog directory.
/// * `principal` - the caller.
/// * `query` - the request's paging parameters.
///
/// # Returns
/// The page, as [`crate::cache::directory::CatalogDirectory::reachable_page`]
/// gives it.
///
/// # Errors
/// 400 for a malformed cursor; 500 if the read fails.
#[tracing::instrument(level = "debug", skip_all)]
pub(crate) async fn reachable_page(
    app: &App,
    principal: &Principal,
    query: &PageQuery,
) -> Result<Page<Arc<CatalogRow>>, OkfError> {
    app.directory()
        .reachable_page(principal, query.after.as_deref(), page_size(query.limit))
        .await
}
