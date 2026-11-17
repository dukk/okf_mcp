//! Unlocks (spec Part 6, "An unlock"): an admin user's time-limited,
//! reasoned access to one customer catalog's contents. Creating and ending
//! one bump `admin_generation`, so the key cache (which holds the live
//! ones) reloads; expiry is checked at each request.
//!
//! Lock order, the same in [`create`] and [`end`] (`tools/agentic/lessons.md`):
//! first every user row the change and its audit entry lock, through
//! [`audit::lock_people`] (the actor, the person of a personal owner team,
//! and in [`create`] the unlocked admin; admins first by id, as
//! `users::set_admin` takes them), then the catalog row (the `unlocks`
//! foreign key holds it `FOR KEY SHARE`) or the unlock row, then the
//! `server_state` generation row. Holding the unlocked admin `FOR KEY SHARE`
//! keeps `users::set_admin` (which locks every admin `FOR UPDATE`) from
//! clearing the flag before the unlock commits; the actor's row, held the
//! same way, does the same for the creator's own admin flag, which
//! [`create`] checks under that lock (issue #447). The owner email is queued in
//! the same transaction, so an unlock that rolls back mails nobody, and
//! nothing is awaited after the commit.
//!
//! Both entries name the admin as the audit person the unlock fixed at
//! creation (`unlocks.person_id`), marked operator
//! ([`audit::record_for_person`]), so a later demotion or deletion of the
//! user never shows owners who it was.

use chrono::{DateTime, TimeDelta, Utc};
use sqlx::{PgConnection, PgExecutor, PgPool};

use crate::clock::format_timestamp;
use crate::error::OkfError;
use crate::mail::queue;
use crate::mail::templates::Template;
use crate::store::audit::{
    self, AccessKind, Action, AuditActor, Entry, FixedPerson, People, RowLock, Target,
};
use crate::store::catalogs::{self, bump_admin_generation, is_valid_name};

/// `unlock_max_hours` with no override.
pub const DEFAULT_MAX_HOURS: u32 = 4;

/// The largest `unlock_max_hours` the setting takes (plan Decision 13).
pub const MAX_HOURS_LIMIT: u32 = 72;

/// The longest reason, in characters (the schema checks it too).
pub const MAX_REASON_CHARS: usize = 500;

/// Why an operator opens a catalog. clap and serde spell each as stored.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum, serde::Deserialize, serde::Serialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    /// Helping the customer.
    #[value(name = "support")]
    Support,
    /// Carrying out a deletion request.
    #[value(name = "deletion_request")]
    DeletionRequest,
    /// Carrying out an export request.
    #[value(name = "export_request")]
    ExportRequest,
    /// Investigating a security problem.
    #[value(name = "security_investigation")]
    SecurityInvestigation,
    /// A legal obligation.
    #[value(name = "legal")]
    Legal,
    /// Anything else (the reason says what).
    #[value(name = "other")]
    Other,
}

impl Category {
    /// Every category, in the order forms list them.
    pub const ALL: [Category; 6] = [
        Category::Support,
        Category::DeletionRequest,
        Category::ExportRequest,
        Category::SecurityInvestigation,
        Category::Legal,
        Category::Other,
    ];

    /// The stored spelling.
    ///
    /// # Returns
    /// `"support"`, `"deletion_request"`, `"export_request"`,
    /// `"security_investigation"`, `"legal"` or `"other"`.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "trivial accessor or conversion: maps a value, no I/O"
        )
    )]
    pub fn as_str(self) -> &'static str {
        match self {
            Category::Support => "support",
            Category::DeletionRequest => "deletion_request",
            Category::ExportRequest => "export_request",
            Category::SecurityInvestigation => "security_investigation",
            Category::Legal => "legal",
            Category::Other => "other",
        }
    }

    /// Parses the stored spelling.
    ///
    /// # Arguments
    /// * `text` - an `unlocks.category` value.
    ///
    /// # Returns
    /// The category, or `None` for anything else.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "trivial accessor or conversion: maps a value, no I/O"
        )
    )]
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|category| category.as_str() == text)
    }

    /// The category in words, for the owners' email ("for {label}").
    ///
    /// # Returns
    /// `"support"`, `"a deletion request"`, `"an export request"`, `"a
    /// security investigation"`, `"a legal obligation"` or `"another reason"`.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "trivial accessor or conversion: maps a value, no I/O"
        )
    )]
    pub fn label(self) -> &'static str {
        match self {
            Category::Support => "support",
            Category::DeletionRequest => "a deletion request",
            Category::ExportRequest => "an export request",
            Category::SecurityInvestigation => "a security investigation",
            Category::Legal => "a legal obligation",
            Category::Other => "another reason",
        }
    }
}

/// An unlock to create.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NewUnlock<'a> {
    /// The catalog's name.
    pub catalog: &'a str,
    /// The admin user it is for, by name.
    pub user: &'a str,
    /// Why, as a category owners see.
    pub category: Category,
    /// Why, in words (a ticket number); never shown to owners.
    pub reason: &'a str,
    /// How long, in hours (1 to `unlock_max_hours`).
    pub hours: u32,
}

/// The settings an unlock follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnlockPolicy {
    /// `unlock_max_hours`.
    pub max_hours: u32,
    /// `unlock_notify_owners`.
    pub notify_owners: bool,
}

/// A stored unlock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnlockRecord {
    /// Its id.
    pub id: i64,
    /// The catalog's name.
    pub catalog: String,
    /// The catalog's owner team now (`None` for an unowned catalog).
    pub owner_team: Option<String>,
    /// The user's name, while it exists.
    pub user: Option<String>,
    /// Who it was, as the audit log names them (`deleted user #N` once the
    /// user is deleted).
    pub person: String,
    /// Why, as a category.
    pub category: Category,
    /// Why, in words.
    pub reason: String,
    /// When it was made (server clock).
    pub created_at: DateTime<Utc>,
    /// When it expires.
    pub expires_at: DateTime<Utc>,
    /// When it was ended early, if it was.
    pub ended_at: Option<DateTime<Utc>>,
    /// Content reads made under it.
    pub reads: i64,
    /// Writes made under it.
    pub writes: i64,
    /// Exports made under it.
    pub exports: i64,
}

