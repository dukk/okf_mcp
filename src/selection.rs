//! Catalog selection (design "Catalog selection"): which catalogs a tool
//! call reads, and which one it would write.
//!
//! The connection's list (the `X-OKF-Catalogs` header, or its alias
//! `X-OKF-Catalog`, else `?catalogs=` on the MCP URL) is the session's
//! scope: when it is set, a call sees only those catalogs, whatever else
//! the key reaches. A call's argument narrows it and never escapes it.
//! Without a connection list, the argument, else the key's default
//! catalog, else the server's `default_catalog`, is the selection. When
//! none of those names a catalog and the key reaches exactly one catalog
//! that exists, that one is the selection (issue #663); a key granted every
//! catalog ([`Reach::All`]) never falls back, so an admin-reach key does not
//! pick a catalog silently. Every name must exist and be reachable by the
//! key: a list never grants.
//!
//! A closed catalog is refused when named and left out of a connection's
//! list: one whose owner team is suspended (milestone 13; admins are
//! exempt), and, through [`Principal::check_open`], one the caller may not
//! open (milestone 14): with `require_unlock` on, operator staff without a
//! live unlock, and the maintenance runner where the owners turned
//! maintenance off. Reach is checked first, so a key that does not reach a
//! catalog never learns it is closed.

use axum::http::request::Parts;
use chrono::{DateTime, Utc};

use crate::auth::{Principal, Reach};
use crate::error::OkfError;
use crate::store::keys::Access;

/// The header naming a connection's catalogs (`pg,work`); HTTP header names
/// are case-insensitive, and `http` stores them lowercase.
pub const CATALOGS_HEADER: &str = "x-okf-catalogs";

/// The singular spelling, accepted as an alias of [`CATALOGS_HEADER`]: both
/// are read, the plural's names first, as one list.
pub const CATALOG_HEADER_ALIAS: &str = "x-okf-catalog";

/// The MCP URL's query parameter with the same meaning.
pub const CATALOGS_QUERY: &str = "catalogs";

/// rmcp's session header (`Mcp-Session-Id`), as `http` stores header names:
/// lowercase. Lookups by it match any case on the wire.
pub const SESSION_HEADER: &str = "mcp-session-id";

/// The resolved catalogs of one call: never empty, no duplicates, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    catalogs: Vec<String>,
}

impl Selection {
    /// The read set, in order.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "trivial field accessor: returns a field or a borrow of one"
        )
    )]
    pub fn catalogs(&self) -> &[String] {
        &self.catalogs
    }

    /// The default write target: the first catalog.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "trivial field accessor: returns a field or a borrow of one"
        )
    )]
    pub fn write_target(&self) -> &str {
        // `resolve` never builds an empty selection.
        &self.catalogs[0]
    }
}

/// Where a call's catalogs can come from, before the key's default.
// `Copy`: borrowed fields and a timestamp, so tests can build variants with
// `..base`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sources<'a> {
    /// The tool call's `catalog` (as a one-name list) or `catalogs`.
    pub argument: Option<&'a [String]>,
    /// The connection's list ([`connection_catalogs`]).
    pub connection: Option<&'a [String]>,
    /// The server's `default_catalog`.
    pub server_default: Option<&'a str>,
    /// The server clock, for an unlock's expiry.
    pub now: DateTime<Utc>,
}

/// Splits a comma-separated list, trimming names and dropping empty ones.
// `impl Iterator<...> + '_`: a lazy sequence (like a Python generator) that
// borrows `list`, so nothing is copied until a caller collects it.
#[tracing::instrument(level = "trace", skip_all)]
fn split_names(list: &str) -> impl Iterator<Item = String> + '_ {
    list.split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
}

/// `None` for an empty list, so an empty source counts as unset.
#[tracing::instrument(level = "trace", skip_all)]
fn non_empty(names: Vec<String>) -> Option<Vec<String>> {
    // `then_some(x)`: `Some(x)` if the condition holds, else `None`.
    (!names.is_empty()).then_some(names)
}

