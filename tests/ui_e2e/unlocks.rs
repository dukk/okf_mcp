//! Unlock before access in the console (M14 Task 9e): a refused admin
//! unlocks from Browse, reads, and ends the unlock on the Unlocks page,
//! which a catalog's owner reads redacted; an owner switches maintenance
//! off. And, per `tools/agentic/lessons.md`' console rules: a double click
//! sends one write, a drawer opened again while it runs draws its control
//! disabled, a refusal keeps its reason on screen when the read after it
//! fails, and a late table read never draws over a newer one.

use chrono::Utc;
use okf_mcp::store::audit::AuditActor;
use okf_mcp::store::settings::{self, SettingKey};
use okf_mcp::store::users::{self, AdminChange};

use crate::harness::{drive, Ui};
use crate::self_service::{console_base, login_server};
use crate::support::server::TestServer;

const PASSWORD: &str = "a long passphrase 1";
const STAFF_EMAIL: &str = "staff@example.com";

/// A [`login_server`] with `staff`, an admin user who signs in with
/// [`STAFF_EMAIL`] and [`PASSWORD`], and `require_unlock` on.
async fn locked_server() -> TestServer {
    let server = login_server().await;
    server
        .user_with_password("staff", STAFF_EMAIL, PASSWORD)
        .await;
    let pool = &server.db.pool;
    users::set_admin(
        pool,
        "staff",
        AdminChange {
            on: true,
            allow_last: true,
        },
        &AuditActor::cli(),
        Utc::now(),
    )
    .await
    .expect("make staff an admin");
    settings::set(
        pool,
        SettingKey::RequireUnlock,
        "true",
        &AuditActor::cli(),
        Utc::now(),
    )
    .await
    .expect("require unlocks");
    server.app.poll().await.expect("poll");
    server
}

/// Fills the open Unlock form for `reason`, one hour.
fn fill_unlock(ui: &Ui, reason: &str) {
    ui.select("#unlock-form select[name=category]", "support");
    ui.fill("#unlock-form input[name=reason]", reason);
    ui.fill("#unlock-form input[name=hours]", "1");
}