/// Where an unlock stands at a moment. serde spells each as `as_str` does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnlockState {
    /// Neither ended nor expired: its admin's credentials may open the
    /// catalog.
    Live,
    /// Past its expiry without being ended.
    Expired,
    /// Ended early by an admin.
    Ended,
}

impl UnlockState {
    /// The state's name.
    ///
    /// # Returns
    /// `"live"`, `"expired"` or `"ended"`.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "trivial accessor or conversion: maps a value, no I/O"
        )
    )]
    pub fn as_str(self) -> &'static str {
        match self {
            UnlockState::Live => "live",
            UnlockState::Expired => "expired",
            UnlockState::Ended => "ended",
        }
    }
}

impl UnlockRecord {
    /// Where the unlock stands at `now`. An unlock ended early reads
    /// `ended` even after its expiry has passed too.
    ///
    /// # Arguments
    /// * `now` - the server clock.
    ///
    /// # Returns
    /// Its state.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "trivial accessor or conversion: maps a value, no I/O"
        )
    )]
    pub fn state(&self, now: DateTime<Utc>) -> UnlockState {
        if self.ended_at.is_some() {
            UnlockState::Ended
        } else if self.expires_at <= now {
            UnlockState::Expired
        } else {
            UnlockState::Live
        }
    }

    /// When the unlock stops (or stopped) applying: its early end, else
    /// its expiry.
    ///
    /// # Returns
    /// The time.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "trivial accessor or conversion: maps a value, no I/O"
        )
    )]
    pub fn ends(&self) -> DateTime<Utc> {
        self.ended_at.unwrap_or(self.expires_at)
    }
}

/// One `unlocks` row as [`find_in`] and [`list`] read it; the category
/// is text until [`UnlockRow::into_record`] parses it.
#[derive(sqlx::FromRow)]
struct UnlockRow {
    id: i64,
    catalog: String,
    owner_team: Option<String>,
    user_name: Option<String>,
    person: String,
    category: String,
    reason: String,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    ended_at: Option<DateTime<Utc>>,
    reads: i64,
    writes: i64,
    exports: i64,
}

impl UnlockRow {
    /// The public record.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "trivial accessor or conversion: maps a value, no I/O"
        )
    )]
    fn into_record(self) -> UnlockRecord {
        UnlockRecord {
            id: self.id,
            catalog: self.catalog,
            owner_team: self.owner_team,
            user: self.user_name,
            person: self.person,
            category: Category::parse(&self.category).expect("the schema checks unlocks.category"),
            reason: self.reason,
            created_at: self.created_at,
            expires_at: self.expires_at,
            ended_at: self.ended_at,
            reads: self.reads,
            writes: self.writes,
            exports: self.exports,
        }
    }
}

// One SELECT shared by `find_in` and `list` (a macro so `concat!` can
// append each query's tail at compile time, as `select_catalogs!` does).
// `COALESCE`: the live name, else the audit person's (`deleted user #N`).
macro_rules! select_unlocks {
    () => {
        "SELECT u.id, c.name AS catalog, t.name AS owner_team, us.name AS user_name, \
                COALESCE(us.name, p.name) AS person, u.category, u.reason, u.created_at, \
                u.expires_at, u.ended_at, u.reads, u.writes, u.exports \
         FROM unlocks u \
         JOIN catalogs c ON c.id = u.catalog_id \
         LEFT JOIN teams t ON t.id = c.owner_team_id \
         LEFT JOIN users us ON us.id = u.user_id \
         JOIN audit_people p ON p.id = u.person_id"
    };
}

/// A live unlock, as the key cache holds it.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct LiveUnlock {
    /// Its id.
    pub id: i64,
    /// The unlocked admin's `users.id`.
    pub user_id: String,
    /// The catalog's name.
    pub catalog: String,
    /// When it expires.
    pub expires_at: DateTime<Utc>,
}

/// Which unlocks [`list`] returns.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UnlockFilter<'a> {
    /// Only this catalog's.
    pub catalog: Option<&'a str>,
    /// Only those of catalogs this team owns now.
    pub team: Option<&'a str>,
    /// Only live ones (neither ended nor expired at this time).
    pub live_at: Option<DateTime<Utc>>,
}

/// The refusal for an unlock past the time limit.
#[cfg_attr(
    dylint_lib = "hyp_tracing",
    allow(
        uninstrumented_fn,
        reason = "trivial conversion: builds an error value, no I/O"
    )
)]
fn hours_refused(policy: &UnlockPolicy) -> OkfError {
    OkfError::LimitExceeded {
        limit: "unlock_max_hours",
        reason: format!("an unlock lasts 1 to {} hours", policy.max_hours),
    }
}

/// A reason as stored: trimmed, 1 to [`MAX_REASON_CHARS`] characters, with
/// no control characters (a NUL, which Postgres refuses, would be a 500).
#[tracing::instrument(level = "trace", skip_all)]
fn check_reason(reason: &str) -> Result<&str, OkfError> {
    let reason = reason.trim();
    // `chars().count()`: characters, not bytes (`len()` counts UTF-8 bytes).
    let length = reason.chars().count();
    if length == 0 || length > MAX_REASON_CHARS || reason.chars().any(char::is_control) {
        return Err(OkfError::LimitExceeded {
            limit: "reason",
            reason: format!(
                "an unlock needs a reason of 1 to {MAX_REASON_CHARS} characters with no control \
                 characters"
            ),
        });
    }
    Ok(reason)
}