/// The catalogs a connection names: the `X-OKF-Catalogs` and
/// `X-OKF-Catalog` headers, else the `catalogs` query parameter.
///
/// Both header spellings are read, every `X-OKF-Catalogs` name before every
/// `X-OKF-Catalog` name, as one list ([`resolve`] drops duplicates).
/// Repeated headers are read in order, as one list (RFC 9110 5.3), and so
/// are repeated `catalogs` parameters (`?catalogs=pg&catalogs=work`). A header
/// that names nothing (`" , "`) counts as unset, so the query is read. Bytes
/// that are not UTF-8 are kept (lossily), so a garbled name reaches
/// [`resolve`] and fails naming it rather than being dropped.
///
/// Names are kept exactly as given, and catalog names are matched
/// case-sensitively, so `PG` does not select `pg`. In the query, `+` decodes
/// as a space: `?catalogs=pg+work` is the one name `"pg work"`. Separate
/// names with `,` (or `%2C`). Either mistake fails in [`resolve`], naming
/// the name it could not find.
///
/// # Arguments
/// * `parts` - the HTTP request (rmcp passes it to every tool).
///
/// # Returns
/// The names, or `None` if neither source names a catalog.
#[tracing::instrument(level = "debug", skip_all)]
pub fn connection_catalogs(parts: &Parts) -> Option<Vec<String>> {
    let mut from_header: Vec<String> = Vec::new();
    let values = [CATALOGS_HEADER, CATALOG_HEADER_ALIAS]
        .into_iter()
        .flat_map(|name| parts.headers.get_all(name));
    for value in values {
        // `from_utf8_lossy` returns a `Cow`: a borrow when the bytes are
        // valid UTF-8, a new string (with U+FFFD for bad bytes) otherwise.
        from_header.extend(split_names(&String::from_utf8_lossy(value.as_bytes())));
    }
    // `or_else` runs the closure only when the header named nothing.
    non_empty(from_header).or_else(|| {
        let query = parts.uri.query()?;
        let mut from_query: Vec<String> = Vec::new();
        // `parse` yields decoded `(key, value)` pairs (`%2C` becomes `,`).
        for (key, value) in form_urlencoded::parse(query.as_bytes()) {
            if key == CATALOGS_QUERY {
                from_query.extend(split_names(&value));
            }
        }
        non_empty(from_query)
    })
}

/// Is `name` inside the connection's scope? Every name is when the
/// connection names none (an empty list counts as unset).
///
/// # Arguments
/// * `connection` - the connection's list ([`connection_catalogs`]).
/// * `name` - a catalog name.
///
/// # Returns
/// `true` if a call on this connection may use `name` (the key's reach is
/// checked separately).
#[tracing::instrument(level = "debug", skip_all)]
pub fn in_scope(connection: Option<&[String]>, name: &str) -> bool {
    // `is_none_or(f)`: `true` for `None`, else `f(value)`.
    connection.is_none_or(|list| list.is_empty() || list.iter().any(|listed| listed == name))
}

/// `list` as a selection: in order, without duplicates, every name allowed.
#[tracing::instrument(level = "trace", skip_all)]
fn checked<'a>(
    list: impl IntoIterator<Item = &'a str>,
    allowed: impl Fn(&str) -> bool,
) -> Result<Selection, OkfError> {
    let mut catalogs: Vec<String> = Vec::new();
    for name in list {
        if !allowed(name) {
            return Err(OkfError::CatalogNotSelected {
                catalog: name.to_string(),
            });
        }
        // A few names at most, so a linear `contains` beats a set.
        if !catalogs.iter().any(|seen| seen == name) {
            catalogs.push(name.to_string());
        }
    }
    Ok(Selection { catalogs })
}

/// `selection` refused whole if any of its catalogs is closed
/// ([`Principal::check_open`]): for a call that named them (its argument,
/// or the key's or server's default).
///
/// Every name in `selection` already passed the reach check, so a
/// credential that does not reach a catalog never learns it is closed.
///
/// # Arguments
/// * `principal` - the calling key.
/// * `selection` - the checked selection.
/// * `now` - the server clock.
///
/// # Returns
/// `selection`, unchanged.
///
/// # Errors
/// The first closed catalog's refusal: [`OkfError::Suspended`],
/// [`OkfError::UnlockRequired`] or [`OkfError::MaintenanceOff`].
#[tracing::instrument(level = "trace", skip_all)]
fn refuse_closed(
    principal: &Principal,
    selection: Selection,
    now: DateTime<Utc>,
) -> Result<Selection, OkfError> {
    for name in &selection.catalogs {
        // The selection is for reading; a tool that writes checks its own
        // level and the write's admission after (M14 Decision 24).
        principal.check_open(name, Access::Read, now)?;
    }
    Ok(selection)
}

