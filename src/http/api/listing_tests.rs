//! Paginated catalog listings and the search fan-out cap over HTTP (M18
//! Task 10, spec Decisions 7 and 8): `GET /catalogs`, `GET /whoami`,
//! `GET /labels` and `GET /search`.

use axum::http::StatusCode;
use serde_json::Value;

use crate::cache::directory::cursor_of;
use crate::config::Config;
use crate::http::api::testing::send;
use crate::mcp::testing::{app_with_new, stored_secret, TestApp};
use crate::store::keys::Access;

/// How to read the catalog names off one listing's page.
type ReadNames = fn(&Value) -> Vec<String>;

/// The three listings, each with how to read the catalog names off a page.
const LISTINGS: [(&str, ReadNames); 3] = [
    ("/api/v1/catalogs", |body| {
        body["catalogs"]
            .as_array()
            .expect("a list")
            .iter()
            .map(|row| row["name"].as_str().expect("a name").to_owned())
            .collect()
    }),
    ("/api/v1/whoami", |body| {
        body["catalogs"]
            .as_array()
            .expect("a list")
            .iter()
            .map(|name| name.as_str().expect("a name").to_owned())
            .collect()
    }),
    ("/api/v1/labels", |body| {
        // A JSON object's keys, in order (`serde_json` keeps them sorted).
        body["catalogs"]
            .as_object()
            .expect("an object")
            .keys()
            .cloned()
            .collect()
    }),
];

/// Every page of `path` for `key`, `limit` at a time: each page's names and
/// whether it had a `next`.
async fn walk(
    test: &TestApp,
    path: &str,
    names: ReadNames,
    key: &str,
    limit: u32,
) -> Vec<(Vec<String>, bool)> {
    let mut pages = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let uri = match &after {
            Some(cursor) => format!("{path}?limit={limit}&after={cursor}"),
            None => format!("{path}?limit={limit}"),
        };
        let (status, body) = send(test, "GET", &uri, Some(key), None).await;
        assert_eq!(status, StatusCode::OK, "{uri}: {body}");
        let next = body
            .get("next")
            .map(|next| next.as_str().expect("a cursor").to_owned());
        pages.push((names(&body), next.is_some()));
        match next {
            Some(cursor) => after = Some(cursor),
            None => return pages,
        }
        assert!(pages.len() < 20, "{path}: the pages never end");
    }
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|name| (*name).to_owned()).collect()
}

const SEVEN: [&str; 7] = ["a", "b", "c", "d", "e", "f", "g"];