/// Creates an unlock, records `catalog.unlocked` in full, queues the owner
/// team's owners an email when `policy.notify_owners`, and bumps the
/// administrative generation, in one transaction (module docs: lock order).
///
/// # Arguments
/// * `pool` - the database.
/// * `new` - the catalog, the admin user, why and for how long.
/// * `policy` - `unlock_max_hours` and `unlock_notify_owners`.
/// * `actor` - who, for the audit log.
/// * `now` - the server clock: the start.
///
/// # Returns
/// The new unlock.
///
/// # Errors
/// [`OkfError::LimitExceeded`] (`unlock_max_hours`) for 0 hours or more
/// than the limit, (`reason`) for a blank, over-long or control-character
/// reason; [`OkfError::UnknownName`] for an unknown (or malformed) user;
/// [`OkfError::CatalogNotSelected`] for an unknown (or malformed) catalog;
/// [`OkfError::Refused`] for a user who is not an admin;
/// [`OkfError::Forbidden`] for an actor whose own user is not an admin user
/// (the command line has none and passes);
/// [`OkfError::Database`], a failed audit write or mail insert included.
#[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "unlocks.create"))]
pub async fn create(
    pool: &PgPool,
    new: &NewUnlock<'_>,
    policy: &UnlockPolicy,
    actor: &AuditActor,
    now: DateTime<Utc>,
) -> Result<UnlockRecord, OkfError> {
    if new.hours == 0 || new.hours > policy.max_hours {
        return Err(hours_refused(policy));
    }
    let reason = check_reason(new.reason)?;
    // `checked_add_signed` is `None` past chrono's range instead of panicking.
    let expires_at = now
        .checked_add_signed(TimeDelta::hours(i64::from(new.hours)))
        .ok_or_else(|| hours_refused(policy))?;
    let mut tx = pool.begin().await?;
    // Users first: the actor, the admin (`FOR KEY SHARE`, which
    // `set_admin`'s `FOR UPDATE` waits on, so the flag read here holds
    // until the commit), and a personal owner team's person.
    let people = People {
        target: Some((new.user, RowLock::KeyShare)),
        catalogs: &[new.catalog],
        ..People::default()
    };
    let (user_id, is_admin) = audit::lock_people(&mut tx, actor, &people)
        .await?
        .ok_or_else(|| crate::store::users::unknown(new.user))?;
    require_staff(&mut tx, actor).await?;
    if !is_admin {
        return Err(OkfError::Refused {
            reason: format!(
                "{:?} is not an admin user; an unlock is for operator staff",
                new.user
            ),
        });
    }
    let catalog = catalogs::find(&mut *tx, new.catalog).await?;
    // The foreign key takes the catalog row `FOR KEY SHARE`, so a
    // concurrent `catalogs::delete` either commits first (this insert then
    // fails its check) or waits for this one.
    let (id, person_id): (i64, i64) = sqlx::query_as(
        "INSERT INTO unlocks (catalog_id, user_id, person_id, category, reason, created_at, \
                              expires_at) \
         VALUES ($1, $2, audit_person($2), $3, $4, $5, $6) RETURNING id, person_id",
    )
    .bind(catalog.id)
    .bind(&user_id)
    .bind(new.category.as_str())
    .bind(reason)
    .bind(now)
    .bind(expires_at)
    .fetch_one(&mut *tx)
    .await?;
    let teams = audit::catalog_teams(&mut tx, catalog.id).await?;
    // The admin is the entry's person, marked operator, so owners see
    // neither who it was nor (Task 8a) the reason. `end` names the same
    // person the same way (`staff_person`).
    audit::record_for_person(
        &mut tx,
        actor,
        &Entry {
            action: Action::CatalogUnlocked,
            target: Target::Catalog(new.catalog),
            person: None,
            teams: &teams,
            details: serde_json::json!({
                "unlock": id,
                "category": new.category.as_str(),
                "reason": reason,
                "starts": format_timestamp(now),
                "expires": format_timestamp(expires_at),
            }),
        },
        staff_person(person_id),
        now,
    )
    .await?;
    if policy.notify_owners {
        notify_owners(&mut tx, catalog.id, new, expires_at, now).await?;
    }
    bump_admin_generation(&mut tx).await?;
    // Read before the commit: nothing that can fail runs after it.
    let record = find_in(&mut tx, id).await?;
    tx.commit().await?;
    Ok(record)
}

