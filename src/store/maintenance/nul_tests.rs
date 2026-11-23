//! A NUL in any client-sent maintenance id, token, cursor or text is answered
//! before SQL, never as a database error (issue #339): Postgres refuses a NUL
//! in text. Each guard is tested apart, with otherwise valid inputs.

use chrono::Duration;

use super::tests::{actor, setup, t0};
use super::{reports, runs};
use crate::db::test_db;
use crate::error::OkfError;
use crate::maintenance::types::*;
use crate::stores::{AccountDb, CatalogDb};

const NUL: &str = "a\0b";

/// A catalog with one live lease: its id, the run's id and the lease.
async fn leased(pool: &sqlx::PgPool) -> (i64, String, Lease) {
    let catalog = setup(pool, &["a.md"]).await;
    let batch = runs::claim(
        &AccountDb::of(pool),
        &CatalogDb::main_of(pool),
        catalog,
        &actor("one"),
        "claim",
        1,
        Duration::days(30),
        t0(),
    )
    .await
    .unwrap();
    let lease = batch.leases[0].clone();
    (catalog, batch.run_id, lease)
}

fn submission(lease: &Lease) -> Submission {
    Submission {
        outcome: WorkOutcome::Completed,
        assessment: Assessment::SupportedCurrent,
        summary: "Original assessment".into(),
        evidence: vec![Evidence {
            pin: lease.pin.clone(),
            excerpt: "Body".into(),
        }],
        findings: vec![],
        reported_tokens: None,
    }
}

fn is_not_found<T: std::fmt::Debug>(result: Result<T, OkfError>) -> bool {
    matches!(result, Err(OkfError::NotFound { .. }))
}

#[tokio::test(flavor = "multi_thread")]
async fn a_nul_in_a_work_id_or_token_is_not_found_for_renew_and_submit() {
    let db = test_db().await;
    let (catalog, _, lease) = leased(&db.pool).await;
    let who = actor("one");

    for (work, token) in [(NUL, lease.token.as_str()), (lease.work_id.as_str(), NUL)] {
        let renewed = runs::renew(
            &CatalogDb::main_of(&db.pool),
            catalog,
            &who,
            work,
            token,
            "renew",
            t0(),
        )
        .await;
        assert!(is_not_found(renewed), "renew {work:?} {token:?}");
        let submitted = reports::submit(
            &CatalogDb::main_of(&db.pool),
            catalog,
            &who,
            work,
            token,
            "submit",
            &submission(&lease),
            t0(),
        )
        .await;
        assert!(is_not_found(submitted), "submit {work:?} {token:?}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_nul_in_a_run_id_is_not_found_for_close_and_the_reads() {
    let db = test_db().await;
    let (catalog, _, _) = leased(&db.pool).await;

    let closed = runs::close(
        &CatalogDb::main_of(&db.pool),
        catalog,
        &actor("one"),
        NUL,
        RunState::Cancelled,
        "reset",
        false,
        "close",
        t0(),
    )
    .await;
    assert!(is_not_found(closed), "close");
    assert!(is_not_found(
        runs::get_run(&CatalogDb::main_of(&db.pool), catalog, NUL).await
    ));
    assert!(is_not_found(
        reports::get_finding(&CatalogDb::main_of(&db.pool), catalog, NUL).await
    ));
    let disposed = reports::dispose(
        &CatalogDb::main_of(&db.pool),
        catalog,
        &actor("one"),
        NUL,
        Disposition::Dismissed,
        "no longer relevant",
        "dispose",
        t0(),
    )
    .await;
    assert!(is_not_found(disposed), "dispose");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_nul_in_a_list_cursor_or_in_report_text_is_never_a_database_error() {
    let db = test_db().await;
    let (catalog, _, lease) = leased(&db.pool).await;
    let who = actor("one");
    let mut seen = Vec::new();

    seen.push((
        "runs cursor",
        runs::list_runs(
            &CatalogDb::main_of(&db.pool),
            catalog,
            Some(NUL),
            10,
            runs::RunOrder::Asc,
        )
        .await
        .map(|_| ()),
    ));
    seen.push((
        "findings cursor",
        reports::list_findings(&CatalogDb::main_of(&db.pool), catalog, Some(NUL), 10)
            .await
            .map(|_| ()),
    ));
    let mut text = submission(&lease);
    text.summary = NUL.into();
    seen.push((
        "summary",
        reports::submit(
            &CatalogDb::main_of(&db.pool),
            catalog,
            &who,
            &lease.work_id,
            &lease.token,
            "text",
            &text,
            t0(),
        )
        .await
        .map(|_| ()),
    ));
    let mut excerpt = submission(&lease);
    excerpt.evidence[0].excerpt = NUL.into();
    seen.push((
        "excerpt",
        reports::submit(
            &CatalogDb::main_of(&db.pool),
            catalog,
            &who,
            &lease.work_id,
            &lease.token,
            "excerpt",
            &excerpt,
            t0(),
        )
        .await
        .map(|_| ()),
    ));
    let disposed = reports::dispose(
        &CatalogDb::main_of(&db.pool),
        catalog,
        &who,
        "someid",
        Disposition::Dismissed,
        NUL,
        "dispose-text",
        t0(),
    )
    .await
    .map(|_| ());
    seen.push(("rationale", disposed));
    for (what, result) in seen {
        // Cursors name no row; text is refused as invalid.
        let ok = match what {
            "runs cursor" | "findings cursor" => is_not_found(result),
            _ => matches!(result, Err(OkfError::Refused { .. })),
        };
        assert!(ok, "{what}");
    }
}