/// Whether catalog `name` lets the maintenance runner in, as stored.
async fn maintenance_on(server: &TestServer, name: &str) -> bool {
    sqlx::query_scalar("SELECT maintenance FROM catalogs WHERE name = $1")
        .bind(name)
        .fetch_one(&server.db.pool)
        .await
        .expect("read the catalog's maintenance flag")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refused_admin_unlocks_main_reads_it_and_ends_the_unlock() {
    let server = locked_server().await;
    let owner_email = server.owner_with_password(PASSWORD).await;
    drive(console_base(&server), move |ui| {
        ui.sign_in_password(STAFF_EMAIL, PASSWORD);
        ui.open("/browse/main");
        ui.wait_visible("#browse-locked");
        ui.wait_text("#browse-locked", "needs an unlock");
        // The refusal's toast says what it is, not "cannot do this".
        ui.wait_text("#toasts", "needs an unlock");
        ui.click("#browse-unlock");
        fill_unlock(ui, "ticket 12");
        ui.click("#unlock-form button[type=submit]");
        ui.wait_text("#toasts", "Unlocked main");
        ui.wait_text("#catalog-tree", "Overview");
        ui.open("/unlocks");
        ui.wait_text("#unlocks-table", "ticket 12");
        ui.wait_text("#unlocks-table", "staff");
        ui.click("[data-test=unlock-end-main]");
        ui.click("#confirm-ok");
        ui.wait_text("#toasts", "Ended the unlock of main");
        ui.wait_text("#unlocks-table", "ended");
        ui.open("/browse/main");
        ui.wait_visible("#browse-locked");
        // The catalog's owner reads the unlock redacted: by an operator,
        // with no reason, no staff member and nothing to end.
        ui.click("#signout");
        ui.wait_visible("#login");
        ui.sign_in_password(&owner_email, PASSWORD);
        ui.wait_visible("[data-nav=unlocks]");
        ui.open("/unlocks");
        ui.wait_text("#unlocks-table", "an operator");
        ui.wait_text("#unlocks-table", "Support");
        let table = ui.text("#unlocks-table");
        assert!(
            !table.contains("ticket 12") && !table.contains("staff"),
            "the owner's table names the reason or the staff member: {table}"
        );
        ui.wait_hidden("[data-test=unlock-end-main]");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_owner_switches_maintenance_off_for_its_catalog() {
    let server = login_server().await;
    // OWNER's personal team owns main; give OWNER a password to sign in with.
    let owner_email = server.owner_with_password(PASSWORD).await;
    let ran = drive(console_base(&server), move |ui| {
        ui.sign_in_password(&owner_email, PASSWORD);
        ui.wait_visible("[data-need=owner] [data-nav=catalogs]");
        ui.open("/catalogs");
        ui.click("[data-catalog=main]");
        ui.wait_visible("#catalog-maintenance:checked");
        ui.click("#catalog-maintenance");
        ui.wait_text("#toasts", "Maintenance is off for main");
        ui.wait_visible("#catalog-maintenance:not(:checked)");
        ui.wait_text("#catalog-maintenance-state", "off");
    })
    .await;
    if !ran {
        return;
    }
    assert!(
        !maintenance_on(&server, "main").await,
        "the switch did not turn maintenance off"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_maintenance_switch_sends_one_write_and_keeps_a_refusal() {
    let server = login_server().await;
    let owner_email = server.owner_with_password(PASSWORD).await;
    let ran = drive(console_base(&server), move |ui| {
        ui.sign_in_password(&owner_email, PASSWORD);
        ui.open("/catalogs");
        ui.click("[data-catalog=main]");
        ui.wait_visible("#catalog-maintenance:checked");
        // Refused, and the read after it fails too: the reason stays, the
        // switch shows the last value, marked out of date, and is usable.
        ui.fail_next("PUT", "/maintenance", 403, "refused by the test");
        ui.fail_next("GET", "/catalogs", 503, "the server is unavailable");
        ui.click("#catalog-maintenance");
        ui.wait_visible("#catalog-maintenance-error");
        ui.wait_text("#catalog-maintenance-error", "refused by the test");
        ui.wait_text("#catalog-maintenance-state", "may be out of date");
        ui.wait_visible("#catalog-maintenance:checked:not([disabled])");
        // A double click sends one write, and a drawer opened again while
        // it runs draws the switch disabled.
        ui.hold_next("PUT", "/api/v1/catalogs/main/maintenance");
        assert!(
            ui.click_twice("#catalog-maintenance"),
            "the switch stayed enabled while its write ran"
        );
        ui.click("#drawer-close");
        ui.click("[data-catalog=main]");
        ui.wait_visible("#catalog-maintenance[disabled]");
        assert_eq!(ui.held_matches(), 1, "a double click sent two writes");
        ui.release_held();
        ui.wait_text("#toasts", "Maintenance is off for main");
        // The reopened drawer shows the write's answer, its refusal gone.
        ui.wait_visible("#catalog-maintenance:not(:checked):not([disabled])");
        ui.wait_hidden("#catalog-maintenance-error");
        ui.wait_text_gone("#catalog-maintenance-state", "out of date");
    })
    .await;
    if !ran {
        return;
    }
    assert!(!maintenance_on(&server, "main").await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unlock_is_sent_once_its_refusals_stay_and_a_late_read_never_draws() {
    let server = locked_server().await;
    drive(console_base(&server), move |ui| {
        ui.sign_in_password(STAFF_EMAIL, PASSWORD);
        // The unlock switches are chosen from true and false on Settings.
        ui.open("/settings");
        ui.wait_visible("select[data-test=setting-input-require_unlock]");
        ui.wait_visible("select[data-test=setting-input-unlock_notify_owners]");
        ui.open("/unlocks");
        ui.wait_text("#unlocks-table", "No unlocks");
        // A refused unlock keeps its reason in the form.
        ui.click("#unlocks-new");
        ui.select("#unlock-form select[name=catalog]", "main");
        fill_unlock(ui, "ticket 7");
        ui.fail_next("POST", "/unlocks", 422, "over the hour limit");
        ui.click("#unlock-form button[type=submit]");
        ui.wait_visible("#unlock-form-error");
        ui.wait_text("#unlock-form-error", "over the hour limit");
        ui.wait_visible("#unlock-form button[type=submit]:not([disabled])");
        // A double click sends one unlock; the form opened again for the
        // same catalog meanwhile draws Unlock disabled.
        ui.hold_next("POST", "/api/v1/unlocks");
        assert!(
            ui.click_twice("#unlock-form button[type=submit]"),
            "Unlock stayed enabled while its write ran"
        );
        ui.mark("#unlock-form");
        ui.click("#drawer-close");
        ui.click("#unlocks-new");
        ui.wait_redrawn("#unlock-form");
        ui.select("#unlock-form select[name=catalog]", "main");
        ui.wait_visible("#unlock-form button[type=submit][disabled]");
        assert_eq!(ui.held_matches(), 1, "a double click sent two unlocks");
        ui.release_held();
        ui.wait_text("#toasts", "Unlocked main");
        ui.wait_text("#unlocks-table", "ticket 7");
        ui.wait_text("#unlocks-table", "live");
        // A refused End keeps its reason beside the row when the reload
        // after it fails, and End stays usable.
        ui.fail_next("DELETE", "", 409, "refused by the test");
        ui.fail_next("GET", "/unlocks", 503, "the server is unavailable");
        ui.click("[data-test=unlock-end-main]");
        ui.click("#confirm-ok");
        ui.wait_visible("[data-test^=unlock-error-]");
        ui.wait_text("[data-test^=unlock-error-]", "refused by the test");
        ui.wait_visible("#unlocks-stale");
        ui.wait_visible("[data-test=unlock-end-main]:not([disabled])");
        // An End whose reload fails still draws its answer, marked out of
        // date; and a Refresh that started before it and answers late
        // draws nothing.
        ui.hold_answer("GET", "/unlocks", || ui.click("#unlocks-refresh"));
        ui.fail_next("GET", "/unlocks", 503, "the server is unavailable");
        ui.click("[data-test=unlock-end-main]");
        ui.click("#confirm-ok");
        ui.wait_text("#toasts", "Ended the unlock of main");
        ui.wait_text("#unlocks-table", "ended");
        ui.wait_hidden("[data-test^=unlock-error-]");
        ui.release_held();
        let table = ui.text("#unlocks-table");
        assert!(
            table.contains("ended") && !table.contains("live"),
            "a late read drew over the newer one: {table}"
        );
        ui.wait_visible("#unlocks-stale");
        ui.click("#unlocks-refresh");
        ui.wait_hidden("#unlocks-stale");
        ui.wait_text("#unlocks-table", "ended");
    })
    .await;
}