/// Refuses an actor whose user is not an admin user (spec Part 6: an unlock
/// is made by an admin user; issue #447). The command line acts with no
/// user, as the operator, and passes. Call it after
/// [`audit::lock_people`], which holds the actor's row `FOR KEY SHARE`:
/// `users::set_admin` takes it `FOR UPDATE`, so the flag read here cannot
/// change before the commit, and a demotion committed while this waited is
/// seen.
///
/// # Arguments
/// * `conn` - the unlock's transaction.
/// * `actor` - who makes the unlock.
///
/// # Errors
/// [`OkfError::Forbidden`] for a user who is not (or no longer) an admin,
/// or is gone; [`OkfError::Database`].
#[tracing::instrument(level = "trace", skip_all, fields(db.system.name = "postgresql", db.operation.name = "unlocks.require_staff"))]
async fn require_staff(conn: &mut PgConnection, actor: &AuditActor) -> Result<(), OkfError> {
    let Some(user_id) = actor.user_id.as_deref() else {
        return Ok(());
    };
    let staff: Option<bool> = sqlx::query_scalar("SELECT is_admin FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_optional(&mut *conn)
        .await?;
    if staff == Some(true) {
        return Ok(());
    }
    Err(OkfError::Forbidden {
        reason: "this credential's user is not an admin user; only operator staff make unlocks"
            .to_string(),
    })
}

/// Queues the unlocked email to every owner of the catalog's owner team
/// that has an address, inside the unlock's transaction (no mail for an
/// unlock that rolls back). An unowned catalog mails nobody.
#[tracing::instrument(level = "trace", skip_all, fields(db.system.name = "postgresql", db.operation.name = "unlocks.notify_owners", catalog_id = catalog_id))]
async fn notify_owners(
    conn: &mut PgConnection,
    catalog_id: i64,
    new: &NewUnlock<'_>,
    expires_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<(), OkfError> {
    let owners: Vec<String> = sqlx::query_scalar(
        "SELECT u.email::text FROM catalogs c \
         JOIN team_members m ON m.team_id = c.owner_team_id AND m.role = 'owner' \
         JOIN users u ON u.id = m.user_id \
         WHERE c.id = $1 AND u.email IS NOT NULL ORDER BY u.email",
    )
    .bind(catalog_id)
    .fetch_all(&mut *conn)
    .await?;
    let template = Template::Unlocked {
        catalog: new.catalog.to_string(),
        category: new.category.label().to_string(),
        until: format_timestamp(expires_at),
    };
    for owner in &owners {
        queue::enqueue_in(&mut *conn, owner, &template, now).await?;
    }
    Ok(())
}

/// Ends a live unlock now, records `catalog.unlock_ended`, and bumps the
/// administrative generation, in one transaction (module docs: lock order).
///
/// # Arguments
/// * `pool` - the database.
/// * `id` - the unlock.
/// * `actor` - who, for the audit log.
/// * `now` - the server clock.
///
/// # Returns
/// The ended unlock.
///
/// # Errors
/// [`OkfError::NotFound`] for an unknown id; [`OkfError::Refused`] for one
/// that has already ended or expired; [`OkfError::Database`], a failed
/// audit write included.
#[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "unlocks.end"))]
pub async fn end(
    pool: &PgPool,
    id: i64,
    actor: &AuditActor,
    now: DateTime<Utc>,
) -> Result<UnlockRecord, OkfError> {
    let not_found = || OkfError::NotFound {
        path: format!("unlocks/{id}"),
    };
    // Read without a lock, to name the catalog (whose personal owner
    // team's person the entry locks) first; the `UPDATE` below decides
    // under the row's lock.
    let catalog: Option<String> = sqlx::query_scalar(
        "SELECT c.name FROM unlocks u JOIN catalogs c ON c.id = u.catalog_id WHERE u.id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    let catalog = catalog.ok_or_else(not_found)?;
    let mut tx = pool.begin().await?;
    // The unlocked admin is not locked: the entry names the audit person the
    // unlock fixed (`staff_person`), not the user as it is now.
    let people = People {
        catalogs: &[catalog.as_str()],
        ..People::default()
    };
    audit::lock_people(&mut tx, actor, &people).await?;
    // `GREATEST`: a clock behind `created_at` still ends it, at its start.
    let ended: Option<(i64, i64)> = sqlx::query_as(
        "UPDATE unlocks SET ended_at = GREATEST($2, created_at) \
         WHERE id = $1 AND ended_at IS NULL AND expires_at > $2 \
         RETURNING catalog_id, person_id",
    )
    .bind(id)
    .bind(now)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((catalog_id, person_id)) = ended else {
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM unlocks WHERE id = $1)")
                .bind(id)
                .fetch_one(&mut *tx)
                .await?;
        return Err(if exists {
            OkfError::Refused {
                reason: format!("unlock {id} has already ended or expired"),
            }
        } else {
            not_found()
        });
    };
    let teams = audit::catalog_teams(&mut tx, catalog_id).await?;
    audit::record_for_person(
        &mut tx,
        actor,
        &Entry {
            action: Action::CatalogUnlockEnded,
            target: Target::Catalog(&catalog),
            person: None,
            teams: &teams,
            details: serde_json::json!({ "unlock": id }),
        },
        staff_person(person_id),
        now,
    )
    .await?;
    bump_admin_generation(&mut tx).await?;
    let record = find_in(&mut tx, id).await?;
    tx.commit().await?;
    Ok(record)
}

/// How both of an unlock's entries name its admin: the audit person the
/// unlock fixed at creation, marked operator. `create` refuses a user who is
/// not an admin under the user's lock, so every unlock's person was operator
/// staff when it began; a later demotion or deletion of the user changes
/// neither the person nor the marking (plan Decision 18).
#[cfg_attr(
    dylint_lib = "hyp_tracing",
    allow(
        uninstrumented_fn,
        reason = "trivial conversion: pure arithmetic or formatting on its arguments, no I/O"
    )
)]
fn staff_person(person_id: i64) -> FixedPerson {
    FixedPerson {
        person_id,
        operator: true,
    }
}

/// One unlock, inside the caller's connection or transaction.
///
/// # Arguments
/// * `conn` - a connection or transaction (`&mut tx`).
/// * `id` - the unlock.
///
/// # Returns
/// Its record.
///
/// # Errors
/// [`OkfError::NotFound`] for an unknown id; [`OkfError::Database`].
#[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "unlocks.find_in"))]
pub async fn find_in(conn: &mut PgConnection, id: i64) -> Result<UnlockRecord, OkfError> {
    sqlx::query_as::<_, UnlockRow>(concat!(select_unlocks!(), " WHERE u.id = $1"))
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?
        .map(UnlockRow::into_record)
        .ok_or_else(|| OkfError::NotFound {
            path: format!("unlocks/{id}"),
        })
}

/// One unlock.
///
/// # Arguments
/// * `pool` - the database.
/// * `id` - the unlock.
///
/// # Returns
/// Its record.
///
/// # Errors
/// As [`find_in`].
#[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "unlocks.find"))]
pub async fn find(pool: &PgPool, id: i64) -> Result<UnlockRecord, OkfError> {
    let mut conn = pool.acquire().await?;
    find_in(&mut conn, id).await
}