/// `selection` without its closed catalogs: for a call that falls back to
/// the connection's list, so one closed catalog does not stop the rest.
///
/// # Arguments
/// * `principal` - the calling key.
/// * `selection` - the checked connection list.
/// * `now` - the server clock.
///
/// # Returns
/// The catalogs that are open to the key, in order; never empty.
///
/// # Errors
/// The first catalog's refusal, as [`refuse_closed`], when every one is
/// closed (a selection is never empty).
#[tracing::instrument(level = "trace", skip_all)]
fn without_closed(
    principal: &Principal,
    selection: Selection,
    now: DateTime<Utc>,
) -> Result<Selection, OkfError> {
    let mut kept = Vec::with_capacity(selection.catalogs.len());
    let mut first_refusal = None;
    // `selection.catalogs` is moved out (no `&`), so each kept name is reused
    // rather than copied.
    for name in selection.catalogs {
        match principal.check_open(&name, Access::Read, now) {
            Ok(_) => kept.push(name),
            // `get_or_insert` keeps the first refusal only.
            Err(error) => {
                first_refusal.get_or_insert(error);
            }
        }
    }
    match first_refusal {
        Some(error) if kept.is_empty() => Err(error),
        _ => Ok(Selection { catalogs: kept }),
    }
}

/// Why a call has no selection: the fix a client can make itself first,
/// then the operator's options.
const NOTHING_SELECTED: &str = "no catalog selected: pass `catalog` (see `list_catalogs`); \
     or set the X-OKF-Catalogs header or `?catalogs=` on the MCP URL, or give the key a \
     default catalog";

/// The only catalog `principal` reaches that exists, for a call that no
/// source names a catalog for (issue #663).
///
/// Only a key granted catalogs by name ([`Reach::Catalogs`]) falls back: a
/// key granted every catalog ([`Reach::All`], an admin's or operator's
/// reach) gets `None` even when one catalog exists, because a key of that
/// reach selecting a catalog it was never pointed at would be a surprise,
/// and a second catalog created later would change what its bare calls
/// read. The count is of reachable catalogs that exist, not of open ones:
/// a closed catalog still counts, so suspending one of two catalogs never
/// makes the other the selection, and a sole closed catalog is refused
/// the way a key default is (the caller checks it).
///
/// # Arguments
/// * `principal` - the calling key.
/// * `exists` - whether a catalog of that name is loaded.
///
/// # Returns
/// The catalog's name, or `None` for zero or several, or for [`Reach::All`].
#[tracing::instrument(level = "trace", skip_all)]
fn only_reachable<'a>(principal: &'a Principal, exists: &impl Fn(&str) -> bool) -> Option<&'a str> {
    let Reach::Catalogs(levels) = &principal.reach else {
        return None;
    };
    // The map's keys are the names the key reaches; `filter` keeps those
    // loaded, lazily, so at most two are looked at.
    let mut reachable = levels
        .keys()
        .map(String::as_str)
        .filter(|name| exists(name));
    let first = reachable.next()?;
    // `is_none().then_some(..)`: `Some(first)` only when there is no second.
    reachable.next().is_none().then_some(first)
}