#[tokio::test(flavor = "multi_thread")]
async fn every_listing_pages_an_every_catalog_key_by_name() {
    let test = app_with_new(&SEVEN, Config::default()).await;
    let root = stored_secret(&test, "root", Access::Admin, &[], false).await;

    for (path, read) in LISTINGS {
        let pages = walk(&test, path, read, &root, 3).await;

        assert_eq!(
            pages,
            [
                (names(&["a", "b", "c"]), true),
                (names(&["d", "e", "f"]), true),
                (names(&["g"]), false),
            ],
            "{path}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_granted_key_gets_its_catalogs_in_one_page_with_no_next() {
    let test = app_with_new(&SEVEN, Config::default()).await;
    let reader = stored_secret(&test, "reader", Access::Read, &["f", "b", "d"], false).await;

    for (path, read) in LISTINGS {
        let (status, body) = send(&test, "GET", path, Some(&reader), None).await;

        assert_eq!(status, StatusCode::OK, "{path}: {body}");
        assert_eq!(read(&body), names(&["b", "d", "f"]), "{path}");
        // Absent, not null, on the last page.
        assert_eq!(body.get("next"), None, "{path}: {body}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cursor_lists_nothing_the_key_does_not_reach() {
    let test = app_with_new(&SEVEN, Config::default()).await;
    let reader = stored_secret(&test, "reader", Access::Read, &["b", "f"], false).await;

    for (path, read) in LISTINGS {
        // After `d` (a catalog the key does not reach) and after `dd` (no
        // such catalog): the same answer, so the cursor probes nothing.
        let (_, hidden) = send(
            &test,
            "GET",
            &format!("{path}?after={}", cursor_of("d")),
            Some(&reader),
            None,
        )
        .await;
        let (_, missing) = send(
            &test,
            "GET",
            &format!("{path}?after={}", cursor_of("dd")),
            Some(&reader),
            None,
        )
        .await;

        assert_eq!(read(&hidden), names(&["f"]), "{path}");
        assert_eq!(read(&missing), read(&hidden), "{path}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_malformed_cursor_is_a_400_on_every_listing() {
    let test = app_with_new(&SEVEN, Config::default()).await;
    let root = stored_secret(&test, "root", Access::Admin, &[], false).await;
    let reader = stored_secret(&test, "reader", Access::Read, &["b"], false).await;
    // Not base64url; padded; a name with a space; a NUL (percent-encoded).
    let malformed = ["!!", "YQ%3D%3D", "QSBC", "YQBi"];

    for (path, _) in LISTINGS {
        for cursor in malformed {
            for key in [&root, &reader] {
                let uri = format!("{path}?after={cursor}");
                let (status, body) = send(&test, "GET", &uri, Some(key), None).await;

                assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}: {body}");
                assert_eq!(body["error"], "InvalidPath", "{uri}: {body}");
            }
        }
    }
}

/// The 26 catalogs `c00` to `c25`.
fn twenty_six() -> Vec<String> {
    (0..26).map(|index| format!("c{index:02}")).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn search_without_catalogs_is_refused_past_the_fanout_cap() {
    let catalogs = twenty_six();
    let all: Vec<&str> = catalogs.iter().map(String::as_str).collect();
    let test = app_with_new(&all, Config::default()).await;
    let root = stored_secret(&test, "root", Access::Admin, &[], false).await;
    let twenty_five = stored_secret(&test, "reader", Access::Read, &all[..25], false).await;

    let (refused_status, refused) =
        send(&test, "GET", "/api/v1/search?q=alpha", Some(&root), None).await;
    let (named_status, named) = send(
        &test,
        "GET",
        "/api/v1/search?q=alpha&catalogs=c00,c25",
        Some(&root),
        None,
    )
    .await;
    let (capped_status, capped) = send(
        &test,
        "GET",
        "/api/v1/search?q=alpha",
        Some(&twenty_five),
        None,
    )
    .await;

    assert_eq!(refused_status, StatusCode::BAD_REQUEST, "{refused}");
    assert!(
        refused["message"].as_str().is_some_and(|message| message
            .contains("name the catalogs to search (at most 25 are searched by default)")),
        "{refused}"
    );
    assert_eq!(named_status, StatusCode::OK, "naming them works: {named}");
    assert_eq!(capped_status, StatusCode::OK, "25 are searched: {capped}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_fanout_cap_follows_its_setting() {
    let test = app_with_new(
        &["a", "b", "c"],
        Config {
            search_fanout_max: 2,
            ..Config::default()
        },
    )
    .await;
    let root = stored_secret(&test, "root", Access::Admin, &[], false).await;
    let two = stored_secret(&test, "two", Access::Read, &["a", "c"], false).await;

    let (over, _) = send(&test, "GET", "/api/v1/search?q=x", Some(&root), None).await;
    let (at, body) = send(&test, "GET", "/api/v1/search?q=x", Some(&two), None).await;

    assert_eq!(over, StatusCode::BAD_REQUEST);
    assert_eq!(at, StatusCode::OK, "{body}");
}

#[tokio::test(flavor = "multi_thread")]
async fn search_naming_more_catalogs_than_the_cap_is_refused() {
    let test = app_with_new(
        &["a", "b", "c"],
        Config {
            search_fanout_max: 2,
            ..Config::default()
        },
    )
    .await;
    let root = stored_secret(&test, "root", Access::Admin, &[], false).await;

    let (over, body) = send(
        &test,
        "GET",
        "/api/v1/search?q=x&catalogs=a,b,c",
        Some(&root),
        None,
    )
    .await;
    let (at, at_body) = send(
        &test,
        "GET",
        "/api/v1/search?q=x&catalogs=a,b",
        Some(&root),
        None,
    )
    .await;
    let (repeated, repeated_body) = send(
        &test,
        "GET",
        "/api/v1/search?q=x&catalogs=a,b,a,b,a",
        Some(&root),
        None,
    )
    .await;

    assert_eq!(over, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"], "InvalidPath", "{body}");
    assert!(
        body["message"]
            .as_str()
            .is_some_and(|message| message.contains("at most 2")),
        "{body}"
    );
    assert_eq!(at, StatusCode::OK, "{at_body}");
    assert_eq!(repeated, StatusCode::OK, "{repeated_body}");
}