/// The unlocks a filter names, newest first.
///
/// # Arguments
/// * `pool` - the database.
/// * `filter` - a catalog, an owner team, and whether only live ones.
///
/// # Returns
/// The records. A malformed catalog or team name matches nothing.
///
/// # Errors
/// [`OkfError::Database`].
#[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "unlocks.list"))]
pub async fn list(pool: &PgPool, filter: &UnlockFilter<'_>) -> Result<Vec<UnlockRecord>, OkfError> {
    // A malformed name (a NUL, which Postgres refuses) names nothing.
    let malformed = |name: Option<&str>| name.is_some_and(|name| !is_valid_name(name));
    if malformed(filter.catalog) || malformed(filter.team) {
        return Ok(Vec::new());
    }
    // A NULL parameter turns its condition off.
    let rows = sqlx::query_as::<_, UnlockRow>(concat!(
        select_unlocks!(),
        " WHERE ($1::text IS NULL OR c.name = $1) AND ($2::text IS NULL OR t.name = $2) \
            AND ($3::timestamptz IS NULL OR (u.ended_at IS NULL AND u.expires_at > $3)) \
          ORDER BY u.created_at DESC, u.id DESC"
    ))
    .bind(filter.catalog)
    .bind(filter.team)
    .bind(filter.live_at)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(UnlockRow::into_record).collect())
}

/// The unlocks in force at `now`, for the key cache: neither ended nor
/// expired, and of a user that still exists.
///
/// # Arguments
/// * `executor` - the database: the pool, or the key cache's snapshot
///   transaction (`impl PgExecutor<'_>` takes either).
/// * `now` - the server clock.
///
/// # Returns
/// The live unlocks, by id.
///
/// # Errors
/// [`OkfError::Database`].
#[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "unlocks.live"))]
pub async fn live(
    executor: impl PgExecutor<'_>,
    now: DateTime<Utc>,
) -> Result<Vec<LiveUnlock>, OkfError> {
    Ok(sqlx::query_as::<_, LiveUnlock>(
        "SELECT u.id, u.user_id, c.name AS catalog, u.expires_at \
         FROM unlocks u JOIN catalogs c ON c.id = u.catalog_id \
         WHERE u.ended_at IS NULL AND u.expires_at > $1 AND u.user_id IS NOT NULL \
         ORDER BY u.id",
    )
    .bind(now)
    .fetch_all(executor)
    .await?)
}