/// Resolves a call's catalogs.
///
/// A connection list is checked whole, even when the argument names a
/// catalog in it, so a mistyped list fails every call instead of falling
/// back to a default.
///
/// # Arguments
/// * `principal` - the calling key.
/// * `sources` - the argument, the connection and the server default.
/// * `exists` - whether a catalog of that name is loaded.
///
/// # Returns
/// The selection: the argument if set, else the connection's list, else
/// the key's default, else the server's, else the only catalog the key
/// reaches (`only_reachable`: never for [`Reach::All`]); without
/// duplicates.
///
/// # Errors
/// [`OkfError::CatalogNotSelected`] naming the first catalog that does not
/// exist, that the key does not reach, or (in the argument) that the
/// connection's list leaves out; [`OkfError::Refused`]
/// (`NOTHING_SELECTED`) if no source names a catalog and the key does not
/// reach exactly one; then, for a named, default or only catalog that is
/// closed to the
/// key, its refusal: [`OkfError::Suspended`] (for a credential below admin)
/// when its owner team is suspended, [`OkfError::UnlockRequired`] when the
/// key needs an unlock it does not have, [`OkfError::MaintenanceOff`] for
/// the maintenance runner on a catalog that opted out. A selection that
/// falls back to the connection's list leaves closed catalogs out instead,
/// and is refused only when every listed catalog is closed. Reach is
/// always checked first.
// `impl Fn(&str) -> bool`: any function or closure taking a name, like a
// Python callable argument.
#[tracing::instrument(level = "debug", skip_all)]
pub fn resolve(
    principal: &Principal,
    sources: Sources<'_>,
    exists: impl Fn(&str) -> bool,
) -> Result<Selection, OkfError> {
    let available = |name: &str| exists(name) && principal.reaches(name);
    // An empty list (`catalogs: []`) counts as unset, so the selection is
    // never empty.
    #[tracing::instrument(level = "trace", skip_all)]
    fn set(list: Option<&[String]>) -> Option<&[String]> {
        list.filter(|list| !list.is_empty())
    }
    let argument = set(sources.argument);
    if let Some(connection) = set(sources.connection) {
        let scope = checked(connection.iter().map(String::as_str), available)?;
        return match argument {
            None => without_closed(principal, scope, sources.now),
            // Every scope name is available, so membership is the whole check.
            // `and_then` runs the open check only on an `Ok` selection.
            Some(list) => checked(list.iter().map(String::as_str), |name| {
                scope.catalogs.iter().any(|listed| listed == name)
            })
            .and_then(|selection| refuse_closed(principal, selection, sources.now)),
        };
    }
    if let Some(list) = argument {
        return checked(list.iter().map(String::as_str), available)
            .and_then(|selection| refuse_closed(principal, selection, sources.now));
    }
    // `as_deref`: `Option<String>` to `Option<&str>`, borrowing the name.
    match principal
        .default_catalog
        .as_deref()
        .or(sources.server_default)
    {
        Some(name) => checked([name], available)
            .and_then(|selection| refuse_closed(principal, selection, sources.now)),
        // The fallback names a catalog the key reaches and that exists, and
        // is refused when closed exactly as a key default is.
        None => match only_reachable(principal, &exists) {
            Some(name) => checked([name], available)
                .and_then(|selection| refuse_closed(principal, selection, sources.now)),
            None => Err(OkfError::Refused {
                reason: NOTHING_SELECTED.to_string(),
            }),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    use crate::auth::{CredentialKind, Reach};
    use crate::store::keys::Access;

    /// A `read` key reaching `reach` (every catalog when empty), seeing each
    /// of `suspended` as suspended, as the key cache would build it.
    fn principal_with(reach: &[&str], default: Option<&str>, suspended: &[&str]) -> Principal {
        Principal {
            key_id: "k".to_string(),
            access: Access::Read,
            maintenance_report: false,
            provisioner: false,
            reach: if reach.is_empty() {
                Reach::All
            } else {
                Reach::Catalogs(
                    reach
                        .iter()
                        .map(|name| (name.to_string(), Access::Read))
                        .collect(),
                )
            },
            default_catalog: default.map(str::to_string),
            actor: "a".to_string(),
            kind: CredentialKind::ApiKey,
            user: None,
            user_id: None,
            gates: Default::default(),
            suspended: std::sync::Arc::new(
                suspended
                    .iter()
                    .map(|name| {
                        (
                            name.to_string(),
                            crate::store::teams::SuspendedCatalog {
                                team: "family".to_string(),
                                notice: None,
                                catalog_label: name.to_string(),
                                team_label: "family".to_string(),
                            },
                        )
                    })
                    .collect(),
            ),
        }
    }

    fn principal(reach: &[&str], default: Option<&str>) -> Principal {
        principal_with(reach, default, &[])
    }

    /// The name of the catalog a `Suspended` refusal names, or a panic.
    fn suspended_name(refused: Result<Selection, OkfError>) -> String {
        match refused {
            Err(OkfError::Suspended { catalog, .. }) => catalog,
            other => panic!("expected Suspended, got {other:?}"),
        }
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| name.to_string()).collect()
    }

    const EXISTING: [&str; 3] = ["pg", "work", "home"];

    fn exists(name: &str) -> bool {
        EXISTING.contains(&name)
    }

    fn select(principal: &Principal, sources: Sources<'_>) -> Result<Vec<String>, OkfError> {
        resolve(principal, sources, exists).map(|selection| selection.catalogs().to_vec())
    }

    #[test]
    fn without_a_connection_the_argument_then_the_defaults_win() {
        let key = principal(&[], Some("home"));
        let argument = names(&["work"]);
        let with_argument = Sources {
            argument: Some(&argument),
            connection: None,
            server_default: Some("pg"),
            now: Utc::now(),
        };

        assert_eq!(select(&key, with_argument).unwrap(), ["work"]);
        let key_default = Sources {
            argument: None,
            ..with_argument
        };
        assert_eq!(select(&key, key_default).unwrap(), ["home"]);
        let server_default = principal(&[], None);
        assert_eq!(select(&server_default, key_default).unwrap(), ["pg"]);
    }

    #[test]
    fn the_connection_is_the_scope_and_the_argument_only_narrows_it() {
        let key = principal(&["pg", "work"], Some("pg"));
        let connection = names(&["work"]);
        let inside = names(&["work", "work"]);
        let outside = names(&["work", "pg"]);
        let base = Sources {
            argument: None,
            connection: Some(&connection),
            server_default: Some("pg"),
            now: Utc::now(),
        };

        let alone = resolve(&key, base, exists).unwrap();
        let narrowed = select(
            &key,
            Sources {
                argument: Some(&inside),
                ..base
            },
        );
        let escaped = select(
            &key,
            Sources {
                argument: Some(&outside),
                ..base
            },
        );

        // Neither the key's default nor its other grant joins the scope.
        assert_eq!(alone.catalogs(), ["work"]);
        assert_eq!(alone.write_target(), "work");
        assert_eq!(narrowed.unwrap(), ["work"]);
        assert!(
            matches!(&escaped, Err(OkfError::CatalogNotSelected { catalog }) if catalog == "pg"),
            "{escaped:?}"
        );
    }

    #[test]
    fn a_bad_connection_list_fails_even_with_a_good_argument() {
        let key = principal(&["pg"], Some("pg"));
        let connection = names(&["pg", "work"]);
        let argument = names(&["pg"]);
        let sources = Sources {
            argument: Some(&argument),
            connection: Some(&connection),
            server_default: Some("pg"),
            now: Utc::now(),
        };

        let error = select(&key, sources).unwrap_err();

        assert!(
            matches!(&error, OkfError::CatalogNotSelected { catalog } if catalog == "work"),
            "{error}"
        );
    }

    #[test]
    fn in_scope_is_everything_without_a_list() {
        let list = names(&["work"]);
        let empty: Vec<String> = Vec::new();

        assert!(in_scope(None, "pg"));
        assert!(in_scope(Some(&empty), "pg"));
        assert!(in_scope(Some(&list), "work"));
        assert!(!in_scope(Some(&list), "pg"));
    }

    #[test]
    fn an_empty_argument_list_counts_as_unset() {
        let key = principal(&[], None);
        let empty: Vec<String> = Vec::new();
        let connection = names(&["work"]);
        let sources = Sources {
            argument: Some(&empty),
            connection: Some(&connection),
            server_default: None,
            now: Utc::now(),
        };

        assert_eq!(select(&key, sources).unwrap(), ["work"]);
        let empty_connection = Sources {
            connection: Some(&empty),
            server_default: Some("pg"),
            ..sources
        };
        assert_eq!(select(&key, empty_connection).unwrap(), ["pg"]);
    }

    #[test]
    fn the_first_name_is_the_write_target_and_duplicates_collapse() {
        let key = principal(&[], None);
        let connection = names(&["work", "pg", "work"]);
        let sources = Sources {
            argument: None,
            connection: Some(&connection),
            server_default: None,
            now: Utc::now(),
        };

        let selection = resolve(&key, sources, exists).unwrap();

        assert_eq!(selection.catalogs(), ["work", "pg"]);
        assert_eq!(selection.write_target(), "work");
    }

    #[test]
    fn an_unknown_or_ungranted_catalog_fails_naming_it() {
        let key = principal(&["pg"], None);
        for list in [&["pg", "nope"][..], &["work"][..]] {
            let connection = names(list);
            let sources = Sources {
                argument: None,
                connection: Some(&connection),
                server_default: None,
                now: Utc::now(),
            };

            let error = select(&key, sources).unwrap_err();

            assert!(
                matches!(&error, OkfError::CatalogNotSelected { catalog } if catalog == list[list.len() - 1]),
                "{list:?}: {error}"
            );
        }
    }

    #[test]
    fn an_ungranted_server_default_is_an_error_too() {
        let key = principal(&["work"], None);
        let sources = Sources {
            argument: None,
            connection: None,
            server_default: Some("pg"),
            now: Utc::now(),
        };

        assert!(matches!(
            select(&key, sources),
            Err(OkfError::CatalogNotSelected { catalog }) if catalog == "pg"
        ));
    }

    #[test]
    fn nothing_set_means_nothing_selected() {
        let key = principal(&[], None);
        let sources = Sources {
            argument: None,
            connection: None,
            server_default: None,
            now: Utc::now(),
        };

        let error = select(&key, sources).unwrap_err();

        assert!(matches!(error, OkfError::Refused { .. }));
        assert!(error.to_string().contains("X-OKF-Catalogs"));
        assert!(error.to_string().contains("?catalogs="));
    }

    /// Sources that name no catalog: no argument, connection or defaults.
    fn unset() -> Sources<'static> {
        Sources {
            argument: None,
            connection: None,
            server_default: None,
            now: Utc::now(),
        }
    }

    /// Issue #663: with nothing selected, a key granted exactly one catalog
    /// that exists gets that one, as its read set and write target. A
    /// granted name that is not loaded does not count.
    #[test]
    fn with_nothing_selected_the_only_reachable_catalog_is_the_selection() {
        for reach in [&["pg"][..], &["pg", "gone"][..]] {
            let key = principal(reach, None);

            let selection = resolve(&key, unset(), exists).unwrap();

            assert_eq!(selection.catalogs(), ["pg"], "{reach:?}");
            assert_eq!(selection.write_target(), "pg", "{reach:?}");
        }
    }

    /// The fallback comes last: every other source still wins over it, and
    /// a bad one still fails rather than falling back.
    #[test]
    fn the_only_catalog_fallback_never_overrides_another_source() {
        let key = principal(&["pg"], None);
        let argument = names(&["work"]);
        let connection = names(&["work"]);
        let defaulted = principal(&["pg"], Some("work"));

        for (source, sources, key) in [
            (
                "argument",
                Sources {
                    argument: Some(&argument),
                    ..unset()
                },
                &key,
            ),
            (
                "connection",
                Sources {
                    connection: Some(&connection),
                    ..unset()
                },
                &key,
            ),
            (
                "server default",
                Sources {
                    server_default: Some("work"),
                    ..unset()
                },
                &key,
            ),
            ("key default", unset(), &defaulted),
        ] {
            let refused = select(key, sources);
            assert!(
                matches!(&refused, Err(OkfError::CatalogNotSelected { catalog }) if catalog == "work"),
                "{source}: an unreachable name is refused, not replaced by `pg`: {refused:?}"
            );
        }
    }

    /// A sole catalog that is closed is refused as a key default is: by
    /// suspension (below admin) or a missing unlock. An admin's sole
    /// suspended catalog is selected, as a named one is.
    #[test]
    fn a_closed_only_catalog_is_refused_like_a_default() {
        use crate::auth::AccessGates;

        let suspended = principal_with(&["pg"], None, &["pg"]);
        let admin = Principal {
            access: Access::Admin,
            ..principal_with(&["pg"], None, &["pg"])
        };
        let locked = Principal {
            user_id: Some("u-staff".to_string()),
            gates: std::sync::Arc::new(AccessGates {
                admins: ["u-staff".to_string()].into(),
                require_unlock: true,
                ..AccessGates::default()
            }),
            ..principal(&["pg"], None)
        };
        let locked_default = Principal {
            default_catalog: Some("pg".to_string()),
            ..locked.clone()
        };

        assert_eq!(suspended_name(resolve(&suspended, unset(), exists)), "pg");
        assert_eq!(select(&admin, unset()).unwrap(), ["pg"]);
        assert_eq!(
            select(&locked, unset()).unwrap_err().kind(),
            select(&locked_default, unset()).unwrap_err().kind(),
            "the fallback refuses as the key default does"
        );
        assert_eq!(
            select(&locked, unset()).unwrap_err().kind(),
            "UnlockRequired"
        );
    }

    /// Several reachable catalogs keep the refusal, even when all but one
    /// are closed: closing a catalog never moves a key's selection.
    #[test]
    fn several_reachable_catalogs_are_still_refused() {
        for key in [
            principal(&["pg", "work"], None),
            principal_with(&["pg", "work"], None, &["work"]),
        ] {
            let error = select(&key, unset()).unwrap_err();

            assert!(matches!(error, OkfError::Refused { .. }), "{error:?}");
            assert!(
                error
                    .to_string()
                    .starts_with("no catalog selected: pass `catalog` (see `list_catalogs`)"),
                "the client's own fix comes first: {error}"
            );
        }
    }

    /// A key granted every catalog never falls back, even when only one
    /// catalog exists (the decision documented on [`only_reachable`]).
    #[test]
    fn a_key_reaching_every_catalog_does_not_fall_back() {
        let key = principal(&[], None);
        assert_eq!(key.reach, Reach::All);

        let refused = resolve(&key, unset(), |name| name == "pg");

        assert!(
            matches!(refused, Err(OkfError::Refused { .. })),
            "{refused:?}"
        );
    }

    fn parts(uri: &str, header: Option<&str>) -> Parts {
        let mut request = axum::http::Request::builder().uri(uri);
        if let Some(value) = header {
            request = request.header(CATALOGS_HEADER, value);
        }
        request.body(()).unwrap().into_parts().0
    }

    #[test]
    fn the_connection_reads_the_header_before_the_query() {
        assert_eq!(
            connection_catalogs(&parts("/mcp?catalogs=home", Some(" pg, work ,"))),
            Some(names(&["pg", "work"]))
        );
        assert_eq!(
            connection_catalogs(&parts("/mcp?x=1&catalogs=home%2Cpg", None)),
            Some(names(&["home", "pg"]))
        );
        assert_eq!(
            connection_catalogs(&parts("/mcp?catalogs=home", Some(" , "))),
            Some(names(&["home"]))
        );
        assert_eq!(connection_catalogs(&parts("/mcp", Some(" , "))), None);
        assert_eq!(connection_catalogs(&parts("/mcp", None)), None);
        assert_eq!(
            connection_catalogs(&parts("/mcp?catalogs=pg&catalogs=work", None)),
            Some(names(&["pg", "work"]))
        );
        assert_eq!(
            connection_catalogs(&parts("/mcp?catalogs=&catalogs=pg", None)),
            Some(names(&["pg"]))
        );

        let mut repeated = parts("/mcp?catalogs=home", Some("pg"));
        repeated
            .headers
            .append(CATALOGS_HEADER, HeaderValue::from_static("work"));
        assert_eq!(connection_catalogs(&repeated), Some(names(&["pg", "work"])));

        let mut both = parts("/mcp?catalogs=home", None);
        both.headers
            .insert(CATALOG_HEADER_ALIAS, HeaderValue::from_static("work, pg"));
        assert_eq!(connection_catalogs(&both), Some(names(&["work", "pg"])));
        both.headers
            .insert(CATALOGS_HEADER, HeaderValue::from_static("pg"));
        assert_eq!(
            connection_catalogs(&both),
            Some(names(&["pg", "work", "pg"]))
        );

        let mut garbled = parts("/mcp?catalogs=home", None);
        garbled.headers.insert(
            CATALOGS_HEADER,
            HeaderValue::from_bytes(b"pg,w\xffrk").unwrap(),
        );
        assert_eq!(
            connection_catalogs(&garbled),
            Some(names(&["pg", "w\u{fffd}rk"]))
        );
    }

    #[test]
    fn a_named_suspended_catalog_is_refused() {
        let key = principal_with(&[], Some("pg"), &["work"]);
        let list = names(&["pg", "work"]);
        let connection = names(&["pg", "work"]);
        let named = Sources {
            argument: Some(&list),
            connection: None,
            server_default: None,
            now: Utc::now(),
        };
        let narrowed = Sources {
            connection: Some(&connection),
            ..named
        };

        assert_eq!(suspended_name(resolve(&key, named, exists)), "work");
        assert_eq!(
            suspended_name(resolve(&key, narrowed, exists)),
            "work",
            "an argument naming a suspended catalog on a connection is refused too"
        );
    }

    #[test]
    fn a_suspended_default_is_refused() {
        let key = principal_with(&[], Some("work"), &["work"]);
        let server = principal_with(&[], None, &["home"]);
        let sources = Sources {
            argument: None,
            connection: None,
            server_default: None,
            now: Utc::now(),
        };

        assert_eq!(suspended_name(resolve(&key, sources, exists)), "work");
        assert_eq!(
            suspended_name(resolve(
                &server,
                Sources {
                    server_default: Some("home"),
                    ..sources
                },
                exists
            )),
            "home",
            "the server's default is a named catalog too"
        );
    }

    #[test]
    fn the_connection_list_drops_suspended_catalogs() {
        let key = principal_with(&[], None, &["pg"]);
        let connection = names(&["pg", "work"]);
        let sources = Sources {
            argument: None,
            connection: Some(&connection),
            server_default: None,
            now: Utc::now(),
        };

        let selection = resolve(&key, sources, exists).unwrap();

        assert_eq!(selection.catalogs(), ["work"]);
        assert_eq!(selection.write_target(), "work");
    }

    #[test]
    fn a_connection_list_of_only_suspended_catalogs_is_refused() {
        let key = principal_with(&[], None, &["pg", "work"]);
        let connection = names(&["pg", "work"]);
        let sources = Sources {
            argument: None,
            connection: Some(&connection),
            server_default: None,
            now: Utc::now(),
        };

        assert_eq!(suspended_name(resolve(&key, sources, exists)), "pg");
    }

    #[test]
    fn an_unreached_suspended_catalog_is_not_selected_rather_than_suspended() {
        // `work` is suspended, but this key reaches only `pg`.
        let key = principal_with(&["pg"], Some("work"), &["work"]);
        let list = names(&["work"]);
        let connection = names(&["pg", "work"]);
        let base = Sources {
            argument: None,
            connection: None,
            server_default: None,
            now: Utc::now(),
        };
        let cases = [
            (
                "argument",
                Sources {
                    argument: Some(&list),
                    ..base
                },
            ),
            ("default", base),
            (
                "connection",
                Sources {
                    connection: Some(&connection),
                    ..base
                },
            ),
        ];

        for (source, sources) in cases {
            let refused = resolve(&key, sources, exists);
            assert!(
                matches!(refused, Err(OkfError::CatalogNotSelected { ref catalog }) if catalog == "work"),
                "{source}: an unreached catalog never reads as suspended: {refused:?}"
            );
        }
    }

    #[test]
    fn an_admin_selects_suspended_catalogs() {
        let admin = Principal {
            access: Access::Admin,
            ..principal_with(&[], Some("pg"), &["pg", "work"])
        };
        let list = names(&["work"]);
        let connection = names(&["pg", "work"]);
        let base = Sources {
            argument: None,
            connection: None,
            server_default: None,
            now: Utc::now(),
        };

        assert_eq!(select(&admin, base).unwrap(), ["pg"]);
        assert_eq!(
            select(
                &admin,
                Sources {
                    argument: Some(&list),
                    ..base
                }
            )
            .unwrap(),
            ["work"]
        );
        assert_eq!(
            select(
                &admin,
                Sources {
                    connection: Some(&connection),
                    ..base
                }
            )
            .unwrap(),
            ["pg", "work"]
        );
    }

    /// M14 Task 9b: with `require_unlock` on, a catalog the key's user has
    /// no unlock for is closed like a suspended one: refused when named or
    /// as the default, dropped from a connection's list, and the list
    /// refused only when nothing is left. An expired unlock is no unlock.
    #[test]
    fn a_locked_catalog_is_refused_when_named_and_dropped_from_a_connection_list() {
        use crate::auth::AccessGates;
        use crate::store::unlocks::LiveUnlock;
        let now = Utc::now();
        let unlock = |catalog: &str, id, expires_at| LiveUnlock {
            id,
            user_id: "u-staff".to_string(),
            catalog: catalog.to_string(),
            expires_at,
        };
        let staff = Principal {
            user_id: Some("u-staff".to_string()),
            gates: std::sync::Arc::new(AccessGates {
                admins: ["u-staff".to_string()].into(),
                require_unlock: true,
                unlocks: [(
                    "u-staff".to_string(),
                    vec![
                        unlock("work", 1, now + chrono::TimeDelta::hours(1)),
                        unlock("home", 2, now - chrono::TimeDelta::seconds(1)),
                    ],
                )]
                .into(),
                ..AccessGates::default()
            }),
            ..principal_with(&[], Some("pg"), &[])
        };
        let connection = names(&["pg", "work"]);
        let only_pg = names(&["pg"]);
        let named_pg = names(&["pg"]);
        let named_home = names(&["home"]);
        let base = Sources {
            argument: None,
            connection: None,
            server_default: None,
            now,
        };
        let kind = |refused: Result<Vec<String>, OkfError>| refused.unwrap_err().kind();

        assert_eq!(
            select(
                &staff,
                Sources {
                    connection: Some(&connection),
                    ..base
                }
            )
            .unwrap(),
            ["work"],
            "the connection list keeps only the unlocked catalog"
        );
        assert_eq!(
            kind(select(
                &staff,
                Sources {
                    connection: Some(&connection),
                    argument: Some(&named_pg),
                    ..base
                }
            )),
            "UnlockRequired",
            "naming the locked one"
        );
        assert_eq!(
            kind(select(&staff, base)),
            "UnlockRequired",
            "the key's default"
        );
        assert_eq!(
            kind(select(
                &staff,
                Sources {
                    connection: Some(&only_pg),
                    ..base
                }
            )),
            "UnlockRequired",
            "a list of only locked catalogs"
        );
        assert_eq!(
            kind(select(
                &staff,
                Sources {
                    argument: Some(&named_home),
                    ..base
                }
            )),
            "UnlockRequired",
            "an expired unlock"
        );
        let unlocked_later = Sources {
            now: now + chrono::TimeDelta::hours(2),
            connection: Some(&connection),
            ..base
        };
        assert_eq!(
            kind(select(&staff, unlocked_later)),
            "UnlockRequired",
            "the unlock of work expired too"
        );
    }
}