/// Counts one request admitted under an unlock (spec Part 6: "reads,
/// writes and exports are counted on the unlock"). Run in the background
/// ([`crate::app::App::note_access`]); the unlock id was fixed at admission,
/// so an unlock deleted with its catalog meanwhile is simply not found and
/// nothing is updated.
///
/// # Arguments
/// * `pool` - the database.
/// * `id` - the unlock's id.
/// * `kind` - which count to add one to.
///
/// # Errors
/// [`OkfError::Database`].
#[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "unlocks.tally"))]
pub async fn tally(pool: &PgPool, id: i64, kind: AccessKind) -> Result<(), OkfError> {
    // One fixed statement per column: a column name cannot be a bind
    // parameter. Every variant is named, so a new kind must pick a column.
    let sql = match kind {
        AccessKind::Read => "UPDATE unlocks SET reads = reads + 1 WHERE id = $1",
        AccessKind::Write => "UPDATE unlocks SET writes = writes + 1 WHERE id = $1",
        AccessKind::Export => "UPDATE unlocks SET exports = exports + 1 WHERE id = $1",
    };
    sqlx::query(sql).bind(id).execute(pool).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{test_db, TEST_TEAM, TEST_USER};
    use crate::http::api::testing::wait_for_lock_waiters;
    use crate::store::audit::{testing, Source, Via};
    use crate::store::catalogs::NewCatalog;
    use crate::store::users::{self, AdminChange, NewUser};
    use std::time::Duration;

    fn now() -> DateTime<Utc> {
        "2026-11-13T12:00:00Z".parse().unwrap()
    }

    fn policy() -> UnlockPolicy {
        UnlockPolicy {
            max_hours: 4,
            notify_owners: true,
        }
    }

    /// Makes `name` an admin user.
    async fn admin(pool: &PgPool, name: &str) {
        let user = NewUser {
            name,
            display_name: "",
        };
        users::create(pool, &user, &AuditActor::cli(), now())
            .await
            .unwrap();
        let on = AdminChange {
            on: true,
            allow_last: true,
        };
        users::set_admin(pool, name, on, &AuditActor::cli(), now())
            .await
            .unwrap();
    }

    /// Admin user `staff`, an email for `TEST_USER` (the owner of
    /// `TEST_TEAM`), and catalog `main` owned by `TEST_TEAM`.
    async fn staff_and_main(pool: &PgPool) {
        admin(pool, "staff").await;
        let email = Some("owner@example.com");
        users::set_email(pool, TEST_USER, email, &AuditActor::cli(), now())
            .await
            .unwrap();
        let main = NewCatalog {
            name: "main",
            description: "",
            owner_team: TEST_TEAM,
        };
        catalogs::create(pool, &main, &AuditActor::cli(), now())
            .await
            .unwrap();
    }

    fn new(hours: u32) -> NewUnlock<'static> {
        NewUnlock {
            catalog: "main",
            user: "staff",
            category: Category::Support,
            reason: "ticket 12",
            hours,
        }
    }

    #[test]
    fn an_unlock_is_live_until_it_expires_or_ends_and_ends_at_the_earlier() {
        let record = |ended_at: Option<DateTime<Utc>>| UnlockRecord {
            id: 1,
            catalog: "main".to_string(),
            owner_team: None,
            user: Some("staff".to_string()),
            person: "staff".to_string(),
            category: Category::Support,
            reason: "ticket 12".to_string(),
            created_at: now(),
            expires_at: now() + TimeDelta::hours(2),
            ended_at,
            reads: 0,
            writes: 0,
            exports: 0,
        };
        let open = record(None);
        let ended = record(Some(now() + TimeDelta::minutes(5)));
        let expiry = now() + TimeDelta::hours(2);

        assert_eq!(open.state(now()), UnlockState::Live);
        assert_eq!(open.state(expiry), UnlockState::Expired);
        assert_eq!(
            ended.state(now() + TimeDelta::minutes(10)),
            UnlockState::Ended
        );
        assert_eq!(ended.state(expiry), UnlockState::Ended);
        assert_eq!(open.ends(), expiry);
        assert_eq!(ended.ends(), now() + TimeDelta::minutes(5));
        assert_eq!(
            serde_json::to_value(UnlockState::Expired).unwrap(),
            "expired"
        );
        assert_eq!(UnlockState::Ended.as_str(), "ended");
    }

    #[test]
    fn categories_round_trip_their_spelling() {
        for category in Category::ALL {
            assert_eq!(Category::parse(category.as_str()), Some(category));
            let json = serde_json::to_value(category).unwrap();
            assert_eq!(json, category.as_str());
        }
        assert_eq!(Category::parse("audit"), None);
        assert_eq!(Category::DeletionRequest.label(), "a deletion request");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_unlock_is_recorded_mails_the_owners_and_can_be_ended_once() {
        let db = test_db().await;
        staff_and_main(&db.pool).await;
        let start = catalogs::admin_generation(&db.pool).await.unwrap();

        let made = create(&db.pool, &new(2), &policy(), &AuditActor::cli(), now())
            .await
            .unwrap();
        let later = now() + TimeDelta::minutes(5);
        let ended = end(&db.pool, made.id, &AuditActor::cli(), later)
            .await
            .unwrap();
        let again = end(&db.pool, made.id, &AuditActor::cli(), later).await;
        let unknown = end(&db.pool, made.id + 100, &AuditActor::cli(), later).await;

        assert_eq!(made.expires_at, now() + TimeDelta::hours(2));
        assert_eq!(made.category, Category::Support);
        assert_eq!(made.catalog, "main");
        assert_eq!(made.owner_team.as_deref(), Some(TEST_TEAM));
        assert_eq!(
            (made.user.as_deref(), made.person.as_str()),
            (Some("staff"), "staff")
        );
        assert_eq!(made.reason, "ticket 12");
        assert_eq!(ended.ended_at, Some(later));
        assert!(matches!(again, Err(OkfError::Refused { .. })), "{again:?}");
        assert!(
            matches!(unknown, Err(OkfError::NotFound { .. })),
            "{unknown:?}"
        );
        let unlocked = testing::entries(&db.pool, "catalog.unlocked").await;
        assert_eq!(unlocked.len(), 1);
        assert_eq!(
            unlocked[0].details,
            serde_json::json!({
                "unlock": made.id,
                "category": "support",
                "reason": "ticket 12",
                "starts": "2026-11-13T12:00:00Z",
                "expires": "2026-11-13T14:00:00Z",
            })
        );
        assert_eq!(unlocked[0].target.as_deref(), Some("main"));
        assert_eq!(unlocked[0].person.as_deref(), Some("staff"));
        assert_eq!(unlocked[0].teams, [TEST_TEAM]);
        let ended_entries = testing::entries(&db.pool, "catalog.unlock_ended").await;
        assert_eq!(ended_entries.len(), 1);
        assert_eq!(
            ended_entries[0].details,
            serde_json::json!({"unlock": made.id})
        );
        assert_eq!(ended_entries[0].person.as_deref(), Some("staff"));
        // The staff member is an admin: owners will not see who it was.
        let operator_targets: Vec<bool> = sqlx::query_scalar(
            "SELECT target_operator FROM audit_entries \
             WHERE action IN ('catalog.unlocked', 'catalog.unlock_ended') ORDER BY id",
        )
        .fetch_all(&db.pool)
        .await
        .unwrap();
        assert_eq!(operator_targets, [true, true]);
        // Created and ended: two bumps; the refusals bump nothing.
        assert_eq!(
            catalogs::admin_generation(&db.pool).await.unwrap(),
            start + 2
        );
        let mail = crate::mail::queue::list(&db.pool).await.unwrap();
        assert_eq!(mail.len(), 1);
        assert_eq!(mail[0].to, "owner@example.com");
        assert_eq!(mail[0].template, "unlocked");
    }

    /// The person and operator marking of the one entry with `action`.
    async fn entry_person(pool: &PgPool, action: &str) -> (Option<i64>, bool) {
        sqlx::query_as(
            "SELECT target_person_id, target_operator FROM audit_entries WHERE action = $1",
        )
        .bind(action)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    /// What happens to the unlocked admin between the unlock and its end.
    #[derive(Debug, Clone, Copy)]
    enum Meanwhile {
        Demoted,
        Deleted,
    }

    /// Unlocks `main` for staff, demotes or deletes staff, ends the unlock,
    /// and answers both entries' person and operator marking plus the
    /// ended record's person.
    async fn ended_after(pool: &PgPool, meanwhile: Meanwhile) -> [(Option<i64>, bool); 2] {
        let made = create(pool, &new(2), &policy(), &AuditActor::cli(), now())
            .await
            .unwrap();
        match meanwhile {
            Meanwhile::Demoted => {
                let off = AdminChange {
                    on: false,
                    allow_last: true,
                };
                users::set_admin(pool, "staff", off, &AuditActor::cli(), now())
                    .await
                    .unwrap();
            }
            Meanwhile::Deleted => {
                users::delete(
                    pool,
                    "staff",
                    users::DeleteOptions::OPERATOR,
                    &AuditActor::cli(),
                    now(),
                )
                .await
                .unwrap();
            }
        }
        let ended = end(
            pool,
            made.id,
            &AuditActor::cli(),
            now() + TimeDelta::minutes(5),
        )
        .await
        .unwrap();
        let expected_person = match meanwhile {
            Meanwhile::Demoted => "staff".to_string(),
            Meanwhile::Deleted => {
                let person: i64 = sqlx::query_scalar("SELECT person_id FROM unlocks WHERE id = $1")
                    .bind(made.id)
                    .fetch_one(pool)
                    .await
                    .unwrap();
                format!("deleted user #{person}")
            }
        };
        assert_eq!(ended.person, expected_person, "{meanwhile:?}");
        [
            entry_person(pool, "catalog.unlocked").await,
            entry_person(pool, "catalog.unlock_ended").await,
        ]
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_unlock_ended_after_its_staff_is_demoted_still_hides_who_it_was() {
        let db = test_db().await;
        staff_and_main(&db.pool).await;

        let [unlocked, ended] = ended_after(&db.pool, Meanwhile::Demoted).await;

        assert!(unlocked.0.is_some() && unlocked.1, "{unlocked:?}");
        assert_eq!(
            ended, unlocked,
            "the end names the same person, as operator"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_unlock_ended_after_its_staff_is_deleted_keeps_the_person_and_marking() {
        let db = test_db().await;
        staff_and_main(&db.pool).await;

        let [unlocked, ended] = ended_after(&db.pool, Meanwhile::Deleted).await;

        assert!(unlocked.0.is_some() && unlocked.1, "{unlocked:?}");
        assert_eq!(
            ended, unlocked,
            "the end names the same person, as operator"
        );
        let shown = testing::entries(&db.pool, "catalog.unlock_ended").await;
        assert_eq!(
            shown[0]
                .person
                .as_deref()
                .map(|name| name.starts_with("deleted user #")),
            Some(true),
            "{shown:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_unlock_is_for_an_admin_user_within_the_limit_with_a_reason() {
        let db = test_db().await;
        staff_and_main(&db.pool).await;
        let cli = AuditActor::cli();

        let too_long = create(&db.pool, &new(5), &policy(), &cli, now()).await;
        let zero = create(&db.pool, &new(0), &policy(), &cli, now()).await;
        let mut refused_reasons = Vec::new();
        for reason in ["  ", "a\0b", &"x".repeat(MAX_REASON_CHARS + 1)] {
            let unlock = NewUnlock { reason, ..new(1) };
            refused_reasons.push(create(&db.pool, &unlock, &policy(), &cli, now()).await);
        }
        let not_admin = NewUnlock {
            user: TEST_USER,
            ..new(1)
        };
        let not_admin = create(&db.pool, &not_admin, &policy(), &cli, now()).await;
        let nobody = NewUnlock {
            user: "nobody\0",
            ..new(1)
        };
        let nobody = create(&db.pool, &nobody, &policy(), &cli, now()).await;
        let no_catalog = NewUnlock {
            catalog: "gone\0",
            ..new(1)
        };
        let no_catalog = create(&db.pool, &no_catalog, &policy(), &cli, now()).await;
        let quiet = UnlockPolicy {
            notify_owners: false,
            ..policy()
        };
        let quiet = create(&db.pool, &new(4), &quiet, &cli, now()).await;

        for result in [too_long, zero] {
            assert!(
                matches!(
                    result,
                    Err(OkfError::LimitExceeded {
                        limit: "unlock_max_hours",
                        ..
                    })
                ),
                "{result:?}"
            );
        }
        for result in refused_reasons {
            assert!(
                matches!(
                    result,
                    Err(OkfError::LimitExceeded {
                        limit: "reason",
                        ..
                    })
                ),
                "{result:?}"
            );
        }
        assert!(
            matches!(not_admin, Err(OkfError::Refused { .. })),
            "{not_admin:?}"
        );
        assert!(
            matches!(nobody, Err(OkfError::UnknownName { .. })),
            "{nobody:?}"
        );
        assert!(
            matches!(no_catalog, Err(OkfError::CatalogNotSelected { .. })),
            "{no_catalog:?}"
        );
        assert!(quiet.is_ok(), "{quiet:?}");
        assert!(crate::mail::queue::list(&db.pool).await.unwrap().is_empty());
        // Only the one that was made is recorded.
        assert_eq!(
            testing::entries(&db.pool, "catalog.unlocked").await.len(),
            1
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn live_unlocks_are_the_unended_unexpired_ones() {
        let db = test_db().await;
        staff_and_main(&db.pool).await;
        let cli = AuditActor::cli();
        let ended = create(&db.pool, &new(1), &policy(), &cli, now())
            .await
            .unwrap();
        end(&db.pool, ended.id, &cli, now()).await.unwrap();
        let live_one = create(&db.pool, &new(1), &policy(), &cli, now())
            .await
            .unwrap();
        let half_hour = now() + TimeDelta::minutes(30);

        let live_now = live(&db.pool, half_hour).await.unwrap();
        let later = live(&db.pool, now() + TimeDelta::hours(2)).await.unwrap();
        let listed = |filter: UnlockFilter<'static>| {
            let pool = db.pool.clone();
            async move {
                list(&pool, &filter)
                    .await
                    .unwrap()
                    .iter()
                    .map(|unlock| unlock.id)
                    .collect::<Vec<_>>()
            }
        };

        assert_eq!(
            live_now.iter().map(|unlock| unlock.id).collect::<Vec<_>>(),
            [live_one.id]
        );
        assert_eq!(live_now[0].catalog, "main");
        assert!(later.is_empty());
        // Newest first; the filters narrow it, and a malformed name is no
        // database error.
        assert_eq!(
            listed(UnlockFilter::default()).await,
            [live_one.id, ended.id]
        );
        let live_at = UnlockFilter {
            live_at: Some(half_hour),
            ..UnlockFilter::default()
        };
        assert_eq!(listed(live_at).await, [live_one.id]);
        let by_team = UnlockFilter {
            team: Some(TEST_TEAM),
            catalog: Some("main"),
            ..UnlockFilter::default()
        };
        assert_eq!(listed(by_team).await.len(), 2);
        for filter in [
            UnlockFilter {
                team: Some("other"),
                ..UnlockFilter::default()
            },
            UnlockFilter {
                catalog: Some("main\0"),
                ..UnlockFilter::default()
            },
        ] {
            assert!(listed(filter).await.is_empty());
        }
    }

    /// A console-session actor for the user called `name`.
    async fn session_actor(pool: &PgPool, name: &str) -> AuditActor {
        let user_id: String = sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
            .bind(name)
            .fetch_one(pool)
            .await
            .unwrap();
        AuditActor {
            source: Source::Console,
            credential: None,
            user_id: Some(user_id),
            label: "console session".to_string(),
            via: Via::Session,
            operator: true,
            provisioner: false,
            ip: None,
        }
    }

    /// How many unlocks exist, and how many `catalog.unlocked` entries.
    async fn unlocks_and_entries(pool: &PgPool) -> (i64, i64) {
        let unlocks: i64 = sqlx::query_scalar("SELECT count(*) FROM unlocks")
            .fetch_one(pool)
            .await
            .unwrap();
        let entries = testing::entries(pool, "catalog.unlocked").await.len();
        (unlocks, i64::try_from(entries).unwrap())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn only_an_admin_user_or_the_command_line_creates_an_unlock() {
        // Issue #447: TEST_USER is not an admin user, so it may not unlock
        // `main` for admin `staff`; nothing is written.
        let db = test_db().await;
        staff_and_main(&db.pool).await;
        let customer = session_actor(&db.pool, TEST_USER).await;
        let staff = session_actor(&db.pool, "staff").await;

        let refused = create(&db.pool, &new(1), &policy(), &customer, now()).await;
        let written = unlocks_and_entries(&db.pool).await;
        let by_staff = create(&db.pool, &new(1), &policy(), &staff, now()).await;
        let by_cli = create(&db.pool, &new(1), &policy(), &AuditActor::cli(), now()).await;

        assert!(
            matches!(refused, Err(OkfError::Forbidden { .. })),
            "{refused:?}"
        );
        assert_eq!(written, (0, 0));
        assert!(by_staff.is_ok(), "{by_staff:?}");
        assert!(by_cli.is_ok(), "{by_cli:?}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_unlock_waiting_on_its_creators_demotion_is_refused() {
        // Admin `staff` unlocks `main` for admin `boss` while `staff` is
        // being demoted. The demotion holds every admin row `FOR UPDATE`
        // and is paused at its generation bump (the `server_state` row,
        // held here); the unlock queues on the users' rows. Once the
        // demotion commits, the unlock must see that its creator is no
        // longer staff: a check made before the lock would let it through.
        let db = test_db().await;
        staff_and_main(&db.pool).await;
        admin(&db.pool, "boss").await;
        let actor = session_actor(&db.pool, "staff").await;
        let mut holder = db.pool.begin().await.unwrap();
        sqlx::query("SELECT 1 FROM server_state FOR UPDATE")
            .execute(&mut *holder)
            .await
            .unwrap();
        let demotion = tokio::spawn({
            let pool = db.pool.clone();
            async move {
                let off = AdminChange {
                    on: false,
                    allow_last: false,
                };
                users::set_admin(&pool, "staff", off, &AuditActor::cli(), now()).await
            }
        });
        wait_for_lock_waiters(&db.pool, 1).await;
        let unlock = tokio::spawn({
            let pool = db.pool.clone();
            async move {
                let new = NewUnlock {
                    user: "boss",
                    ..new(1)
                };
                create(&pool, &new, &policy(), &actor, now()).await
            }
        });
        wait_for_lock_waiters(&db.pool, 2).await;
        // Dropping the holder rolls it back and releases the row.
        drop(holder);
        let wait = Duration::from_secs(30);
        let demoted = tokio::time::timeout(wait, demotion)
            .await
            .expect("the demotion finished within 30 s")
            .expect("the demotion task did not panic");
        let unlocked = tokio::time::timeout(wait, unlock)
            .await
            .expect("the unlock finished within 30 s")
            .expect("the unlock task did not panic");

        assert!(demoted.unwrap(), "staff was an admin before the demotion");
        assert!(
            matches!(unlocked, Err(OkfError::Forbidden { .. })),
            "{unlocked:?}"
        );
        assert_eq!(unlocks_and_entries(&db.pool).await, (0, 0));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_unlock_by_an_admin_and_a_concurrent_demotion_of_its_staff_both_finish() {
        // Two admins: the lower id acts (a console session), the higher is
        // unlocked. `users::set_admin` locks every admin by id, so an unlock
        // that held its staff row before the actor's (the plan's first
        // draft) deadlocked against it.
        let db = test_db().await;
        staff_and_main(&db.pool).await;
        admin(&db.pool, "boss").await;
        let ids: Vec<(String, String)> = sqlx::query_as(
            "SELECT id, name FROM users WHERE name IN ('staff', 'boss') ORDER BY id",
        )
        .fetch_all(&db.pool)
        .await
        .unwrap();
        let (_, actor_name) = &ids[0];
        let (_, target) = ids[1].clone();
        let actor = session_actor(&db.pool, actor_name).await;
        // The catalog row, which the unlock's foreign key needs after it
        // has locked its users.
        let mut holder = db.pool.begin().await.unwrap();
        sqlx::query("SELECT 1 FROM catalogs WHERE name = 'main' FOR UPDATE")
            .execute(&mut *holder)
            .await
            .unwrap();
        let unlock = tokio::spawn({
            let pool = db.pool.clone();
            let target = target.clone();
            async move {
                let new = NewUnlock {
                    user: &target,
                    ..new(1)
                };
                create(&pool, &new, &policy(), &actor, now()).await
            }
        });
        wait_for_lock_waiters(&db.pool, 1).await;
        let demotion = tokio::spawn({
            let pool = db.pool.clone();
            async move {
                let off = AdminChange {
                    on: false,
                    allow_last: false,
                };
                users::set_admin(&pool, &target, off, &AuditActor::cli(), now()).await
            }
        });
        wait_for_lock_waiters(&db.pool, 2).await;
        // Dropping the holder rolls it back and releases the catalog row.
        drop(holder);
        let wait = Duration::from_secs(30);
        let unlocked = tokio::time::timeout(wait, unlock)
            .await
            .expect("the unlock finished within 30 s")
            .expect("the unlock task did not panic");
        let demoted = tokio::time::timeout(wait, demotion)
            .await
            .expect("the demotion finished within 30 s")
            .expect("the demotion task did not panic");

        // The unlock held the staff row first, so it was made for an admin;
        // the demotion then went through.
        assert!(unlocked.is_ok(), "{unlocked:?}");
        assert!(demoted.unwrap(), "staff was an admin before the demotion");
    }
}
