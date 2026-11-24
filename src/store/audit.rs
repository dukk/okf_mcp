//! The audit log (spec "Part 5"): one row per administrative change,
//! catalog write, export and admin read, written by [`record`] inside the
//! transaction of the change it records. Immutable columns never hold a
//! person's name or a reusable name: people are `audit_people` ids, teams
//! `audit_teams` ids (one per `teams.id`, never reused), credentials
//! `audit_credentials` ids; [`forget_person`] clears what identifies a
//! deleted user in those rows. Never a secret: no key, token, password,
//! link, session id or email address goes into an entry.

use std::net::IpAddr;

use chrono::{DateTime, DurationRound, TimeDelta, Utc};
use sqlx::{PgConnection, PgPool};

use crate::auth::{CatalogScope, CredentialKind, Principal};
use crate::cache::labels::LabelLookup;
use crate::error::OkfError;
use crate::http::requester::Requester;
use crate::labels::directory::{Labels, Wanted};
use crate::labels::user_label;
use crate::store::keys::Access;
use crate::stores::{AccountDb, CatalogDb, StoreName, Stores};

/// How many days entries are kept with no `audit_retention_days` override
/// (the append-only trigger in `migrations/accounts/0012_audit.sql` uses the same).
pub const DEFAULT_RETENTION_DAYS: u32 = 400;

/// The shortest retention: the setting refuses less, and the trigger
/// keeps entries this long whatever the stored value says.
pub const MIN_RETENTION_DAYS: u32 = 30;

/// The longest retention the setting takes (ten years; plan Decision 13).
pub const MAX_RETENTION_DAYS: u32 = 3650;

/// Where a change came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// `okf-mcp` on the server's command line.
    Cli,
    /// A bearer key or OAuth grant (REST or MCP).
    Api,
    /// A signed-in console session.
    Console,
    /// The server itself (an OAuth code redeemed, a maintenance run ending).
    System,
}

impl Source {
    /// The stored spelling.
    ///
    /// # Returns
    /// `"cli"`, `"api"`, `"console"` or `"system"`.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "trivial accessor or conversion: maps a value, no I/O"
        )
    )]
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Cli => "cli",
            Source::Api => "api",
            Source::Console => "console",
            Source::System => "system",
        }
    }
}

/// What kind of credential acted: what an entry's immutable `actor_label`
/// says, so it never needs a person's name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    /// A console session.
    Session,
    /// An API key.
    Key,
    /// An OAuth grant.
    Grant,
    /// The command line.
    Cli,
    /// The server itself.
    System,
}

/// Who made a change, as an entry records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditActor {
    /// Where it came from.
    pub source: Source,
    /// The key or grant id; `None` for a session, the CLI and the system.
    pub credential: Option<String>,
    /// The acting user's `users.id`, if any (a legacy key has none).
    pub user_id: Option<String>,
    /// The credential's actor, the OS user, or a system label. For a
    /// credential it goes to the mutable `audit_credentials.label`, never
    /// into the entry ([`AuditActor::stored_label`]).
    pub label: String,
    /// What kind of credential acted.
    pub via: Via,
    /// An admin credential or a provisioner: owners see "an operator".
    pub operator: bool,
    /// A provisioner key: store functions refuse it an admin target.
    pub provisioner: bool,
    /// The client address of a web request.
    pub ip: Option<IpAddr>,
}

impl AuditActor {
    /// The command line: the operating-system user, as the operator.
    ///
    /// # Returns
    /// The actor; its label is `$USER` (else `$LOGNAME`, else `cli`).
    #[tracing::instrument(level = "debug", skip_all)]
    pub fn cli() -> Self {
        // `or_else` tries the next variable only if the first is missing.
        let label = std::env::var("USER")
            .or_else(|_| std::env::var("LOGNAME"))
            .ok()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| "cli".to_string());
        Self {
            source: Source::Cli,
            credential: None,
            user_id: None,
            label,
            via: Via::Cli,
            operator: true,
            provisioner: false,
            ip: None,
        }
    }

    /// The server itself.
    ///
    /// # Arguments
    /// * `label` - what acted (`"oauth"`, `"maintenance"`).
    ///
    /// # Returns
    /// The actor.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "trivial accessor or conversion: maps a value, no I/O"
        )
    )]
    pub fn system(label: &str) -> Self {
        Self {
            source: Source::System,
            credential: None,
            user_id: None,
            label: label.to_string(),
            via: Via::System,
            operator: false,
            provisioner: false,
            ip: None,
        }
    }

    /// The caller of a web request.
    ///
    /// # Arguments
    /// * `principal` - the credential.
    /// * `requester` - its IP and source.
    ///
    /// # Returns
    /// The actor. A console session has no credential id: `session:<name>`
    /// would name the person, which entries do only through `audit_people`.
    #[tracing::instrument(level = "debug", skip_all)]
    pub fn of(principal: &Principal, requester: &Requester) -> Self {
        let (credential, via) = match principal.kind {
            CredentialKind::Session { .. } => (None, Via::Session),
            CredentialKind::ApiKey => (Some(principal.key_id.clone()), Via::Key),
            CredentialKind::OAuth { .. } => (Some(principal.key_id.clone()), Via::Grant),
        };
        Self {
            source: requester.source,
            credential,
            user_id: principal.user_id.clone(),
            // A session's `actor` is its user's name: it is kept here for
            // `settings.updated_by`, never written into an entry.
            label: principal.actor.clone(),
            via,
            operator: principal.access == Access::Admin || principal.provisioner,
            provisioner: principal.provisioner,
            ip: requester.ip,
        }
    }

    /// The entry's immutable `actor_label`: never a person's name. Every
    /// key and grant is named by its kind, whether or not it has a user now:
    /// a key with no user can be assigned one later, and its own label (a
    /// legacy key's is often a person's device or name) must stay where
    /// [`forget_person`] can clear it, in the mutable `audit_credentials`.
    /// Only the CLI's operating-system user (operator staff, not an okf-mcp
    /// user) and the system keep their label.
    ///
    /// # Returns
    /// The label to store.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "trivial accessor or conversion: maps a value, no I/O"
        )
    )]
    pub fn stored_label(&self) -> String {
        match self.via {
            Via::Session => "console session".to_string(),
            Via::Key => "api key".to_string(),
            Via::Grant => "oauth grant".to_string(),
            Via::Cli => format!("cli:{}", self.label),
            Via::System => format!("system:{}", self.label),
        }
    }
}

// One list of actions, so the enum, `as_str` and `ALL` cannot drift apart.
// `macro_rules!` writes the three from it at compile time.
macro_rules! actions {
    ($($variant:ident => $name:literal),+ $(,)?) => {
        /// What an entry records (spec "What is recorded").
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum Action { $(
            #[doc = concat!("`", $name, "`.")]
            $variant
        ),+ }

        impl Action {
            /// Every action.
            pub const ALL: &'static [Action] = &[$(Action::$variant),+];

            /// The stored name.
            ///
            /// # Returns
            /// For example `"user.created"`.
            #[cfg_attr(
                dylint_lib = "hyp_tracing",
                allow(uninstrumented_fn, reason = "trivial accessor or conversion: maps a value, no I/O")
            )]
            pub fn as_str(self) -> &'static str {
                match self { $(Action::$variant => $name),+ }
            }

            /// Reads a stored name (for the `action` filter).
            ///
            /// # Arguments
            /// * `name` - a stored name such as `"user.created"`.
            ///
            /// # Returns
            /// The action, or `None` for an unknown name.
            #[cfg_attr(
                dylint_lib = "hyp_tracing",
                allow(uninstrumented_fn, reason = "trivial accessor or conversion: maps a value, no I/O")
            )]
            pub fn parse(name: &str) -> Option<Self> {
                Self::ALL.iter().copied().find(|action| action.as_str() == name)
            }
        }
    };
}

actions! {
    UserCreated => "user.created",
    UserDeleted => "user.deleted",
    UserEmailChanged => "user.email_changed",
    UserDisplayNameChanged => "user.display_name_changed",
    UserAdminChanged => "user.admin_changed",
    UserInvited => "user.invited",
    UserEntitlementSet => "user.entitlement_set",
    UserEntitlementRemoved => "user.entitlement_removed",
    UserSessionsRevoked => "user.sessions_revoked",
    TeamCreated => "team.created",
    TeamDeleted => "team.deleted",
    TeamMemberAdded => "team.member_added",
    TeamRoleChanged => "team.role_changed",
    TeamMemberRemoved => "team.member_removed",
    TeamSuspended => "team.suspended",
    TeamUnsuspended => "team.unsuspended",
    TeamDisplayNameChanged => "team.display_name_changed",
    CatalogCreated => "catalog.created",
    CatalogDeleted => "catalog.deleted",
    CatalogOwnerChanged => "catalog.owner_changed",
    CatalogMemberAccessChanged => "catalog.member_access_changed",
    CatalogDescriptionChanged => "catalog.description_changed",
    CatalogDisplayNameChanged => "catalog.display_name_changed",
    CatalogImported => "catalog.imported",
    CatalogRepaired => "catalog.repaired",
    CatalogMaintenanceChanged => "catalog.maintenance_changed",
    KeyCreated => "key.created",
    KeyRevoked => "key.revoked",
    KeyCatalogGranted => "key.catalog_granted",
    KeyCatalogUngranted => "key.catalog_ungranted",
    KeyUserAssigned => "key.user_assigned",
    KeyReportFlagChanged => "key.report_flag_changed",
    OauthCodeCreated => "oauth.code_created",
    OauthCodeRevoked => "oauth.code_revoked",
    OauthGrantCreated => "oauth.grant_created",
    OauthGrantRevoked => "oauth.grant_revoked",
    SettingChanged => "setting.changed",
    SettingCleared => "setting.cleared",
    CatalogPublished => "catalog.published",
    CatalogReverted => "catalog.reverted",
    CatalogChangesetDiscarded => "catalog.changeset_discarded",
    CatalogExported => "catalog.exported",
    CatalogAdminRead => "catalog.admin_read",
    CatalogMaintenanceRun => "catalog.maintenance_run",
    CatalogUnlocked => "catalog.unlocked",
    CatalogUnlockEnded => "catalog.unlock_ended",
    UserLookedUp => "user.looked_up",
    ProvisionerRefused => "provisioner.refused",
    StoreAdded => "store.added",
    SettingDefaultStoreChanged => "setting.default_store_changed",
    TeamStorePinned => "team.store_pinned",
    TeamStoreUnpinned => "team.store_unpinned",
}

/// What an entry is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target<'a> {
    /// A user, by `users.id` (stored only as a person).
    User(&'a str),
    /// A team, by name (stored as its `audit_teams` id).
    Team(&'a str),
    /// A catalog, by name.
    Catalog(&'a str),
    /// An API key, by id.
    Key(&'a str),
    /// An OAuth grant, by id.
    Grant(&'a str),
    /// A sign-in code, by id.
    Code(&'a str),
    /// A setting, by key.
    Setting(&'a str),
    /// An unlock, by id.
    Unlock(i64),
    /// A catalog store, by name.
    Store(&'a str),
}

/// Where a target is stored: its kind, and at most one of a name, a team
/// (by name, resolved to its `audit_teams` id) or a credential id (resolved
/// to its `audit_credentials` id); a user is stored as a person only.
struct TargetParts<'a> {
    kind: &'static str,
    name: Option<String>,
    team: Option<&'a str>,
    credential: Option<&'a str>,
}

impl<'a> Target<'a> {
    /// The stored parts.
    #[tracing::instrument(level = "trace", skip_all)]
    fn parts(&self) -> TargetParts<'a> {
        let only = |kind| TargetParts {
            kind,
            name: None,
            team: None,
            credential: None,
        };
        let plain = |kind, name: String| TargetParts {
            name: Some(name),
            ..only(kind)
        };
        let credential = |kind, id: &'a str| TargetParts {
            credential: Some(id),
            ..only(kind)
        };
        // `*self` copies the `Copy` enum out of the reference, so each arm
        // binds the borrowed `&'a str` itself rather than a `&&str`.
        match *self {
            Target::User(_) => only("user"),
            Target::Team(name) => TargetParts {
                team: Some(name),
                ..only("team")
            },
            Target::Catalog(name) => plain("catalog", name.to_string()),
            Target::Key(id) => credential("key", id),
            Target::Grant(id) => credential("grant", id),
            Target::Code(id) => credential("code", id),
            Target::Setting(key) => plain("setting", key.to_string()),
            Target::Unlock(id) => plain("unlock", id.to_string()),
            Target::Store(name) => plain("store", name.to_string()),
        }
    }
}

/// One entry to record.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry<'a> {
    /// What happened.
    pub action: Action,
    /// What it happened to.
    pub target: Target<'a>,
    /// A user the entry is also about (`users.id`): a member added to a
    /// team, the new user of a key. Ignored for a [`Target::User`].
    pub person: Option<&'a str>,
    /// `audit_teams` ids ([`team_ref`], [`catalog_teams`], [`key_teams`]).
    pub teams: &'a [i64],
    /// A small JSON object; never a secret or an email address.
    pub details: serde_json::Value,
}

/// Writes one entry inside the caller's transaction.
///
/// # Arguments
/// * `conn` - the change's transaction (`&mut tx`).
/// * `actor` - who.
/// * `entry` - what.
/// * `now` - when (server clock).
///
/// # Returns
/// The entry's id.
///
/// # Errors
/// [`OkfError::Database`], which fails the caller's change with it
/// (including a [`Target::Team`] that names no team, which the table's
/// target-shape check refuses).
#[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "audit.record"))]
pub async fn record(
    conn: &mut PgConnection,
    actor: &AuditActor,
    entry: &Entry<'_>,
    now: DateTime<Utc>,
) -> Result<i64, OkfError> {
    insert(conn, actor, entry, None, now).await
}

/// An entry's person given as the audit person fixed when something began
/// (an unlock's `person_id`), with the operator marking it had then, so a
/// later entry about the same thing names the same person and hides it
/// from owners the same way, whatever the user's admin flag is now and even
/// once the user is deleted ([`record_for_person`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FixedPerson {
    /// The `audit_people` id.
    pub person_id: i64,
    /// Whether the person was operator staff (an admin user) then.
    pub operator: bool,
}

/// [`record`], with the entry's person given as a [`FixedPerson`] instead of
/// a `users.id` resolved now (`entry.person` is ignored). The person's
/// `audit_people` row is read as it is and never refreshed or locked.
///
/// # Arguments
/// * `conn` - the change's transaction (`&mut tx`).
/// * `actor` - who.
/// * `entry` - what; its target must not be a user, a key, a grant or a
///   code (whose person and marking come from the user or owner now).
/// * `person` - the person and its operator marking.
/// * `now` - when (server clock).
///
/// # Returns
/// The entry's id.
///
/// # Errors
/// [`OkfError::Refused`] for a user, key, grant or code target, before any
/// SQL; otherwise as [`record`].
#[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "audit.record_for_person"))]
pub async fn record_for_person(
    conn: &mut PgConnection,
    actor: &AuditActor,
    entry: &Entry<'_>,
    person: FixedPerson,
    now: DateTime<Utc>,
) -> Result<i64, OkfError> {
    // Such a target names its own person; a fixed one would contradict it.
    if matches!(
        entry.target,
        Target::User(_) | Target::Key(_) | Target::Grant(_) | Target::Code(_)
    ) {
        return Err(OkfError::Refused {
            reason: format!(
                "an entry about {:?} names its own person, not a fixed one",
                entry.target
            ),
        });
    }
    insert(conn, actor, entry, Some(person), now).await
}

/// The one `INSERT` behind [`record`] and [`record_for_person`].
#[tracing::instrument(level = "trace", skip_all, fields(db.system.name = "postgresql", db.operation.name = "audit.insert"))]
async fn insert(
    conn: &mut PgConnection,
    actor: &AuditActor,
    entry: &Entry<'_>,
    fixed: Option<FixedPerson>,
    now: DateTime<Utc>,
) -> Result<i64, OkfError> {
    let target = entry.target.parts();
    let person = match (entry.target, fixed) {
        // A fixed person is never resolved from a user now (and
        // `record_for_person` refuses a user target).
        (_, Some(_)) => None,
        (Target::User(user_id), None) => Some(user_id),
        (_, None) => entry.person,
    };
    // The subquery resolves both people once (`audit_person` creates or
    // refreshes a row; NULL in, NULL out); the credentials and the target
    // team resolve to their audit ids the same way. The actor's credential
    // label goes to `audit_credentials` (mutable), the entry gets only
    // `stored_label`. `target_operator`: the target person, or the owner of
    // a target key, grant or code (which `audit_credential` resolves the
    // same way), is an admin user now, so owners never see who it is. A
    // fixed person ($17, $18) replaces both the resolved target person and
    // that marking. `actor_operator` likewise: an operator credential, or
    // any credential of an admin user (a self-issued key, a grant), which
    // is still operator staff acting (issue #402).
    Ok(sqlx::query_scalar(
        "INSERT INTO audit_entries (at, action, source, actor_credential_ref, actor_person_id, \
             actor_label, actor_operator, client_ip, target_kind, target_name, target_team_ref, \
             target_credential_ref, target_person_id, target_operator, teams, details) \
         SELECT $1, $2, $3, audit_credential($4, p.actor, $5), p.actor, $6, \
                $7 OR COALESCE((SELECT is_admin FROM users WHERE id = $16), false), $8::inet, $9, \
                $10, audit_team_ref((SELECT id FROM teams WHERE name = $11)), \
                audit_credential($12, p.target, NULL), COALESCE($17::bigint, p.target), \
                COALESCE($18::boolean, (SELECT bool_or(is_admin) FROM users WHERE id IN ($13, \
                    (SELECT user_id FROM api_keys WHERE id = $12), \
                    (SELECT user_id FROM sign_in_codes WHERE id = $12))), false), $14, $15 \
         FROM (SELECT audit_person($16) AS actor, audit_person($13) AS target) p \
         RETURNING id",
    )
    .bind(now)
    .bind(entry.action.as_str())
    .bind(actor.source.as_str())
    .bind(&actor.credential)
    .bind(&actor.label)
    .bind(actor.stored_label())
    .bind(actor.operator)
    .bind(actor.ip.map(|ip| ip.to_string()))
    .bind(target.kind)
    .bind(target.name)
    .bind(target.team)
    .bind(target.credential)
    .bind(person)
    .bind(entry.teams)
    .bind(&entry.details)
    .bind(&actor.user_id)
    .bind(fixed.map(|fixed| fixed.person_id))
    .bind(fixed.map(|fixed| fixed.operator))
    // `&mut *conn` reborrows the connection, so `conn` stays usable after.
    .fetch_one(&mut *conn)
    .await?)
}

impl Action {
    /// Whether an entry of this action is written inside a catalog store's
    /// transaction ([`record_catalog`], plan decision P4) rather than on the
    /// account database ([`record`]). The six actions recorded where a
    /// catalog's documents, changesets or maintenance runs change: the
    /// entry then commits or rolls back with the change it records.
    ///
    /// # Returns
    /// `true` for `catalog.published`, `catalog.imported`, `catalog.repaired`,
    /// `catalog.reverted`, `catalog.changeset_discarded` and
    /// `catalog.maintenance_run`.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "trivial accessor or conversion: maps a value, no I/O"
        )
    )]
    pub fn is_catalog_event(self) -> bool {
        matches!(
            self,
            Action::CatalogPublished
                | Action::CatalogImported
                | Action::CatalogRepaired
                | Action::CatalogReverted
                | Action::CatalogChangesetDiscarded
                | Action::CatalogMaintenanceRun
        )
    }
}

/// The account database's side of a catalog entry, resolved BEFORE the
/// store transaction that writes the entry begins: the `audit_people`,
/// `audit_credentials` and `audit_teams` ids the store row holds as plain
/// values (a store has none of those tables), and the markings the account
/// `INSERT` of [`record`] computes in SQL. Built by [`resolve_actor`] (a
/// change's actor) or [`resolve_run`] (a maintenance run's), on the
/// change's account transaction, after [`lock_people`] and so in the
/// order every writer takes (`CatalogDb::begin`'s acquisition rule).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedActor {
    source: Source,
    credential_ref: Option<i64>,
    person_id: Option<i64>,
    label: String,
    operator: bool,
    ip: Option<IpAddr>,
    teams: Vec<i64>,
    target_person: Option<FixedPerson>,
}

impl ResolvedActor {
    /// The entry also names a user (a discard when the user is deleted): its
    /// `audit_people` id and operator marking, from [`resolve_person`].
    ///
    /// # Arguments
    /// * `person` - the person and marking.
    ///
    /// # Returns
    /// The actor, with the person set.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(uninstrumented_fn, reason = "sets one field, no I/O")
    )]
    pub fn about(mut self, person: Option<FixedPerson>) -> Self {
        self.target_person = person;
        self
    }
}

/// One entry for a catalog store: the six actions of
/// [`Action::is_catalog_event`].
#[derive(Debug, Clone, PartialEq)]
pub struct CatalogEntry<'a> {
    /// What happened; one of the six catalog events.
    pub action: Action,
    /// The catalog's id.
    pub catalog_id: i64,
    /// The catalog's name as it is now (kept in the entry: it outlives the
    /// catalog, plan decision P5).
    pub catalog: &'a str,
    /// A small JSON object; never a secret or an email address.
    pub details: serde_json::Value,
}

/// Resolves a change's actor and its catalog's owner team on the account
/// database, for [`record_catalog`] in the store transaction that follows.
/// The same lookups [`record`]'s `INSERT` makes in SQL (`audit_person`,
/// `audit_credential`, the operator marking of issue #402, [`catalog_teams`]),
/// so an entry says the same on either side.
///
/// # Arguments
/// * `conn` - the change's account transaction, with [`lock_people`] already
///   called for this actor and catalog.
/// * `actor` - who.
/// * `catalog_id` - the catalog the entry is about.
///
/// # Returns
/// The resolved ids and markings.
///
/// # Errors
/// [`OkfError::Database`].
#[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "audit.resolve_actor"))]
pub async fn resolve_actor(
    conn: &mut PgConnection,
    actor: &AuditActor,
    catalog_id: i64,
) -> Result<ResolvedActor, OkfError> {
    // One statement, as `insert` resolves them: the subquery makes the
    // person once, and the credential row reuses it.
    let (credential_ref, person_id, operator): (Option<i64>, Option<i64>, bool) = sqlx::query_as(
        "SELECT audit_credential($1, p.actor, $2), p.actor, \
                    $3 OR COALESCE((SELECT is_admin FROM users WHERE id = $4), false) \
             FROM (SELECT audit_person($4) AS actor) p",
    )
    .bind(&actor.credential)
    .bind(&actor.label)
    .bind(actor.operator)
    .bind(&actor.user_id)
    .fetch_one(&mut *conn)
    .await?;
    Ok(ResolvedActor {
        source: actor.source,
        credential_ref,
        person_id,
        label: actor.stored_label(),
        operator,
        ip: actor.ip,
        teams: catalog_teams(conn, catalog_id).await?,
        target_person: None,
    })
}

/// A user's `audit_people` id and operator marking, for an entry that is
/// also about them ([`ResolvedActor::about`]).
///
/// # Arguments
/// * `conn` - the change's account transaction, holding the user's row.
/// * `user_id` - the user's `users.id`.
///
/// # Returns
/// The person and whether they are an admin now; `None` for an unknown user.
///
/// # Errors
/// [`OkfError::Database`].
#[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "audit.resolve_person"))]
pub async fn resolve_person(
    conn: &mut PgConnection,
    user_id: &str,
) -> Result<Option<FixedPerson>, OkfError> {
    let row: Option<(Option<i64>, bool)> =
        sqlx::query_as("SELECT audit_person(id), is_admin FROM users WHERE id = $1")
            .bind(user_id)
            .fetch_optional(&mut *conn)
            .await?;
    Ok(row.and_then(|(person, operator)| {
        person.map(|person_id| FixedPerson {
            person_id,
            operator,
        })
    }))
}

/// What a maintenance run's entry names about the run's credential: the
/// ids [`resolve_run`] reads. A run's own key may be gone (a deleted user's
/// keys), and then the entry names no credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunCredential {
    /// The run's `credential_id` (a key or grant id, kept in the run).
    pub credential_id: String,
    /// The run's `key_id`: the credential's row while it exists.
    pub key_id: Option<String>,
}

/// Resolves a maintenance run's actor on the account database, for
/// [`record_catalog`]: the system, running as the run's credential. It
/// locks no user and waits for no audit row: the credential's `audit_credentials`
/// row is only read (a row [`forget_person`] has cleared simply names no
/// credential), and an admin-level key, or one of an admin user, is marked
/// as operator staff, as [`record`] marks it (issue #402). The entry names
/// no label (M14 Decision 17: `system:maintenance`).
///
/// # Arguments
/// * `conn` - the change's account transaction.
/// * `run` - the run's credential.
/// * `owner_team_id` - the catalog's owner team (`teams.id`), if any.
///
/// # Returns
/// The resolved actor.
///
/// # Errors
/// [`OkfError::Database`].
#[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "audit.resolve_run"))]
pub async fn resolve_run(
    conn: &mut PgConnection,
    run: &RunCredential,
    owner_team_id: Option<&str>,
) -> Result<ResolvedActor, OkfError> {
    let (credential_ref, operator, team): (Option<i64>, bool, Option<i64>) = sqlx::query_as(
        "SELECT (SELECT a.id FROM audit_credentials a WHERE a.credential_id = $1), \
                EXISTS (SELECT 1 FROM api_keys k LEFT JOIN users u ON u.id = k.user_id \
                         WHERE k.id = $2 AND (k.access = 'admin' OR COALESCE(u.is_admin, false))), \
                (SELECT t.id FROM audit_teams t WHERE t.team_id = $3)",
    )
    .bind(&run.credential_id)
    .bind(&run.key_id)
    .bind(owner_team_id)
    .fetch_one(&mut *conn)
    .await?;
    Ok(ResolvedActor {
        source: Source::System,
        credential_ref,
        person_id: None,
        label: AuditActor::system("maintenance").stored_label(),
        operator,
        ip: None,
        teams: team.into_iter().collect(),
        target_person: None,
    })
}

/// Writes one catalog entry inside the catalog's store transaction, from ids
/// resolved on the account database beforehand ([`resolve_actor`],
/// [`resolve_run`]). It reads no account table, so the store connection
/// never waits for an account row (the acquisition rule of
/// `CatalogDb::begin`).
///
/// # Arguments
/// * `conn` - the change's store transaction (`&mut tx`).
/// * `actor` - the resolved actor and teams.
/// * `entry` - what.
/// * `now` - when (server clock).
///
/// # Returns
/// The entry's id on the store.
///
/// # Errors
/// [`OkfError::Refused`] for an action that is not a catalog event, before
/// any SQL; [`OkfError::Database`], which fails the caller's change with it.
#[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "audit.record_catalog"))]
pub async fn record_catalog(
    conn: &mut PgConnection,
    actor: &ResolvedActor,
    entry: &CatalogEntry<'_>,
    now: DateTime<Utc>,
) -> Result<i64, OkfError> {
    if !entry.action.is_catalog_event() {
        return Err(OkfError::Refused {
            reason: format!(
                "{} is recorded on the account database, not on a store",
                entry.action.as_str()
            ),
        });
    }
    Ok(sqlx::query_scalar(
        "INSERT INTO catalog_audit_entries (at, catalog_id, action, source, \
             actor_credential_ref, actor_person_id, actor_label, actor_operator, client_ip, \
             target_name, target_person_id, target_operator, teams, details) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9::inet, $10, $11, $12, $13, $14) \
         RETURNING id",
    )
    .bind(now)
    .bind(entry.catalog_id)
    .bind(entry.action.as_str())
    .bind(actor.source.as_str())
    .bind(actor.credential_ref)
    .bind(actor.person_id)
    .bind(&actor.label)
    .bind(actor.operator)
    .bind(actor.ip.map(|ip| ip.to_string()))
    .bind(entry.catalog)
    .bind(actor.target_person.map(|person| person.person_id))
    .bind(actor.target_person.is_some_and(|person| person.operator))
    .bind(&actor.teams)
    .bind(&entry.details)
    .fetch_one(&mut *conn)
    .await?)
}

/// How a change locks its target user's row ([`People::target`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowLock {
    /// `FOR SHARE`: a guarded insert excludes non-key user changes too.
    Share,
    /// `FOR UPDATE`: the change writes the row's key-adjacent columns or
    /// must exclude `users::set_admin` (an email, the admin flag).
    Update,
    /// `FOR NO KEY UPDATE`: the change ends or replaces the user's child
    /// rows (sessions, emailed links).
    NoKeyUpdate,
    /// `FOR KEY SHARE`: the change only needs the user to stay (an
    /// entitlement), as `users::id_of` holds it.
    KeyShare,
}

/// The users a change locks besides its actor, by where they come from:
/// [`lock_people`]'s input. A name in `users`, `teams` or `catalogs` that
/// is unknown or malformed locks nothing (the change's own lookups refuse
/// it); an unknown or malformed `target` is refused here.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct People<'a> {
    /// The user the change is about, by name, and how its row is locked;
    /// [`lock_people`] answers its id and admin flag.
    pub target: Option<(&'a str, RowLock)>,
    /// Other users by name (a member, an owner), held `FOR KEY SHARE`.
    pub users: &'a [&'a str],
    /// Teams by name, for the user whose personal team each is.
    pub teams: &'a [&'a str],
    /// Catalogs by name, for the user whose personal team owns each.
    pub catalogs: &'a [&'a str],
}

/// Locks, before anything else in a change's transaction, every user row
/// the change and its entry will lock: the actor and its credential's
/// owner ([`record`] resolves both), the target (in its [`RowLock`] mode),
/// the users it names, and the person of each personal team it names
/// directly or as a catalog's owner ([`team_ref`] and [`catalog_teams`]
/// resolve them). Every other user is held `FOR KEY SHARE`.
///
/// The one order every writer of user rows takes (`tools/agentic/lessons.md`):
/// users first, then teams, then catalogs and changesets, then the
/// `server_state` generation row; and among users the order of
/// `users::set_admin` and `users::delete`, every admin by id, then the
/// others by id. The rows are read without a lock to sort them, then locked
/// one by one; the target's admin flag is read again under its lock.
///
/// # Arguments
/// * `conn` - the change's transaction (`&mut tx`).
/// * `actor` - who.
/// * `people` - the target and the other users, teams and catalogs.
///
/// # Returns
/// The target's `users.id` and admin flag under its lock, or `None`
/// without a target.
///
/// # Errors
/// [`OkfError::UnknownName`] for a malformed or unknown target;
/// [`OkfError::Database`].
#[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "audit.lock_people"))]
pub async fn lock_people(
    conn: &mut PgConnection,
    actor: &AuditActor,
    people: &People<'_>,
) -> Result<Option<(String, bool)>, OkfError> {
    use crate::store::catalogs::is_valid_name;
    use crate::store::users::unknown;
    let target = people.target.map(|(name, _)| name);
    // A malformed name (a NUL, which Postgres refuses) names nobody: the
    // target is refused, and the others are left out, so the change's own
    // lookup answers "unknown", never a 500.
    if let Some(name) = target.filter(|name| !is_valid_name(name)) {
        return Err(unknown(name));
    }
    let valid = |names: &[&str]| -> Vec<String> {
        names
            .iter()
            .filter(|name| is_valid_name(name))
            .map(|name| name.to_string())
            .collect()
    };
    // Each row: id, admin flag, whether it is the target. `COALESCE`:
    // `name = NULL` is NULL without a target.
    let rows: Vec<(String, bool, bool)> = sqlx::query_as(
        "SELECT u.id, u.is_admin, COALESCE(u.name = $1, false) FROM users u \
         WHERE u.name = $1 OR u.id = $2 OR u.name = ANY($3) \
            OR u.id = (SELECT user_id FROM api_keys WHERE id = $4) \
            OR u.id = (SELECT user_id FROM sign_in_codes WHERE id = $4) \
            OR u.id IN (SELECT personal_of FROM teams WHERE name = ANY($5)) \
            OR u.id IN (SELECT t.personal_of FROM catalogs c \
                        JOIN teams t ON t.id = c.owner_team_id WHERE c.name = ANY($6)) \
         ORDER BY u.is_admin DESC, u.id",
    )
    .bind(target)
    .bind(&actor.user_id)
    .bind(valid(people.users))
    .bind(&actor.credential)
    .bind(valid(people.teams))
    .bind(valid(people.catalogs))
    .fetch_all(&mut *conn)
    .await?;
    let mut locked = None;
    for (id, _, is_target) in &rows {
        let sql = match people.target {
            Some((_, RowLock::Share)) if *is_target => {
                "SELECT is_admin FROM users WHERE id = $1 FOR SHARE"
            }
            Some((_, RowLock::Update)) if *is_target => {
                "SELECT is_admin FROM users WHERE id = $1 FOR UPDATE"
            }
            Some((_, RowLock::NoKeyUpdate)) if *is_target => {
                "SELECT is_admin FROM users WHERE id = $1 FOR NO KEY UPDATE"
            }
            _ => "SELECT is_admin FROM users WHERE id = $1 FOR KEY SHARE",
        };
        let is_admin: Option<bool> = sqlx::query_scalar(sql)
            .bind(id)
            .fetch_optional(&mut *conn)
            .await?;
        // A row deleted meanwhile is skipped; a deleted target is unknown.
        if let (true, Some(is_admin)) = (*is_target, is_admin) {
            locked = Some((id.clone(), is_admin));
        }
    }
    match (target, locked) {
        (Some(name), None) => Err(unknown(name)),
        (_, locked) => Ok(locked),
    }
}

/// Locks a credential's owner and granted catalogs' people before its row.
/// Reads the owner and grants without locking first, then rechecks them under
/// the credential lock. A concurrent assignment or grant change is refused
/// for retry instead of taking a newly discovered user out of order.
///
/// # Arguments
/// * `conn` - the change's transaction.
/// * `actor` - who makes the change.
/// * `id` - the immutable credential id.
/// * `code` - whether this is a sign-in code rather than a key or grant.
/// * `people` - additional users and catalogs (including an assignment's new owner).
///
/// # Returns
/// Nothing; the locks last until the transaction ends. An absent credential,
/// including one deleted with its owner while this waited, is left for the
/// caller's existing unknown-target handling.
///
/// # Errors
/// [`OkfError::Refused`] if ownership or grants changed while locking;
/// [`OkfError::Database`] or [`OkfError::UnknownName`] from locking people.
#[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "audit.lock_credential_people"))]
pub async fn lock_credential_people(
    conn: &mut PgConnection,
    actor: &AuditActor,
    id: &str,
    code: bool,
    people: &People<'_>,
) -> Result<(), OkfError> {
    let snapshot_sql = if code {
        "SELECT s.user_id, u.name FROM sign_in_codes s LEFT JOIN users u ON u.id = s.user_id WHERE s.id = $1"
    } else {
        "SELECT s.user_id, u.name FROM api_keys s LEFT JOIN users u ON u.id = s.user_id WHERE s.id = $1"
    };
    let before: Option<(Option<String>, Option<String>)> = sqlx::query_as(snapshot_sql)
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?;
    let catalog_sql = if code {
        "SELECT c.name FROM sign_in_code_catalogs g JOIN catalogs c ON c.id = g.catalog_id WHERE g.code_id = $1 ORDER BY c.name"
    } else {
        "SELECT c.name FROM key_catalogs g JOIN catalogs c ON c.id = g.catalog_id WHERE g.key_id = $1 ORDER BY c.name"
    };
    let catalogs: Vec<String> = sqlx::query_scalar(catalog_sql)
        .bind(id)
        .fetch_all(&mut *conn)
        .await?;
    let mut users = people.users.to_vec();
    if let Some((_, Some(name))) = &before {
        users.push(name);
    }
    let mut names = people.catalogs.to_vec();
    names.extend(catalogs.iter().map(String::as_str));
    lock_people(
        conn,
        actor,
        &People {
            users: &users,
            catalogs: &names,
            ..*people
        },
    )
    .await?;
    let lock_sql = if code {
        "SELECT user_id FROM sign_in_codes WHERE id = $1 FOR UPDATE"
    } else {
        "SELECT user_id FROM api_keys WHERE id = $1 FOR UPDATE"
    };
    let after: Option<Option<String>> = sqlx::query_scalar(lock_sql)
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?;
    let current_catalogs: Vec<String> = sqlx::query_scalar(catalog_sql)
        .bind(id)
        .fetch_all(&mut *conn)
        .await?;
    // Gone under the lock (its owner was deleted meanwhile, which cascades):
    // not a change to retry, so the caller's own lookup answers "unknown".
    // `let ... else`: bind the row when there is one, else return early.
    let Some(after) = after else {
        return Ok(());
    };
    if before.map(|(owner, _)| owner) != Some(after) || catalogs != current_catalogs {
        return Err(OkfError::Refused {
            reason: "credential changed while locking; retry the request".to_string(),
        });
    }
    Ok(())
}

/// How entries name a team: its `audit_teams` id, which follows the team's
/// immutable `teams.id`, so a later team with the same name is another id.
///
/// # Arguments
/// * `conn` - a connection or transaction.
/// * `team` - the team's current name.
///
/// # Returns
/// Its `audit_teams` id.
///
/// # Errors
/// [`OkfError::UnknownName`] (a malformed name included); [`OkfError::Database`].
#[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "audit.team_ref"))]
pub async fn team_ref(conn: &mut PgConnection, team: &str) -> Result<i64, OkfError> {
    let unknown = || OkfError::UnknownName {
        noun: "team",
        name: team.to_string(),
    };
    // As `teams::find`, `lock_team` and `id_of` do (#339): a malformed name
    // (a NUL, which Postgres refuses) names no team, so it is never a
    // database error.
    if !crate::store::catalogs::is_valid_name(team) {
        return Err(unknown());
    }
    sqlx::query_scalar("SELECT audit_team_ref(id) FROM teams WHERE name = $1")
        .bind(team)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(unknown)
}

/// Creates a new key's or grant's `audit_credentials` row in the
/// transaction that inserts it (M18 spec Decision 20), labelled with its
/// `actor` as [`record`] labels it when it acts, so caches read the audit
/// id with the credential and never have to write one. Call it after the
/// insert and after [`lock_people`] has locked the owner: the SQL resolver
/// takes the owner `FOR KEY SHARE` again (a no-op under the stronger lock
/// already held) and inserts a row no other transaction can see yet, so it
/// waits on nothing (`migrations/accounts/0014_audit_row_locks.sql`).
///
/// # Arguments
/// * `conn` - the creating transaction.
/// * `credential_id` - the new `api_keys.id`.
/// * `label` - its `actor`.
///
/// # Returns
/// Nothing; the row lasts with the transaction.
///
/// # Errors
/// [`OkfError::Database`].
#[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "audit.credential_created"))]
pub async fn credential_created(
    conn: &mut PgConnection,
    credential_id: &str,
    label: &str,
) -> Result<(), OkfError> {
    sqlx::query("SELECT audit_credential($1, NULL, $2)")
        .bind(credential_id)
        .bind(label)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// The `audit_teams` id of a catalog's owner team: what a catalog event
/// belongs to.
///
/// # Arguments
/// * `conn` - a connection or transaction.
/// * `catalog_id` - the catalog.
///
/// # Returns
/// One reference, or none for a catalog with no owner yet (or no catalog).
///
/// # Errors
/// [`OkfError::Database`].
#[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "audit.catalog_teams", catalog_id = catalog_id))]
pub async fn catalog_teams(conn: &mut PgConnection, catalog_id: i64) -> Result<Vec<i64>, OkfError> {
    let team: Option<i64> = sqlx::query_scalar(
        "SELECT audit_team_ref(owner_team_id) FROM catalogs \
         WHERE id = $1 AND owner_team_id IS NOT NULL",
    )
    .bind(catalog_id)
    .fetch_optional(&mut *conn)
    .await?;
    // `into_iter().collect()` turns `Option<T>` into a zero- or one-item Vec.
    Ok(team.into_iter().collect())
}

/// The `audit_teams` ids of the catalogs a key or grant is granted: what a
/// credential event belongs to. An all-catalogs credential belongs to no
/// team (operator only; plan Decision 6).
///
/// # Arguments
/// * `conn` - a connection or transaction.
/// * `key_id` - the key or grant.
///
/// # Returns
/// The owner teams of its granted catalogs, once each, in id order.
///
/// # Errors
/// [`OkfError::Database`].
#[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "audit.key_teams"))]
pub async fn key_teams(conn: &mut PgConnection, key_id: &str) -> Result<Vec<i64>, OkfError> {
    Ok(sqlx::query_scalar(
        "SELECT audit_team_ref(t.owner_team_id) FROM \
           (SELECT DISTINCT c.owner_team_id FROM key_catalogs g \
              JOIN catalogs c ON c.id = g.catalog_id \
             WHERE g.key_id = $1 AND c.owner_team_id IS NOT NULL) t \
         ORDER BY 1",
    )
    .bind(key_id)
    .fetch_all(&mut *conn)
    .await?)
}

/// The audit teams of the catalogs granted by a sign-in code.
///
/// # Arguments
/// * `conn` - the change's transaction.
/// * `code_id` - the sign-in code's immutable id.
///
/// # Returns
/// The distinct audit team references, sorted by id; none for an all-catalogs code.
///
/// # Errors
/// [`OkfError::Database`].
#[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "audit.code_teams"))]
pub async fn code_teams(conn: &mut PgConnection, code_id: &str) -> Result<Vec<i64>, OkfError> {
    Ok(sqlx::query_scalar(
        "SELECT audit_team_ref(t.owner_team_id) FROM \
         (SELECT DISTINCT c.owner_team_id FROM sign_in_code_catalogs g \
          JOIN catalogs c ON c.id = g.catalog_id \
          WHERE g.code_id = $1 AND c.owner_team_id IS NOT NULL) t ORDER BY 1",
    )
    .bind(code_id)
    .fetch_all(&mut *conn)
    .await?)
}

/// Stops the log identifying a user that is being deleted, in the
/// deletion's transaction: the person row becomes `deleted user #<id>`, and
/// the user's credentials lose their id (a legacy key's id was its name)
/// and their label (a self-issued key's defaults to a name the user chose),
/// showing as `credential #<id>`. Entries themselves hold no name to clear.
/// Call it before deleting the user row (its credentials are still there).
///
/// # Arguments
/// * `conn` - the deletion's transaction.
/// * `user_id` - the user's `users.id`.
///
/// # Errors
/// [`OkfError::Database`].
#[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "audit.forget_person"))]
pub async fn forget_person(conn: &mut PgConnection, user_id: &str) -> Result<(), OkfError> {
    // Credentials first, found two ways: linked to the person, or (a row
    // whose person was never resolved) still named by the user's keys,
    // grants and codes. So it must run before the user row is deleted,
    // which cascades those.
    sqlx::query(
        "UPDATE audit_credentials SET credential_id = NULL, label = NULL \
         WHERE person_id = (SELECT id FROM audit_people WHERE user_id = $1) \
            OR credential_id IN (SELECT id FROM api_keys WHERE user_id = $1 \
                                 UNION ALL SELECT id FROM sign_in_codes WHERE user_id = $1)",
    )
    .bind(user_id)
    .execute(&mut *conn)
    .await?;
    sqlx::query(
        "UPDATE audit_people SET user_id = NULL, name = 'deleted user #' || id, \
                display_name = '' WHERE user_id = $1",
    )
    .bind(user_id)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Deletes entries older than `before`: the daily retention prune, the one
/// delete of the log. The append-only trigger refuses the whole statement
/// if any of those entries is still inside the retention period by the
/// database's clock, so the caller leaves a margin.
///
/// # Arguments
/// * `pool` - the database.
/// * `before` - the cut-off: now, minus the retention, minus a margin.
///
/// # Returns
/// How many entries went (their `audit_access_counts` rows cascade).
///
/// # Errors
/// [`OkfError::Database`], which deletes nothing.
#[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "audit.prune"))]
pub async fn prune(pool: &PgPool, before: DateTime<Utc>) -> Result<u64, OkfError> {
    Ok(sqlx::query("DELETE FROM audit_entries WHERE at < $1")
        .bind(before)
        .execute(pool)
        .await?
        .rows_affected())
}

/// Deletes a store's catalog events older than `before`: the retention
/// prune of `catalog_audit_entries`, the one delete of that log. The
/// store's append-only trigger refuses the whole statement if any of those
/// entries is still inside the retention period by the store's clock, so
/// the caller leaves a margin, as for [`prune`]. A store has no settings
/// table: `retention_days` is passed to the trigger in a transaction-local
/// setting (`okf.audit_retention_days`).
///
/// # Arguments
/// * `store` - the store to prune.
/// * `before` - the cut-off: now, minus the retention, minus a margin.
/// * `retention_days` - the `audit_retention_days` setting.
///
/// # Returns
/// How many entries went.
///
/// # Errors
/// [`OkfError::Database`] or [`OkfError::StoreUnavailable`], which delete
/// nothing.
#[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "audit.prune_catalog"))]
pub async fn prune_catalog(
    store: &CatalogDb,
    before: DateTime<Utc>,
    retention_days: i64,
) -> Result<u64, OkfError> {
    let mut tx = store
        .executor()
        .begin()
        .await
        .map_err(|error| store.classify(error))?;
    // `set_config(.., true)` is `SET LOCAL` with a bindable value.
    sqlx::query("SELECT set_config('okf.audit_retention_days', $1, true)")
        .bind(retention_days.to_string())
        .execute(&mut *tx)
        .await?;
    let removed = sqlx::query("DELETE FROM catalog_audit_entries WHERE at < $1")
        .bind(before)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    tx.commit().await.map_err(|error| store.classify(error))?;
    Ok(removed)
}

/// How long past the end of its hour a provisioner budget counter is kept,
/// so a replica whose clock lags a little still finds its hour's counter.
const BUDGET_COUNTER_GRACE_HOURS: i64 = 1;

/// Deletes the hourly budget counters of provisioner entries (rows of
/// `audit_access_counts` with no entry to cascade from) once their hour
/// ended over an hour (a grace for a lagging replica clock) before `now`. A counter only
/// gates the entries opened within its own hour and records nothing itself
/// (the entries carry the counts), so it is not kept for the retention
/// period.
///
/// # Arguments
/// * `pool` - the database.
/// * `now` - the server clock.
///
/// # Returns
/// How many counters went.
///
/// # Errors
/// [`OkfError::Database`], which deletes nothing.
#[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "audit.prune_budget_counters"))]
pub async fn prune_budget_counters(pool: &PgPool, now: DateTime<Utc>) -> Result<u64, OkfError> {
    // A counter's third part is the start of its hour, in Unix seconds; the
    // hour ended an hour after that. The `CASE` keeps the cast to counter
    // rows: Postgres may evaluate `AND` terms in any order.
    let before = now - TimeDelta::hours(1 + BUDGET_COUNTER_GRACE_HOURS);
    Ok(sqlx::query(
        "DELETE FROM audit_access_counts WHERE entry_id IS NULL \
           AND CASE WHEN split_part(bucket, chr(31), 1) = 'provcap' \
                    THEN split_part(bucket, chr(31), 3)::bigint < $1 END",
    )
    .bind(before.timestamp())
    .execute(pool)
    .await?
    .rows_affected())
}

/// What a recorded access did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccessKind {
    /// Read contents.
    Read,
    /// Wrote to a changeset.
    Write,
    /// Exported.
    Export,
}

/// Who made a request, with every identity fixed at admission (audit ids
/// from the key cache), so a background write needs no lookup by name and
/// creates no person or credential row (plan Decisions 21 and 22).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmittedActor {
    /// Where the request came from.
    pub source: Source,
    /// The actor's `audit_credentials` id (a key or grant).
    pub credential: Option<i64>,
    /// The actor's `audit_people` id.
    pub person: Option<i64>,
    /// [`AuditActor::stored_label`]: never a person's name.
    pub label: String,
    /// An admin credential or a provisioner.
    pub operator: bool,
    /// The client address.
    pub ip: Option<IpAddr>,
    /// When it was admitted (server clock).
    pub at: DateTime<Utc>,
}

impl AdmittedActor {
    /// The start of the clock hour the request was admitted in: the window
    /// every counted entry collapses repeats within.
    ///
    /// # Returns
    /// The hour's start. A time chrono cannot round (a clock never gives
    /// one) stands for itself.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "trivial accessor or conversion: maps a value, no I/O"
        )
    )]
    fn hour(&self) -> DateTime<Utc> {
        self.at
            .duration_trunc(TimeDelta::hours(1))
            .unwrap_or(self.at)
    }

    /// The actor's part of a bucket name: audit ids only, so a deletion
    /// leaves no name in the bucket key either.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "trivial accessor or conversion: maps a value, no I/O"
        )
    )]
    fn bucket_name(&self) -> String {
        match (self.credential, self.person) {
            (Some(credential), _) => format!("c{credential}"),
            (None, Some(person)) => format!("p{person}"),
            (None, None) => format!("l{}", self.label),
        }
    }
}

/// One operator access, as admission saw it (plan Decisions 21 and 22).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessRecord {
    /// Who, with every identity fixed at admission.
    pub actor: AdmittedActor,
    /// The catalog's name at admission (a display snapshot).
    pub catalog: String,
    /// The catalog's immutable id and owner team at admission.
    pub scope: CatalogScope,
}

/// Counts one admin read in its `catalog.admin_read` entry: one entry per
/// actor, catalog identity (`catalogs.id`), owning team and clock hour, all
/// as admission recorded them (plan Decision 20), created on the bucket's
/// first read. Exact under concurrency: the counter row is upserted, so a
/// second first read waits for the first to commit and then increments.
/// It only references audit rows that already exist: a write that runs
/// after the actor's user was deleted (and forgotten) adds nothing that
/// identifies them, and a catalog moved or deleted meanwhile keeps the read
/// in the scope it was admitted in.
///
/// # Arguments
/// * `pool` - the database.
/// * `record` - the access, as admitted.
///
/// # Errors
/// [`OkfError::Database`].
#[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "audit.record_admin_read"))]
pub async fn record_admin_read(pool: &PgPool, record: &AccessRecord) -> Result<(), OkfError> {
    let actor = &record.actor;
    let hour = actor.hour();
    // `0` stands for "no owner team" (audit ids start at 1). U+001F (unit
    // separator) cannot occur in the parts.
    let bucket = format!(
        "{}\u{1f}{}\u{1f}{}\u{1f}{}",
        actor.bucket_name(),
        record.scope.id,
        record.scope.team.unwrap_or(0),
        hour.timestamp()
    );
    let mut tx = pool.begin().await?;
    let (entry_id, _) = count_bucket(&mut tx, &bucket).await?;
    if entry_id.is_none() {
        // The ids as admission fixed them; no `audit_person` or
        // `audit_credential` call, so nothing is created for a deleted user.
        // `into_iter().collect()` turns the `Option` into a zero- or
        // one-item Vec.
        let teams: Vec<i64> = record.scope.team.into_iter().collect();
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO audit_entries (at, action, source, actor_credential_ref, \
                 actor_person_id, actor_label, actor_operator, client_ip, target_kind, \
                 target_name, teams, details) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8::inet, 'catalog', $9, $10, $11) \
             RETURNING id",
        )
        .bind(actor.at)
        .bind(Action::CatalogAdminRead.as_str())
        .bind(actor.source.as_str())
        .bind(actor.credential)
        .bind(actor.person)
        .bind(&actor.label)
        .bind(actor.operator)
        .bind(actor.ip.map(|ip| ip.to_string()))
        .bind(&record.catalog)
        .bind(&teams)
        .bind(serde_json::json!({
            "hour": crate::clock::format_timestamp(hour),
            "catalog_id": record.scope.id,
        }))
        .fetch_one(&mut *tx)
        .await?;
        link_bucket(&mut tx, &bucket, id).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Counts one request in a bucket, creating the bucket's row on its first.
/// Exact under concurrency: the row is upserted, so a second first request
/// waits for the first to commit and then increments.
///
/// # Arguments
/// * `conn` - the write's transaction.
/// * `bucket` - the bucket's name (audit ids only).
///
/// # Returns
/// The bucket's entry (`None` while it has none: this request writes it)
/// and its request count including this one.
///
/// # Errors
/// [`OkfError::Database`].
#[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "audit.count_bucket"))]
async fn count_bucket(
    conn: &mut PgConnection,
    bucket: &str,
) -> Result<(Option<i64>, i64), OkfError> {
    Ok(sqlx::query_as(
        "INSERT INTO audit_access_counts (bucket, requests) VALUES ($1, 1) \
         ON CONFLICT (bucket) DO UPDATE SET requests = audit_access_counts.requests + 1 \
         RETURNING entry_id, requests",
    )
    .bind(bucket)
    .fetch_one(conn)
    .await?)
}

/// Whether a bucket has a row.
///
/// # Errors
/// [`OkfError::Database`].
#[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "audit.bucket_exists"))]
async fn bucket_exists(conn: &mut PgConnection, bucket: &str) -> Result<bool, OkfError> {
    Ok(
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM audit_access_counts WHERE bucket = $1)")
            .bind(bucket)
            .fetch_one(conn)
            .await?,
    )
}

/// Ties a bucket to the entry that was written for it.
///
/// # Errors
/// [`OkfError::Database`].
#[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "audit.link_bucket"))]
async fn link_bucket(conn: &mut PgConnection, bucket: &str, entry: i64) -> Result<(), OkfError> {
    sqlx::query("UPDATE audit_access_counts SET entry_id = $2 WHERE bucket = $1")
        .bind(bucket)
        .bind(entry)
        .execute(conn)
        .await?;
    Ok(())
}

/// How many distinct entries one provisioner credential may open in a clock
/// hour. Repeats collapse into their entry's count, so this bounds what a
/// scripted flood of different routes, methods and users can add; every
/// further kind that hour counts in one overflow entry per action.
pub const PROVISIONER_ENTRIES_PER_HOUR: i64 = 50;

/// Why a provisioner request was refused: a stable code, never text that
/// could echo what the caller sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusalReason {
    /// The route is not in the provisioner allow-list (the gate refused it).
    RouteNotAllowed,
    /// The route is allowed but its handler or a store check refused the
    /// request (a listing without `email`, an admin user as the target).
    Forbidden,
}

impl RefusalReason {
    /// The stored code.
    ///
    /// # Returns
    /// `"route_not_allowed"` or `"forbidden"`.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "trivial accessor or conversion: maps a value, no I/O"
        )
    )]
    pub fn as_str(self) -> &'static str {
        match self {
            RefusalReason::RouteNotAllowed => "route_not_allowed",
            RefusalReason::Forbidden => "forbidden",
        }
    }
}

/// A request method as an entry stores it: one of the standard methods, or
/// `OTHER`, so a client cannot make up the text (or the number of buckets).
///
/// # Arguments
/// * `method` - the request's method.
///
/// # Returns
/// Its standard spelling.
#[cfg_attr(
    dylint_lib = "hyp_tracing",
    allow(
        uninstrumented_fn,
        reason = "trivial accessor or conversion: maps a value, no I/O"
    )
)]
pub fn standard_method(method: &str) -> &'static str {
    [
        "GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS", "TRACE", "CONNECT",
    ]
    .into_iter()
    .find(|known| *known == method)
    .unwrap_or("OTHER")
}

/// What a provisioner did that is worth an entry (the other reads and
/// no-ops stay in request traces and metrics).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProvisionerEvent {
    /// `GET /api/v1/users?email=`: stores the outcome and the matched user,
    /// never the address.
    Lookup {
        /// The matched `users.id`; `None` for no user, and for an admin
        /// user the provisioner is not shown.
        matched: Option<String>,
    },
    /// A request refused after admission.
    Refused {
        /// A standard method ([`standard_method`]).
        method: &'static str,
        /// The matched route template (`MatchedPath`), never the literal
        /// path.
        route: String,
        /// Why.
        reason: RefusalReason,
    },
}

/// One provisioner request that earns an entry, as admission saw it: the
/// actor is fixed here, and the outcome when the handler decided it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvisionerUse {
    /// The provisioner key, with every identity fixed at admission.
    pub actor: AdmittedActor,
    /// What it did.
    pub event: ProvisionerEvent,
}

/// How a [`ProvisionerEvent`] is stored.
struct EntryShape {
    action: Action,
    /// Names the bucket within an actor and hour (audit ids and codes only).
    key: String,
    /// The entry's route target, used when it names no matched user.
    target_name: String,
    matched: Option<String>,
    details: serde_json::Value,
}

impl ProvisionerEvent {
    /// How the event is stored.
    #[tracing::instrument(level = "trace", skip_all)]
    fn shape(&self) -> EntryShape {
        match self {
            ProvisionerEvent::Lookup { matched } => {
                let outcome = if matched.is_some() {
                    "found"
                } else {
                    "not_found"
                };
                EntryShape {
                    action: Action::UserLookedUp,
                    key: format!("{outcome}\u{1f}{}", matched.as_deref().unwrap_or("")),
                    target_name: "GET /api/v1/users".to_string(),
                    matched: matched.clone(),
                    details: serde_json::json!({ "outcome": outcome }),
                }
            }
            ProvisionerEvent::Refused {
                method,
                route,
                reason,
            } => EntryShape {
                action: Action::ProvisionerRefused,
                key: format!(
                    "refused\u{1f}{method}\u{1f}{route}\u{1f}{}",
                    reason.as_str()
                ),
                target_name: format!("{method} {route}"),
                matched: None,
                details: serde_json::json!({
                    "method": method,
                    "route": route,
                    "reason": reason.as_str(),
                }),
            },
        }
    }
}

/// Counts one provisioner request in its entry: one entry per key, kind of
/// event (lookup outcome and matched user, or method, route and reason) and
/// clock hour, with an exact request count in `audit_access_counts`, as
/// [`record_admin_read`] does. Bounded two ways: repeats collapse, and a key
/// opens at most [`PROVISIONER_ENTRIES_PER_HOUR`] entries an hour (the rest
/// count in one overflow entry per action, target `*`). The entry belongs to
/// no team, so owners never see it, and stores no request text: not the
/// email, the literal path or the body.
///
/// # Arguments
/// * `pool` - the database.
/// * `usage` - the request, as admitted.
///
/// # Errors
/// [`OkfError::Database`], which loses the count (the caller logs it).
#[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "audit.record_provisioner_use"))]
pub async fn record_provisioner_use(pool: &PgPool, usage: &ProvisionerUse) -> Result<(), OkfError> {
    let actor = &usage.actor;
    let hour = actor.hour();
    let stamp = hour.timestamp();
    let who = actor.bucket_name();
    let mut shape = usage.event.shape();
    let mut tx = pool.begin().await?;
    let mut bucket = format!("prov\u{1f}{who}\u{1f}{}\u{1f}{stamp}", shape.key);
    // A bucket that exists collapses for free. A new one counts against the
    // key's hourly budget (a row that is only a counter, which
    // `prune_budget_counters` clears once its hour is past) and is spent on the overflow entry once
    // the budget is gone. The counter row's lock serialises the key's
    // allocations, so the bucket is looked for again once it is held: a
    // concurrent request for the same kind may have created it, and then
    // this one collapses into it and gives its slot back.
    if !bucket_exists(&mut tx, &bucket).await? {
        let cap = format!("provcap\u{1f}{who}\u{1f}{stamp}");
        let (_, used) = count_bucket(&mut tx, &cap).await?;
        if bucket_exists(&mut tx, &bucket).await? {
            sqlx::query(
                "UPDATE audit_access_counts SET requests = GREATEST(requests - 1, 1) \
                 WHERE bucket = $1",
            )
            .bind(&cap)
            .execute(&mut *tx)
            .await?;
        } else if used > PROVISIONER_ENTRIES_PER_HOUR {
            bucket = format!(
                "provov\u{1f}{who}\u{1f}{}\u{1f}{stamp}",
                shape.action.as_str()
            );
            shape.target_name = "*".to_string();
            shape.matched = None;
            shape.details = serde_json::json!({ "overflow": true });
        }
    }
    if count_bucket(&mut tx, &bucket).await?.0.is_none() {
        if let Some(object) = shape.details.as_object_mut() {
            object.insert(
                "hour".to_string(),
                serde_json::Value::String(crate::clock::format_timestamp(hour)),
            );
        }
        // A matched user is the entry's target person (an admin's view
        // shows who); without one, or if the user is gone, the entry names
        // the route. Reading `audit_people` takes no lock.
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO audit_entries (at, action, source, actor_credential_ref, \
                 actor_person_id, actor_label, actor_operator, client_ip, target_kind, \
                 target_name, target_person_id, details) \
             SELECT $1, $2, $3, $4, $5, $6, true, $7::inet, \
                    CASE WHEN p.id IS NULL THEN 'route' ELSE 'user' END, \
                    CASE WHEN p.id IS NULL THEN $8 END, p.id, $9 \
             FROM (SELECT $10::text AS user_id) m \
             LEFT JOIN audit_people p ON p.user_id = m.user_id \
             RETURNING id",
        )
        .bind(actor.at)
        .bind(shape.action.as_str())
        .bind(actor.source.as_str())
        .bind(actor.credential)
        .bind(actor.person)
        .bind(&actor.label)
        .bind(actor.ip.map(|ip| ip.to_string()))
        .bind(&shape.target_name)
        .bind(&shape.details)
        .bind(&shape.matched)
        .fetch_one(&mut *tx)
        .await?;
        link_bucket(&mut tx, &bucket, id).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Refuses a provisioner acting on an admin user (spec Part 4). Store
/// functions call it after reading the target's admin flag under a lock
/// that `users::set_admin` conflicts with, so the check cannot race.
///
/// # Arguments
/// * `actor` - the caller.
/// * `is_admin` - the target's flag, read under that lock.
/// * `user` - its name, for the message.
///
/// # Errors
/// [`OkfError::Forbidden`] for a provisioner and an admin target.
#[tracing::instrument(level = "debug", skip_all)]
pub fn refuse_admin_target(actor: &AuditActor, is_admin: bool, user: &str) -> Result<(), OkfError> {
    if actor.provisioner && is_admin {
        return Err(OkfError::Forbidden {
            reason: format!("a provisioner key cannot act on admin user {user:?}"),
        });
    }
    Ok(())
}

/// Who is reading the log, which decides what they see (spec "Who reads
/// it, and how").
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Viewer {
    /// An every-catalog admin or the CLI: everything, unredacted.
    Admin,
    /// An owner of one team, through a console session: that team's
    /// entries, operator staff redacted.
    Owner {
        /// The team's `teams.id` (for the member-IP rule).
        team_id: String,
        /// Its `audit_teams` id: the scope. It follows the immutable
        /// `teams.id`, so a later team with the same name has another and
        /// sees none of this one's entries.
        audit_team: i64,
    },
    /// A provisioner, by key id: only the entries it wrote.
    Credential(String),
}

/// A position in the merged audit view: the entry a page ends at, by the
/// order the view sorts by, `(at, store, id)` newest first. `store` is the
/// name of the store the entry is on, and the empty string for the account
/// database (so entries written before the split, which stay there, sort
/// first among equal times). Ids are per source: two sources can hold the
/// same id, so the id alone says nothing.
///
/// An admin's cursor is the readable [`AuditCursor::to_plain`]; a team
/// owner's is sealed (`http::api::audit::cursor`, issue #430), because an id
/// shows how much was written in between.
// The derived `Ord` compares the fields in declaration order, which is the
// view's order.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct AuditCursor {
    at_micros: i64,
    store: String,
    id: i64,
}

impl AuditCursor {
    /// The position of an entry.
    ///
    /// # Arguments
    /// * `at` - when the entry was written (database precision: microseconds).
    /// * `store` - the store it is on, or `""` for the account database.
    /// * `id` - its id in that source.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(uninstrumented_fn, reason = "one-line constructor")
    )]
    pub fn new(at: DateTime<Utc>, store: &str, id: i64) -> Self {
        Self {
            at_micros: at.timestamp_micros(),
            store: store.to_string(),
            id,
        }
    }

    /// The position from its parts, as [`AuditCursor::parts`] gives them.
    ///
    /// # Arguments
    /// * `at_micros` - microseconds since the Unix epoch.
    /// * `store` - the store's name, or `""`.
    /// * `id` - the entry's id in its source.
    ///
    /// # Errors
    /// [`OkfError::InvalidPath`] (400, naming `after`) for a time out of
    /// range, a store name that is not one, or a negative id.
    #[tracing::instrument(level = "trace", skip_all)]
    pub fn from_parts(at_micros: i64, store: &str, id: i64) -> Result<Self, OkfError> {
        let refused = || OkfError::InvalidPath {
            path: "after".to_string(),
            reason: "not a cursor this audit view issued; load the first page again".to_string(),
        };
        let store_ok = store.is_empty() || crate::stores::StoreName::parse(store).is_ok();
        if !store_ok || id < 0 || DateTime::from_timestamp_micros(at_micros).is_none() {
            return Err(refused());
        }
        Ok(Self {
            at_micros,
            store: store.to_string(),
            id,
        })
    }

    /// The parts: microseconds since the epoch, the store (`""` for the
    /// account database) and the id.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(uninstrumented_fn, reason = "one-line accessor")
    )]
    pub fn parts(&self) -> (i64, &str, i64) {
        (self.at_micros, &self.store, self.id)
    }

    /// The readable form an admin or the CLI passes back as `after`:
    /// `<microseconds>:<store>:<id>`. Callers treat it as opaque.
    ///
    /// # Returns
    /// The text.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(uninstrumented_fn, reason = "formats a value, no I/O")
    )]
    pub fn to_plain(&self) -> String {
        format!("{}:{}:{}", self.at_micros, self.store, self.id)
    }

    /// Reads [`AuditCursor::to_plain`]'s text.
    ///
    /// # Arguments
    /// * `text` - the `after` value as sent.
    ///
    /// # Errors
    /// [`OkfError::InvalidPath`] (400, naming `after`) for anything else, a
    /// bare entry id included: ids are per source, so one no longer says
    /// where a page ends.
    #[tracing::instrument(level = "trace", skip_all)]
    pub fn parse_plain(text: &str) -> Result<Self, OkfError> {
        let refused = || OkfError::InvalidPath {
            path: "after".to_string(),
            reason: format!(
                "{text:?} is not a cursor; pass a page's next_after (or last id column)"
            ),
        };
        let mut parts = text.splitn(3, ':');
        let (Some(at), Some(store), Some(id), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(refused());
        };
        let at = at.parse::<i64>().map_err(|_| refused())?;
        let id = id.parse::<i64>().map_err(|_| refused())?;
        Self::from_parts(at, store, id).map_err(|_| refused())
    }

    /// How a source sorts against this cursor, for the page after it.
    ///
    /// # Arguments
    /// * `source` - the source's store name, `""` for the account database.
    ///
    /// # Returns
    /// 1 when a source's entries at this cursor's time all come after it
    /// (the source's name sorts before the cursor's, so every tie is
    /// older), 0 when the id decides, and -1 when none do.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(uninstrumented_fn, reason = "compares two strings, no I/O")
    )]
    fn ties_for(&self, source: &str) -> i32 {
        match source.cmp(self.store.as_str()) {
            std::cmp::Ordering::Less => 1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => -1,
        }
    }
}

/// The most entries one page holds.
pub const MAX_PAGE: i64 = 500;

/// A page's size when the query asks for none.
pub const DEFAULT_PAGE: i64 = 100;

/// The filters of `GET /audit` and `audit list` (all optional).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuditQuery<'a> {
    /// An admin's or provisioner's team filter, by name: every team that
    /// had it (a personal team by its user's name). An owner's view is
    /// scoped by [`Viewer::Owner`] instead.
    pub team: Option<&'a str>,
    /// One action.
    pub action: Option<Action>,
    /// A person's name, a credential id or an actor label.
    pub actor: Option<&'a str>,
    /// A target name, a target team's or person's name, or a credential id.
    pub target: Option<&'a str>,
    /// From this time on.
    pub since: Option<DateTime<Utc>>,
    /// Before this time.
    pub until: Option<DateTime<Utc>>,
    /// Older than this position (the cursor). A team owner's request never
    /// sets it from the query string directly: the route opens the owner's
    /// sealed cursor into it (`http::api::audit::cursor`, issue #430).
    pub after: Option<AuditCursor>,
    /// At most this many ([`AuditQuery::page_size`]).
    pub limit: i64,
}

impl AuditQuery<'_> {
    /// How many entries a page of this query holds: `limit`, at most
    /// [`MAX_PAGE`], and [`DEFAULT_PAGE`] when it is 0 or less.
    ///
    /// # Returns
    /// A size from 1 to [`MAX_PAGE`].
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "trivial accessor or conversion: maps a value, no I/O"
        )
    )]
    pub fn page_size(&self) -> i64 {
        if self.limit <= 0 {
            DEFAULT_PAGE
        } else {
            self.limit.min(MAX_PAGE)
        }
    }
}

/// The filters as text (a query string, command-line flags), before
/// [`parse_filters`] checks them. An empty value is no filter.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FilterText<'a> {
    /// [`AuditQuery::team`].
    pub team: Option<&'a str>,
    /// An action name such as `user.created`.
    pub action: Option<&'a str>,
    /// [`AuditQuery::actor`].
    pub actor: Option<&'a str>,
    /// [`AuditQuery::target`].
    pub target: Option<&'a str>,
    /// An RFC 3339 time.
    pub since: Option<&'a str>,
    /// An RFC 3339 time.
    pub until: Option<&'a str>,
    /// [`AuditQuery::after`], as [`AuditCursor::to_plain`] writes it.
    pub after: Option<&'a str>,
    /// [`AuditQuery::limit`]; [`DEFAULT_PAGE`] when absent.
    pub limit: Option<i64>,
}

/// Checks text filters and turns them into a query: shared by the REST
/// routes and the CLI, so both refuse the same input the same way.
///
/// # Arguments
/// * `text` - the filters as given.
///
/// # Returns
/// The query; empty values are dropped.
///
/// # Errors
/// [`OkfError::InvalidPath`] (400) for an unknown action, a time that is
/// not RFC 3339, or an `after` that is not a cursor, naming the filter.
#[tracing::instrument(level = "debug", skip_all)]
pub fn parse_filters<'a>(text: &FilterText<'a>) -> Result<AuditQuery<'a>, OkfError> {
    // `filter` turns `Some("")` into `None`: an empty form field filters nothing.
    let given = |value: Option<&'a str>| value.filter(|value| !value.is_empty());
    let invalid = |filter: &str, reason: String| OkfError::InvalidPath {
        path: filter.to_string(),
        reason,
    };
    let time = |filter: &str, value: Option<&'a str>| {
        given(value)
            .map(|value| {
                DateTime::parse_from_rfc3339(value)
                    .map(|time| time.with_timezone(&Utc))
                    .map_err(|_| invalid(filter, format!("{value:?} is not an RFC 3339 time")))
            })
            // `transpose` turns `Option<Result<T, E>>` into `Result<Option<T>, E>`,
            // so `?` can return the error.
            .transpose()
    };
    let action = given(text.action)
        .map(|name| {
            Action::parse(name)
                .ok_or_else(|| invalid("action", format!("{name:?} is not an audit action")))
        })
        .transpose()?;
    Ok(AuditQuery {
        team: given(text.team),
        action,
        actor: given(text.actor),
        target: given(text.target),
        since: time("since", text.since)?,
        until: time("until", text.until)?,
        after: given(text.after)
            .map(AuditCursor::parse_plain)
            .transpose()?,
        limit: text.limit.unwrap_or(DEFAULT_PAGE),
    })
}

/// Who an entry shows as acting.
// `tag = "kind"` puts the variant's name in a `kind` field next to its own
// fields: `{"kind": "operator"}` or `{"kind": "known", "person": ...}`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ActorView {
    /// An admin credential or a provisioner, as a team owner sees it: no
    /// person, credential, label or IP.
    Operator,
    /// Anyone else, or an operator as an admin sees it.
    Known {
        /// The person, if the actor was one (`deleted user #N` once deleted).
        person: Option<String>,
        /// The person's label from the audit snapshot of their display name:
        /// never their slug, and `Unnamed user` once the user is deleted.
        /// `None` when `person` is.
        person_label: Option<String>,
        /// The key or grant id (`credential #N` once its user is deleted).
        credential: Option<String>,
        /// The credential's label, or the entry's actor label.
        label: String,
        /// Whether it was an operator credential.
        operator: bool,
    },
}

/// An entry's target as a reader sees it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TargetView {
    /// `user`, `team`, `catalog`, `key`, `grant`, `code`, `setting` or `unlock`.
    pub kind: String,
    /// Its name (`None` for a user, and for a hidden credential).
    pub name: Option<String>,
    /// The person it is about, if any, and if the reader may see them.
    pub person: Option<String>,
    /// The person's label from the audit snapshot (see
    /// [`ActorView::Known`]'s `person_label`); `None` when `person` is.
    pub person_label: Option<String>,
    /// The label of a catalog target that still exists, or of a shared
    /// team target, read from the directory now; `None` otherwise (a
    /// deleted catalog, a personal team, any other kind).
    pub target_label: Option<String>,
}

/// One entry as a reader sees it.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct AuditRecord {
    /// Its id in its source, for an admin or provisioner; `None` for a team
    /// owner, whose payload then has no `id` key at all (issue #430): ids
    /// are sequences, so the gaps between an owner's entries would show how
    /// much every other team did in between. An id is only unique with
    /// [`AuditRecord::store`]: each store numbers its own entries.
    // `skip_serializing_if` leaves the key out of the JSON when it is `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<i64>,
    /// The store the entry is on, for an admin or provisioner; `None` for
    /// the account database and for a team owner (store names are the
    /// operator's).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub store: Option<String>,
    /// Its position for the server's own use: the next page's cursor, which
    /// the owner routes seal (`http::api::audit::cursor`) before an owner
    /// sees it. Never serialized, in any view.
    #[serde(skip)]
    pub cursor: AuditCursor,
    /// When (`2026-11-13T12:00:00Z`).
    pub at: String,
    /// The action.
    pub action: String,
    /// `cli`, `api`, `console` or `system`.
    pub source: String,
    /// Who.
    pub actor: ActorView,
    /// The client address, when the reader may see it (`null` otherwise).
    pub ip: Option<String>,
    /// What.
    pub target: TargetView,
    /// The teams it belongs to, personal teams shown as their user; for an
    /// owner, only the owner's own team.
    pub teams: Vec<String>,
    /// Its details, redacted for owners.
    pub details: serde_json::Value,
    /// For `catalog.admin_read`: the request count.
    pub requests: Option<i64>,
}

/// The `details` keys a team owner never sees, per action: an unlock's
/// free-text reason (spec Part 6). Add any free-text detail a new action
/// records here.
pub const OWNER_REDACTED: &[(&str, &[&str])] = &[("catalog.unlocked", &["reason"])];

/// The filters and the viewer, as [`list`] hands them to each source.
struct ViewScope<'a> {
    /// An owner's `audit_teams` id: the one team an entry must belong to.
    scope: Option<i64>,
    /// An admin's or provisioner's team filter, by name.
    team_name: Option<&'a str>,
    /// An owner's `teams.id` (for the member-IP rule).
    team_id: Option<&'a str>,
    /// A provisioner's key id: only the entries it wrote.
    credential: Option<&'a str>,
    /// Whether an owner reads (operator staff are redacted).
    owner: bool,
}

impl<'a> ViewScope<'a> {
    /// What the viewer and the query ask of every source.
    ///
    /// # Arguments
    /// * `viewer` - who reads.
    /// * `query` - the filters.
    ///
    /// # Returns
    /// The scope.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(uninstrumented_fn, reason = "maps a value, no I/O")
    )]
    fn of(viewer: &'a Viewer, query: &AuditQuery<'a>) -> Self {
        // `scope`: the one audit team an owner's entries must belong to.
        let (scope, team_name, team_id, credential) = match viewer {
            Viewer::Admin => (None, query.team, None, None),
            Viewer::Owner {
                team_id,
                audit_team,
            } => (Some(*audit_team), None, Some(team_id.as_str()), None),
            Viewer::Credential(id) => (None, query.team, None, Some(id.as_str())),
        };
        Self {
            scope,
            team_name,
            team_id,
            credential,
            owner: matches!(viewer, Viewer::Owner { .. }),
        }
    }
}

/// A cursor's three binds for one source: the time, the id, and how the
/// source's name sorts against the cursor's ([`AuditCursor::ties_for`]).
///
/// # Arguments
/// * `after` - the cursor, if any.
/// * `source` - the source's store name, `""` for the account database.
///
/// # Returns
/// `(None, 0, 0)` without a cursor.
#[cfg_attr(
    dylint_lib = "hyp_tracing",
    allow(uninstrumented_fn, reason = "maps a value, no I/O")
)]
fn after_binds(after: Option<&AuditCursor>, source: &str) -> (Option<DateTime<Utc>>, i64, i32) {
    match after {
        Some(cursor) => (
            DateTime::from_timestamp_micros(cursor.at_micros),
            cursor.id,
            cursor.ties_for(source),
        ),
        None => (None, 0, 0),
    }
}

/// The entries a viewer may read, newest first, from the account database
/// and every store.
///
/// The account database (the account actions, and every catalog event
/// written before the split, plan decision P7) and each store's
/// `catalog_audit_entries` (the catalog events since) are read
/// concurrently, each with the same filters and limit, and merged by
/// `(at, store, id)` descending ([`AuditCursor`]), so an entry appears
/// once and a page never skips or repeats one. A store's rows hold the
/// account database's person, credential and team ids as plain values:
/// the filters that name an actor, team or target are turned into those
/// ids first, and names are read from the account database after the
/// merge, so deleting a user (which clears their name there) clears it in
/// the store's entries too. Entries stay visible after their catalog is
/// deleted or transferred: nothing here reads the catalog's store side.
///
/// For an owner, the actor filter never matches an operator's entry and the
/// target filter never matches a hidden (operator) person or credential, so
/// neither can reveal who operator staff are.
///
/// # Arguments
/// * `stores` - the account database and the stores.
/// * `viewer` - who reads.
/// * `query` - the filters.
/// * `labels` - the lookup that labels catalog and shared-team targets.
///
/// # Returns
/// At most [`AuditQuery::page_size`] records. A filter that cannot name
/// anything (a malformed team name, a NUL) matches nothing, without SQL.
///
/// # Errors
/// [`OkfError::Database`]; [`OkfError::StoreUnavailable`] when a store
/// cannot be read: a page that silently left a store out would hide
/// entries and skip them for good once the cursor passed their time.
#[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "audit.list"))]
pub async fn list(
    stores: &Stores,
    viewer: &Viewer,
    query: &AuditQuery<'_>,
    labels: &LabelLookup,
) -> Result<Vec<AuditRecord>, OkfError> {
    // Postgres refuses a NUL in text, and no stored name holds one (team
    // names are `[a-z0-9-]+`): such a filter is "nothing", never a 500.
    let unmatchable = query
        .team
        .is_some_and(|team| !crate::store::catalogs::is_valid_name(team))
        || [query.actor, query.target]
            .into_iter()
            .flatten()
            .any(|text| text.contains('\0'));
    if unmatchable {
        return Ok(Vec::new());
    }
    let scope = ViewScope::of(viewer, query);
    // Both reads at once: the account database, and the stores.
    let (account, stored) = tokio::try_join!(
        account_rows(stores.accounts(), &scope, query),
        store_rows(stores, &scope, query),
    )?;
    let mut rows = account;
    rows.extend(stored);
    // Newest first by the view's order; each source is already in it, so
    // the first `page_size` of the union are the page.
    rows.sort_unstable_by(|a, b| (b.at, &b.store, b.id).cmp(&(a.at, &a.store, a.id)));
    rows.truncate(usize::try_from(query.page_size()).unwrap_or(usize::MAX));
    // The labels of this page's catalog and team targets, read together.
    let mut wanted = Wanted::default();
    for row in &rows {
        if let (true, Some(slug)) = (row.target_kind == "catalog", row.target_catalog.as_deref()) {
            wanted.catalog(slug);
        }
        if let (true, Some(slug)) = (row.target_kind == "team", row.target_team.as_deref()) {
            wanted.team(slug);
        }
    }
    let labels = labels.labels_for(&wanted).await?;
    Ok(rows
        .into_iter()
        .map(|row| row.into_record(scope.owner, &labels))
        .collect())
}

/// The account database's page: [`LIST_SQL`].
///
/// # Arguments
/// * `accounts` - the account database.
/// * `scope` - the viewer's scope.
/// * `query` - the filters.
///
/// # Returns
/// At most one page of rows, newest first, with `store` empty.
///
/// # Errors
/// [`OkfError::Database`].
#[tracing::instrument(level = "debug", skip_all)]
async fn account_rows(
    accounts: AccountDb,
    scope: &ViewScope<'_>,
    query: &AuditQuery<'_>,
) -> Result<Vec<ListedRow>, OkfError> {
    let (after_at, after_id, ties) = after_binds(query.after.as_ref(), "");
    Ok(sqlx::query_as::<_, ListedRow>(LIST_SQL)
        .bind(scope.scope)
        .bind(query.action.map(Action::as_str))
        .bind(query.actor)
        .bind(query.target)
        .bind(query.since)
        .bind(query.until)
        .bind(after_at)
        .bind(scope.credential)
        .bind(scope.team_id)
        .bind(scope.owner)
        .bind(query.page_size())
        .bind(scope.team_name)
        .bind(after_id)
        .bind(ties)
        .fetch_all(accounts.executor())
        .await?)
}

/// The account ids a store's rows are filtered by: what the filters that
/// name an actor, team or target stand for in `audit_people`,
/// `audit_credentials` and `audit_teams`, which a store cannot join.
#[derive(Debug, Default, sqlx::FromRow)]
struct FilterIds {
    /// Every audit team that had the team filter's name.
    teams: Vec<i64>,
    /// The people the actor filter names.
    actor_people: Vec<i64>,
    /// The credentials the actor filter names (by id or label).
    actor_credentials: Vec<i64>,
    /// The people the target filter names.
    target_people: Vec<i64>,
    /// The provisioner's own credential.
    viewer_credentials: Vec<i64>,
}

/// Turns the filters into account ids, in one statement.
///
/// # Arguments
/// * `accounts` - the account database.
/// * `scope` - the viewer's scope.
/// * `query` - the filters.
///
/// # Returns
/// The ids; empty vectors for a filter that is not set or names nobody.
///
/// # Errors
/// [`OkfError::Database`].
#[tracing::instrument(level = "debug", skip_all)]
async fn filter_ids(
    accounts: &AccountDb,
    scope: &ViewScope<'_>,
    query: &AuditQuery<'_>,
) -> Result<FilterIds, OkfError> {
    if scope.team_name.is_none()
        && scope.credential.is_none()
        && query.actor.is_none()
        && query.target.is_none()
    {
        return Ok(FilterIds::default());
    }
    Ok(sqlx::query_as::<_, FilterIds>(
        "SELECT ARRAY(SELECT t.id FROM audit_teams t \
                      LEFT JOIN audit_people p ON p.id = t.person_id \
                      WHERE t.name = $1::text OR p.name = $1::text) AS teams, \
                ARRAY(SELECT id FROM audit_people WHERE name = $2::text) AS actor_people, \
                ARRAY(SELECT id FROM audit_credentials \
                      WHERE credential_id = $2::text OR label = $2::text) AS actor_credentials, \
                ARRAY(SELECT id FROM audit_people WHERE name = $3::text) AS target_people, \
                ARRAY(SELECT id FROM audit_credentials \
                      WHERE credential_id = $4::text) AS viewer_credentials",
    )
    .bind(scope.team_name)
    .bind(query.actor)
    .bind(query.target)
    .bind(scope.credential)
    .fetch_one(accounts.executor())
    .await?)
}

/// One row of a store's `catalog_audit_entries`, before its names are read
/// from the account database.
#[derive(Debug, sqlx::FromRow)]
struct StoredRow {
    id: i64,
    at: DateTime<Utc>,
    action: String,
    source: String,
    actor_credential_ref: Option<i64>,
    actor_person_id: Option<i64>,
    actor_label: String,
    actor_operator: bool,
    client_ip: Option<String>,
    target_name: String,
    target_person_id: Option<i64>,
    target_operator: bool,
    teams: Vec<i64>,
    details: serde_json::Value,
    // Set by the reader that fetched it, not by the query.
    #[sqlx(skip)]
    store: String,
}

/// The stores' page: each store's newest entries (one page each, with the
/// same filters), merged, cut to a page and given their names.
///
/// # Arguments
/// * `stores` - the registry.
/// * `scope` - the viewer's scope.
/// * `query` - the filters.
///
/// # Returns
/// At most one page of rows, in no particular order.
///
/// # Errors
/// [`OkfError::Database`]; [`OkfError::StoreUnavailable`] for a store that
/// cannot be reached.
#[tracing::instrument(level = "debug", skip_all)]
async fn store_rows(
    stores: &Stores,
    scope: &ViewScope<'_>,
    query: &AuditQuery<'_>,
) -> Result<Vec<ListedRow>, OkfError> {
    // A store holds the six catalog events only: any other action's filter
    // matches nothing there.
    if query
        .action
        .is_some_and(|action| !action.is_catalog_event())
    {
        return Ok(Vec::new());
    }
    let names = stores.all().await?;
    if names.is_empty() {
        return Ok(Vec::new());
    }
    let ids = filter_ids(&stores.accounts(), scope, query).await?;
    // Every store at once. `try_join_all` returns the first error and drops
    // (cancels) the rest.
    let pages = futures_util::future::try_join_all(
        names
            .iter()
            .map(|name| stored_page(stores, name, scope, query, &ids)),
    )
    .await?;
    let mut rows: Vec<StoredRow> = pages.into_iter().flatten().collect();
    rows.sort_unstable_by(|a, b| (b.at, &b.store, b.id).cmp(&(a.at, &a.store, a.id)));
    rows.truncate(usize::try_from(query.page_size()).unwrap_or(usize::MAX));
    resolve_stored(&stores.accounts(), scope, &rows).await
}

/// One store's page of raw rows.
///
/// # Arguments
/// * `stores` - the registry.
/// * `name` - the store.
/// * `scope` - the viewer's scope.
/// * `query` - the filters.
/// * `ids` - the account ids the filters stand for.
///
/// # Returns
/// At most one page, newest first, each tagged with the store's name.
///
/// # Errors
/// [`OkfError::StoreUnavailable`]; [`OkfError::Database`].
#[tracing::instrument(level = "debug", skip_all)]
async fn stored_page(
    stores: &Stores,
    name: &StoreName,
    scope: &ViewScope<'_>,
    query: &AuditQuery<'_>,
    ids: &FilterIds,
) -> Result<Vec<StoredRow>, OkfError> {
    let db = stores.get(name).await?;
    let (after_at, after_id, ties) = after_binds(query.after.as_ref(), name.as_str());
    let mut rows = sqlx::query_as::<_, StoredRow>(STORE_LIST_SQL)
        .bind(scope.scope)
        .bind(query.action.map(Action::as_str))
        .bind(query.actor)
        .bind(query.target)
        .bind(query.since)
        .bind(query.until)
        .bind(after_at)
        .bind(after_id)
        .bind(ties)
        .bind(scope.owner)
        .bind(query.page_size())
        // `team_name.map(..)`: no team filter binds NULL, which matches all.
        .bind(scope.team_name.map(|_| &ids.teams))
        .bind(&ids.actor_people)
        .bind(&ids.actor_credentials)
        .bind(&ids.target_people)
        .bind(scope.credential.map(|_| &ids.viewer_credentials))
        .fetch_all(db.executor())
        .await
        .map_err(|error| db.classify(error))?;
    for row in &mut rows {
        row.store = name.to_string();
    }
    Ok(rows)
}

/// Gives raw store rows their names, from the account database: the
/// listing's own joins ([`listed_columns!`]), over the rows passed as JSON.
///
/// # Arguments
/// * `accounts` - the account database.
/// * `scope` - the viewer's scope.
/// * `rows` - the raw rows.
///
/// # Returns
/// The rows as the listing reads them.
///
/// # Errors
/// [`OkfError::Database`].
#[tracing::instrument(level = "debug", skip_all)]
async fn resolve_stored(
    accounts: &AccountDb,
    scope: &ViewScope<'_>,
    rows: &[StoredRow],
) -> Result<Vec<ListedRow>, OkfError> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let as_json: Vec<serde_json::Value> = rows
        .iter()
        .map(|row| {
            serde_json::json!({
                "store": row.store,
                "id": row.id,
                "at": row.at,
                "action": row.action,
                "source": row.source,
                "actor_credential_ref": row.actor_credential_ref,
                "actor_person_id": row.actor_person_id,
                "actor_label": row.actor_label,
                "actor_operator": row.actor_operator,
                "client_ip": row.client_ip,
                "target_kind": "catalog",
                "target_name": row.target_name,
                "target_person_id": row.target_person_id,
                "target_operator": row.target_operator,
                "teams": row.teams,
                "details": row.details,
            })
        })
        .collect();
    Ok(sqlx::query_as::<_, ListedRow>(RESOLVE_SQL)
        .bind(scope.scope)
        .bind(scope.team_id)
        .bind(serde_json::Value::Array(as_json))
        .fetch_all(accounts.executor())
        .await?)
}

/// Every entry a viewer may read that the filters select, as JSON Lines
/// (one [`AuditRecord`] per line, newest first): the export behind `GET
/// /api/v1/audit/export` and `okf-mcp audit export`, so the two cannot
/// differ. `query.after` is a starting cursor and `query.limit` is
/// ignored: it reads [`MAX_PAGE`] entries at a time, moving the cursor to
/// each full page's last entry, until a page is not full. Retention bounds
/// the total.
///
/// # Arguments
/// * `stores` - the account database and the stores.
/// * `viewer` - who reads ([`list`] redacts for them).
/// * `query` - the filters.
/// * `labels` - the lookup [`list`] labels targets with.
///
/// # Returns
/// The lines, each ending in `\n` (empty when nothing matches).
///
/// # Errors
/// As [`list`].
#[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "audit.export_lines"))]
pub async fn export_lines(
    stores: &Stores,
    viewer: &Viewer,
    query: &AuditQuery<'_>,
    labels: &LabelLookup,
) -> Result<String, OkfError> {
    // A copy with the page size fixed; `..query.clone()` takes the other
    // fields from the caller's query, like `dataclasses.replace`.
    let mut page_query = AuditQuery {
        limit: MAX_PAGE,
        ..query.clone()
    };
    let mut lines = String::new();
    loop {
        let page = list(stores, viewer, &page_query, labels).await?;
        for record in &page {
            // A record holds only strings, numbers and JSON values, so
            // serializing it cannot fail.
            lines.push_str(&serde_json::to_string(record).expect("an audit record serializes"));
            lines.push('\n');
        }
        match page.last() {
            Some(last) if page.len() as i64 == MAX_PAGE => {
                page_query.after = Some(last.cursor.clone());
            }
            _ => return Ok(lines),
        }
    }
}

/// The listing's select list, as a macro so `concat!` can splice it into
/// both queries that read it ([`LIST_SQL`] for the account database,
/// [`RESOLVE_SQL`] for a store's rows): names are always read the same
/// way. It reads the entry as `e` and needs the joins of [`listed_joins!`].
/// `$scope` and `$team_id` name the bind parameters holding an owner's
/// audit team and `teams.id`.
macro_rules! listed_columns {
    ($scope:literal, $team_id:literal) => {
        concat!(
            "e.id, e.at, e.action, e.source, e.actor_operator, \
           COALESCE(ac.credential_id, 'credential #' || ac.id) AS actor_credential, \
           COALESCE(ac.label, e.actor_label) AS actor_label, \
           ap.name AS actor_person, ap.display_name AS actor_display_name, \
           host(e.client_ip) AS ip, e.target_kind, \
           COALESCE(e.target_name, tt.name, ttp.name, tc.credential_id, 'credential #' || tc.id) \
               AS target_name, \
           tp.name AS target_person, tp.display_name AS target_display_name, \
           CASE WHEN EXISTS (SELECT 1 FROM teams lt WHERE lt.id = tt.team_id) \
                THEN tt.name END AS target_team, \
           CASE WHEN e.target_kind = 'catalog' AND EXISTS ( \
                    SELECT 1 FROM catalogs lc WHERE lc.name = e.target_name AND lc.created_at <= e.at) \
                THEN e.target_name END AS target_catalog, \
           e.target_operator, \
           ARRAY(SELECT COALESCE(t.name, p.name) \
                 FROM unnest(e.teams) WITH ORDINALITY AS r (id, n) \
                 JOIN audit_teams t ON t.id = r.id \
                 LEFT JOIN audit_people p ON p.id = t.person_id \
                 WHERE ",
            $scope,
            "::bigint IS NULL OR r.id = ",
            $scope,
            " ORDER BY r.n) AS teams, \
           e.details, \
           (",
            $team_id,
            "::text IS NOT NULL AND EXISTS ( \
               SELECT 1 FROM audit_people p JOIN team_members m ON m.user_id = p.user_id \
               WHERE p.id = e.actor_person_id AND m.team_id = ",
            $team_id,
            ")) AS actor_in_team"
        )
    };
}

/// The joins [`listed_columns!`] reads names through: the entry's actor,
/// target and credentials in the account database's audit tables.
macro_rules! listed_joins {
    () => {
        "LEFT JOIN audit_credentials ac ON ac.id = e.actor_credential_ref \
         LEFT JOIN audit_people ap ON ap.id = e.actor_person_id \
         LEFT JOIN audit_people tp ON tp.id = e.target_person_id \
         LEFT JOIN audit_teams tt ON tt.id = e.target_team_ref \
         LEFT JOIN audit_people ttp ON ttp.id = tt.person_id \
         LEFT JOIN audit_credentials tc ON tc.id = e.target_credential_ref"
    };
}

/// The account database's listing. `$1` an owner's audit team id, `$2`
/// action, `$3` actor, `$4` target, `$5` since, `$6` until, `$7` the
/// cursor's time, `$8` credential, `$9` the owner's `teams.id`, `$10` owner
/// view, `$11` limit, `$12` a team name (every audit team that had it, a
/// personal team by its person), `$13` the cursor's id and `$14` how this
/// source's name sorts against the cursor's ([`AuditCursor::ties_for`]).
/// Newest first is `ORDER BY at DESC, id DESC`, the view's order within one
/// source. The target filter's `target_name` and `tt.name` branches need no
/// operator guard: they hold only catalog, setting and unlock names and
/// shared teams' names (a personal team's `name` is NULL), never a
/// person's.
const LIST_SQL: &str = concat!(
    "SELECT ",
    listed_columns!("$1", "$9"),
    ", ''::text AS store, c.requests \
     FROM audit_entries e ",
    listed_joins!(),
    " LEFT JOIN audit_access_counts c ON c.entry_id = e.id \
    WHERE ($1::bigint IS NULL OR e.teams @> ARRAY[$1]) \
      AND ($12::text IS NULL OR e.teams && ARRAY( \
           SELECT t.id FROM audit_teams t LEFT JOIN audit_people p ON p.id = t.person_id \
           WHERE t.name = $12 OR p.name = $12)) \
      AND ($2::text IS NULL OR e.action = $2) \
      AND ($3::text IS NULL OR ((NOT $10 OR NOT e.actor_operator) \
           AND (ap.name = $3 OR ac.credential_id = $3 OR ac.label = $3 OR e.actor_label = $3))) \
      AND ($4::text IS NULL OR e.target_name = $4 OR tt.name = $4 \
           OR ((NOT $10 OR NOT e.target_operator) \
               AND (tp.name = $4 OR ttp.name = $4 OR tc.credential_id = $4))) \
      AND ($5::timestamptz IS NULL OR e.at >= $5) \
      AND ($6::timestamptz IS NULL OR e.at < $6) \
      AND ($7::timestamptz IS NULL OR e.at < $7 \
           OR (e.at = $7 AND ($14::int > 0 OR ($14::int = 0 AND e.id < $13::bigint)))) \
      AND ($8::text IS NULL OR ac.credential_id = $8) \
    ORDER BY e.at DESC, e.id DESC LIMIT $11"
);

/// A store's listing, with the same filters as [`LIST_SQL`] translated to
/// the account ids a store holds ([`FilterIds`]). `$1` an owner's audit
/// team id, `$2` action, `$3` actor text, `$4` target text, `$5` since, `$6`
/// until, `$7` the cursor's time, `$8` its id, `$9` how this store's name
/// sorts against the cursor's, `$10` owner view, `$11` limit, `$12` the
/// team filter's audit teams (NULL for no filter), `$13` the actor's
/// people, `$14` its credentials, `$15` the target's people, `$16` a
/// provisioner's credential (NULL for another viewer).
const STORE_LIST_SQL: &str = "\
    SELECT e.id, e.at, e.action, e.source, e.actor_credential_ref, e.actor_person_id, \
           e.actor_label, e.actor_operator, host(e.client_ip) AS client_ip, e.target_name, \
           e.target_person_id, e.target_operator, e.teams, e.details \
    FROM catalog_audit_entries e \
    WHERE ($1::bigint IS NULL OR e.teams @> ARRAY[$1]) \
      AND ($12::bigint[] IS NULL OR e.teams && $12) \
      AND ($2::text IS NULL OR e.action = $2) \
      AND ($3::text IS NULL OR ((NOT $10 OR NOT e.actor_operator) \
           AND (e.actor_person_id = ANY($13::bigint[]) \
                OR e.actor_credential_ref = ANY($14::bigint[]) OR e.actor_label = $3))) \
      AND ($4::text IS NULL OR e.target_name = $4 \
           OR ((NOT $10 OR NOT e.target_operator) AND e.target_person_id = ANY($15::bigint[]))) \
      AND ($5::timestamptz IS NULL OR e.at >= $5) \
      AND ($6::timestamptz IS NULL OR e.at < $6) \
      AND ($7::timestamptz IS NULL OR e.at < $7 \
           OR (e.at = $7 AND ($9::int > 0 OR ($9::int = 0 AND e.id < $8::bigint)))) \
      AND ($16::bigint[] IS NULL OR e.actor_credential_ref = ANY($16)) \
    ORDER BY e.at DESC, e.id DESC LIMIT $11";

/// Reads store rows' names from the account database: [`listed_columns!`]
/// over the rows passed as JSON (`$3`), shaped like `audit_entries` rows
/// plus their `store`. `$1` an owner's audit team id, `$2` the owner's
/// `teams.id`. A store row's `id` is its own: it is never joined to
/// `audit_access_counts`, whose entries are the account database's.
const RESOLVE_SQL: &str = concat!(
    "SELECT ",
    listed_columns!("$1", "$2"),
    ", e.store, NULL::bigint AS requests \
     FROM jsonb_to_recordset($3::jsonb) AS e (store text, id bigint, at timestamptz, \
         action text, source text, actor_credential_ref bigint, actor_person_id bigint, \
         actor_label text, actor_operator boolean, client_ip inet, target_kind text, \
         target_name text, target_person_id bigint, target_operator boolean, \
         teams bigint[], details jsonb, target_team_ref bigint, target_credential_ref bigint) ",
    listed_joins!()
);

/// One row of [`LIST_SQL`] or [`RESOLVE_SQL`], before redaction.
// `FromRow` maps each selected column to the field of the same name.
#[derive(Debug, sqlx::FromRow)]
struct ListedRow {
    id: i64,
    at: DateTime<Utc>,
    // The store the entry is on; empty for the account database.
    store: String,
    action: String,
    source: String,
    actor_operator: bool,
    actor_credential: Option<String>,
    actor_label: String,
    actor_person: Option<String>,
    actor_display_name: Option<String>,
    ip: Option<String>,
    target_kind: String,
    target_name: Option<String>,
    target_person: Option<String>,
    target_display_name: Option<String>,
    // The slug of a shared team target while the same team (by `teams.id`)
    // still exists; `None` for a personal team or a deleted one, even when a
    // new team has taken its slug.
    target_team: Option<String>,
    // The name of a catalog target while that catalog still exists: the log
    // keeps no catalog id, so a catalog created after the entry is another
    // one reusing the name.
    target_catalog: Option<String>,
    target_operator: bool,
    teams: Vec<String>,
    details: serde_json::Value,
    requests: Option<i64>,
    actor_in_team: bool,
}

/// A person's label from the audit snapshot of their display name. The
/// snapshot has no email, so there is no email fallback; a deleted user's
/// snapshot is blank, so it reads as the placeholder, never the slug.
///
/// # Arguments
/// * `person` - the snapshot's person name, if the entry has one.
/// * `display_name` - the snapshot's display name (joined with `person`).
///
/// # Returns
/// The label, or `None` when there is no person.
#[tracing::instrument(level = "trace", skip_all)]
fn snapshot_label(person: &Option<String>, display_name: &Option<String>) -> Option<String> {
    person
        .as_ref()
        .map(|_| user_label(display_name.as_deref().unwrap_or_default(), None))
}

/// The label of a target that is a catalog or a shared team, read from the
/// directory now.
///
/// # Arguments
/// * `kind` - the target's kind.
/// * `name` - the slug of a catalog target that still exists.
/// * `team` - the slug of a shared team target that still exists.
/// * `labels` - the directory.
///
/// # Returns
/// The label, or `None` for another kind, a deleted catalog or team, or a
/// personal team.
#[tracing::instrument(level = "trace", skip_all)]
fn target_label(
    kind: &str,
    name: Option<&str>,
    team: Option<&str>,
    labels: &Labels,
) -> Option<String> {
    let label = match kind {
        "catalog" => name.and_then(|slug| labels.catalog(slug)),
        "team" => team.and_then(|slug| labels.team(slug)),
        _ => None,
    };
    label.map(str::to_string)
}

impl ListedRow {
    /// The record a reader sees. For an owner: an operator actor becomes
    /// [`ActorView::Operator`] with no IP; other IPs show only for the
    /// team's own members; [`OWNER_REDACTED`] keys leave `details`; a target
    /// person who is operator staff (`target_operator`, such as the admin an
    /// unlock is for) is hidden, with a credential naming them; and `teams`
    /// holds only the owner's team (the SQL drops the others, which may be
    /// another customer's or a staff member's personal team); and the record
    /// has no `id` (issue #430), only the unserialized `cursor`. The stored
    /// entry is never changed.
    #[tracing::instrument(level = "trace", skip_all)]
    fn into_record(self, owner: bool, labels: &Labels) -> AuditRecord {
        let actor = if owner && self.actor_operator {
            ActorView::Operator
        } else {
            ActorView::Known {
                person_label: snapshot_label(&self.actor_person, &self.actor_display_name),
                person: self.actor_person,
                credential: self.actor_credential,
                label: self.actor_label,
                operator: self.actor_operator,
            }
        };
        let ip = if owner && (self.actor_operator || !self.actor_in_team) {
            None
        } else {
            self.ip
        };
        let mut details = self.details;
        if owner {
            let redacted = OWNER_REDACTED
                .iter()
                .find(|(action, _)| *action == self.action);
            // `if let (Some(..), Some(..))` runs only when both are present.
            if let (Some((_, keys)), Some(object)) = (redacted, details.as_object_mut()) {
                for key in *keys {
                    object.remove(*key);
                }
            }
        }
        let target = if owner && self.target_operator {
            let credential = matches!(self.target_kind.as_str(), "key" | "grant" | "code");
            TargetView {
                name: if credential { None } else { self.target_name },
                target_label: None,
                kind: self.target_kind,
                person: None,
                person_label: None,
            }
        } else {
            TargetView {
                target_label: target_label(
                    &self.target_kind,
                    self.target_catalog.as_deref(),
                    self.target_team.as_deref(),
                    labels,
                ),
                person_label: snapshot_label(&self.target_person, &self.target_display_name),
                kind: self.target_kind,
                name: self.target_name,
                person: self.target_person,
            }
        };
        let cursor = AuditCursor::new(self.at, &self.store, self.id);
        AuditRecord {
            id: (!owner).then_some(self.id),
            store: (!owner && !self.store.is_empty()).then_some(self.store),
            cursor,
            at: crate::clock::format_timestamp(self.at),
            action: self.action,
            source: self.source,
            actor,
            ip,
            target,
            teams: self.teams,
            details,
            requests: self.requests,
        }
    }
}

/// Test helpers shared by every module that records entries.
#[cfg(test)]
pub(crate) mod testing {
    use sqlx::PgPool;

    use crate::stores::{PoolSettings, Stores};

    /// A registry over `pool` for a test that holds only the account pool:
    /// the `main` store is the account database itself.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(uninstrumented_fn, reason = "test helper")
    )]
    pub(crate) fn stores_of(pool: &PgPool) -> Stores {
        Stores::new(
            pool.clone(),
            PoolSettings::from_config(&crate::config::Config::default()),
        )
    }

    /// The registry of `db` with a second store, `shared-1`, in a database of
    /// its own (migrated, on the same server and login): the tests of what
    /// the view does across stores.
    ///
    /// # Returns
    /// The registry and the second database, which the test keeps alive.
    pub(crate) async fn with_second_store(
        db: &crate::db::pg_support::TestDb,
    ) -> (Stores, crate::db::pg_support::TestDb) {
        let other = crate::db::test_db().await;
        let options = other.pool.connect_options();
        // The password in `scheme://user:password@host/db`.
        let password = other
            .url
            .split("://")
            .nth(1)
            .and_then(|rest| rest.split('@').next())
            .and_then(|credentials| credentials.split_once(':'))
            .map(|(_, password)| password.to_string())
            .unwrap_or_default();
        sqlx::query(
            "INSERT INTO stores (name, location_kind, host, port, database, \
             credential_kind, credential_ref, created_at) \
             VALUES ('shared-1', 'host', $1, $2, $3, 'env', 'SHARED_1_PASSWORD', now())",
        )
        .bind(options.get_host())
        .bind(i32::from(options.get_port()))
        .bind(options.get_database().expect("a database name"))
        .execute(&db.pool)
        .await
        .expect("register the second store");
        let stores = Stores::with_environment(
            db.pool.clone(),
            PoolSettings::from_config(&crate::config::Config::default()),
            std::sync::Arc::new(move |name| {
                (name == "SHARED_1_PASSWORD").then(|| password.clone())
            }),
        );
        (stores, other)
    }

    /// Every catalog event of this action on the store `main`, oldest
    /// first, with names read from the account database as the view reads
    /// them.
    pub(crate) async fn catalog_entries(pool: &PgPool, action: &str) -> Vec<RecordedEntry> {
        /// A stored row, before its names are read.
        #[derive(sqlx::FromRow)]
        struct Stored {
            source: String,
            actor_label: String,
            target_name: String,
            actor_operator: bool,
            actor_person_id: Option<i64>,
            target_person_id: Option<i64>,
            teams: Vec<i64>,
            details: serde_json::Value,
        }
        let rows: Vec<Stored> = sqlx::query_as(
            "SELECT source, actor_label, target_name, actor_operator, actor_person_id, \
                    target_person_id, teams, details \
             FROM catalog.catalog_audit_entries WHERE action = $1 ORDER BY id",
        )
        .bind(action)
        .fetch_all(pool)
        .await
        .expect("read the catalog audit entries");
        let mut entries = Vec::with_capacity(rows.len());
        for row in rows {
            let (actor, person) = (row.actor_person_id, row.target_person_id);
            let name = |id: Option<i64>| async move {
                match id {
                    Some(id) => sqlx::query_scalar::<_, String>(
                        "SELECT name FROM audit_people WHERE id = $1",
                    )
                    .bind(id)
                    .fetch_optional(pool)
                    .await
                    .expect("read a person"),
                    None => None,
                }
            };
            let team_names: Vec<String> = sqlx::query_scalar(
                "SELECT COALESCE(t.name, p.name) FROM unnest($1::bigint[]) WITH ORDINALITY AS r (id, n) \
                 JOIN audit_teams t ON t.id = r.id LEFT JOIN audit_people p ON p.id = t.person_id \
                 ORDER BY r.n",
            )
            .bind(&row.teams)
            .fetch_all(pool)
            .await
            .expect("read the teams");
            entries.push(RecordedEntry {
                source: row.source,
                actor_label: row.actor_label,
                actor_operator: row.actor_operator,
                actor_person: name(actor).await,
                target_kind: "catalog".to_string(),
                target: Some(row.target_name),
                person: name(person).await,
                teams: team_names,
                details: row.details,
                requests: None,
            });
        }
        entries
    }

    /// A credential's `audit_credentials` row as `(label, owner's user id)`,
    /// or `None` when it has none: what the create paths must leave behind
    /// (M18 spec Decision 20).
    pub(crate) async fn credential_row(
        pool: &PgPool,
        credential_id: &str,
    ) -> Option<(Option<String>, Option<String>)> {
        sqlx::query_as(
            "SELECT a.label, p.user_id FROM audit_credentials a \
             LEFT JOIN audit_people p ON p.id = a.person_id WHERE a.credential_id = $1",
        )
        .bind(credential_id)
        .fetch_optional(pool)
        .await
        .expect("read the credential's audit row")
    }

    /// One stored entry, flattened for assertions.
    #[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
    pub(crate) struct RecordedEntry {
        pub source: String,
        pub actor_label: String,
        pub actor_operator: bool,
        /// The actor's person name, if any.
        pub actor_person: Option<String>,
        pub target_kind: String,
        /// The target's name: a catalog, setting or unlock name, a team's
        /// name (or its person's), or a credential id.
        pub target: Option<String>,
        /// The target person's name (a user target, or `Entry.person`).
        pub person: Option<String>,
        /// The teams' display names, personal teams as their person.
        pub teams: Vec<String>,
        pub details: serde_json::Value,
        /// A `catalog.admin_read` entry's request count.
        pub requests: Option<i64>,
    }

    /// The [`super::AccessRecord`] admission would build for `actor` on
    /// `catalog`, its ids resolved with SQL as the key cache resolves them.
    pub(crate) async fn access_record(
        pool: &PgPool,
        actor: &super::AuditActor,
        catalog: &str,
        at: chrono::DateTime<chrono::Utc>,
    ) -> super::AccessRecord {
        let (credential, person, id, team): (Option<i64>, Option<i64>, i64, Option<i64>) =
            sqlx::query_as(
                "SELECT audit_credential($1, NULL, $2), audit_person($3), c.id, \
                        audit_team_ref(c.owner_team_id) \
                 FROM catalogs c WHERE c.name = $4",
            )
            .bind(&actor.credential)
            .bind(&actor.label)
            .bind(&actor.user_id)
            .bind(catalog)
            .fetch_one(pool)
            .await
            .expect("resolve the access");
        super::AccessRecord {
            actor: super::AdmittedActor {
                source: actor.source,
                credential,
                person,
                label: actor.stored_label(),
                operator: actor.operator,
                ip: actor.ip,
                at,
            },
            catalog: catalog.to_string(),
            scope: crate::auth::CatalogScope { id, team },
        }
    }

    /// Every entry with this action, oldest first: from the store `main`
    /// for a catalog event ([`super::Action::is_catalog_event`]), else from
    /// the account database.
    pub(crate) async fn entries(pool: &PgPool, action: &str) -> Vec<RecordedEntry> {
        if super::Action::parse(action).is_some_and(super::Action::is_catalog_event) {
            return catalog_entries(pool, action).await;
        }
        sqlx::query_as::<_, RecordedEntry>(
            "SELECT e.source, e.actor_label, e.actor_operator, ap.name AS actor_person, \
                    e.target_kind, \
                    COALESCE(e.target_name, tt.name, ttp.name, tc.credential_id) AS target, \
                    tp.name AS person, \
                    ARRAY(SELECT COALESCE(t.name, p.name) \
                          FROM unnest(e.teams) WITH ORDINALITY AS r (id, n) \
                          JOIN audit_teams t ON t.id = r.id \
                          LEFT JOIN audit_people p ON p.id = t.person_id ORDER BY r.n) AS teams, \
                    e.details, c.requests \
             FROM audit_entries e \
             LEFT JOIN audit_people ap ON ap.id = e.actor_person_id \
             LEFT JOIN audit_people tp ON tp.id = e.target_person_id \
             LEFT JOIN audit_teams tt ON tt.id = e.target_team_ref \
             LEFT JOIN audit_people ttp ON ttp.id = tt.person_id \
             LEFT JOIN audit_credentials tc ON tc.id = e.target_credential_ref \
             LEFT JOIN audit_access_counts c ON c.entry_id = e.id \
             WHERE e.action = $1 ORDER BY e.id",
        )
        .bind(action)
        .fetch_all(pool)
        .await
        .expect("read the audit entries")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{test_db, TEST_TEAM, TEST_USER};
    use crate::http::requester::Requester;
    use crate::store::teams::{self, NewTeam};
    use crate::store::users::{self, NewUser};
    use chrono::TimeDelta;
    use sqlx::PgPool;

    /// [`list`] with a fresh label lookup.
    async fn list_plain(
        pool: &PgPool,
        viewer: &Viewer,
        query: &AuditQuery<'_>,
    ) -> Result<Vec<AuditRecord>, OkfError> {
        let lookup = LabelLookup::new(pool.clone(), 100, std::time::Duration::from_secs(60));
        list(&testing::stores_of(pool), viewer, query, &lookup).await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_audit_page_reads_its_target_labels_once() {
        use crate::mcp::testing::{local_metrics, render};
        use crate::store::catalogs::{self, NewCatalog};
        let db = crate::db::test_db().await;
        for n in 0..5 {
            let display = crate::labels::DisplayName::parse(&format!("Notes {n}")).unwrap();
            let new = NewCatalog {
                name: &format!("notes-{n}"),
                display_name: &display,
                description: "",
                owner_team: crate::db::TEST_TEAM,
            };
            catalogs::create(&db.pool, &new, &AuditActor::cli(), now())
                .await
                .unwrap();
        }
        let (_provider, metrics, registry) = local_metrics();
        let lookup = LabelLookup::new(db.pool.clone(), 100, std::time::Duration::from_secs(60))
            .with_metrics(metrics);

        let page = list(
            &testing::stores_of(&db.pool),
            &Viewer::Admin,
            &AuditQuery::default(),
            &lookup,
        )
        .await
        .unwrap();

        let labelled = page
            .iter()
            .filter_map(|record| record.target.target_label.as_deref())
            .filter(|label| label.starts_with("Notes "))
            .count();
        assert_eq!(labelled, 5, "every catalog target is labelled");
        let loads: u64 = render(&registry)
            .lines()
            .find(|line| {
                line.starts_with("okf_cache_load_duration_seconds_count")
                    && line.contains("cache=\"labels\"")
            })
            .and_then(|line| line.rsplit(' ').next())
            .and_then(|count| count.parse().ok())
            .unwrap_or(0);
        assert_eq!(loads, 1, "one read for the whole page");
    }

    fn now() -> DateTime<Utc> {
        "2026-11-13T12:00:00Z".parse().unwrap()
    }

    async fn user_id(pool: &PgPool, name: &str) -> String {
        sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
            .bind(name)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    fn admin_actor(user_id: &str) -> AuditActor {
        AuditActor {
            source: Source::Api,
            credential: Some("k1".to_string()),
            user_id: Some(user_id.to_string()),
            label: "root".to_string(),
            via: Via::Key,
            operator: true,
            provisioner: false,
            ip: Some("192.0.2.7".parse().unwrap()),
        }
    }

    /// An entry about `target` with no person, teams or details.
    fn entry(action: Action, target: Target<'_>) -> Entry<'_> {
        Entry {
            action,
            target,
            person: None,
            teams: &[],
            details: serde_json::json!({}),
        }
    }

    /// Stores a legacy-style admin key `id` (its id is also its name) of
    /// `user_id`, or of no user, with no grants, so a credential naming it
    /// has a row to resolve.
    async fn store_key(pool: &PgPool, id: &str, user_id: Option<&str>) {
        sqlx::query(
            "INSERT INTO api_keys \
                 (id, name, sha256, access, all_catalogs, actor, created_at, user_id) \
             VALUES ($1, $1, sha256($1::bytea), 'admin', true, 'root', now(), $2)",
        )
        .bind(id)
        .bind(user_id)
        .execute(pool)
        .await
        .unwrap();
    }

    /// Stores an unredeemed sign-in code `id` of `user_id`.
    async fn store_code(pool: &PgPool, id: &str, user_id: &str) {
        sqlx::query(
            "INSERT INTO sign_in_codes (id, name, sha256, access, all_catalogs, actor, \
                 created_at, expires_at, user_id) \
             VALUES ($1, $1, sha256($1::bytea), 'read', true, 'root', now(), \
                     now() + interval '1 day', $2)",
        )
        .bind(id)
        .bind(user_id)
        .execute(pool)
        .await
        .unwrap();
    }

    /// The `(credential_id, label, person_id)` of every `audit_credentials`
    /// row, in id order.
    /// Takes a connection, so it also reads inside an open transaction.
    async fn audit_credentials(
        conn: &mut PgConnection,
    ) -> Vec<(Option<String>, Option<String>, Option<i64>)> {
        sqlx::query_as("SELECT credential_id, label, person_id FROM audit_credentials ORDER BY id")
            .fetch_all(conn)
            .await
            .unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_fixed_person_is_refused_for_a_target_that_names_its_own() {
        let db = test_db().await;
        let user = user_id(&db.pool, TEST_USER).await;
        let fixed = FixedPerson {
            person_id: 1,
            operator: true,
        };
        let mut conn = db.pool.acquire().await.unwrap();

        let mut refusals = Vec::new();
        for target in [
            Target::User(&user),
            Target::Key("k1"),
            Target::Grant("g1"),
            Target::Code("c1"),
        ] {
            let about = entry(Action::UserCreated, target);
            refusals
                .push(record_for_person(&mut conn, &AuditActor::cli(), &about, fixed, now()).await);
        }

        for refused in refusals {
            assert!(
                matches!(refused, Err(OkfError::Refused { .. })),
                "{refused:?}"
            );
        }
        let written: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_entries WHERE at = $1")
            .bind(now())
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(written, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn record_writes_one_entry_with_its_actor_target_and_teams() {
        let db = test_db().await;
        let me = user_id(&db.pool, TEST_USER).await;
        store_key(&db.pool, "k1", Some(&me)).await;
        let mut tx = db.pool.begin().await.unwrap();
        let team = team_ref(&mut tx, TEST_TEAM).await.unwrap();

        let id = record(
            &mut tx,
            &admin_actor(&me),
            &Entry {
                teams: std::slice::from_ref(&team),
                ..entry(Action::CatalogCreated, Target::Catalog("main"))
            },
            now(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        // A ten-column row: sqlx decodes a tuple of up to 16 columns.
        #[allow(clippy::type_complexity)]
        let row: (
            String,
            String,
            Option<String>,
            Option<String>,
            String,
            String,
            bool,
            Option<String>,
            String,
            Vec<i64>,
        ) = sqlx::query_as(
            "SELECT e.action, e.source, c.credential_id, c.label, e.actor_label, p.name, \
                    e.actor_operator, e.client_ip::text, e.target_name, e.teams \
             FROM audit_entries e JOIN audit_people p ON p.id = e.actor_person_id \
             JOIN audit_credentials c ON c.id = e.actor_credential_ref \
             WHERE e.id = $1",
        )
        .bind(id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert_eq!(row.0, "catalog.created");
        assert_eq!(row.1, "api");
        assert_eq!(row.2.as_deref(), Some("k1"));
        assert_eq!(
            row.3.as_deref(),
            Some("root"),
            "the key's own label, in the mutable table"
        );
        assert_eq!(row.4, "api key", "the immutable label names no one");
        assert_eq!(row.5, TEST_USER);
        assert!(row.6);
        assert_eq!(row.7.as_deref(), Some("192.0.2.7/32"));
        assert_eq!(row.8, "main");
        assert_eq!(row.9, [team]);
        // The shared reader later tasks assert with sees the same entry.
        assert_eq!(
            testing::entries(&db.pool, "catalog.created").await,
            [testing::RecordedEntry {
                source: "api".to_string(),
                actor_label: "api key".to_string(),
                actor_operator: true,
                actor_person: Some(TEST_USER.to_string()),
                target_kind: "catalog".to_string(),
                target: Some("main".to_string()),
                person: None,
                teams: vec![TEST_USER.to_string()],
                details: serde_json::json!({}),
                requests: None,
            }]
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_team_created_again_under_its_old_name_gets_a_new_audit_id() {
        let db = test_db().await;
        let family = NewTeam {
            name: "family",
            display_name: &crate::labels::DisplayName::default(),
            owner: TEST_USER,
        };
        teams::create(&db.pool, &family, &AuditActor::cli(), now())
            .await
            .unwrap();
        let mut tx = db.pool.begin().await.unwrap();
        let first = team_ref(&mut tx, "family").await.unwrap();
        tx.commit().await.unwrap();
        teams::delete(&db.pool, "family", &AuditActor::cli(), Utc::now())
            .await
            .unwrap();
        teams::create(&db.pool, &family, &AuditActor::cli(), now())
            .await
            .unwrap();

        let mut tx = db.pool.begin().await.unwrap();
        let second = team_ref(&mut tx, "family").await.unwrap();
        let again = team_ref(&mut tx, "family").await.unwrap();

        assert_ne!(
            first, second,
            "scope follows teams.id, never the reusable name"
        );
        assert_eq!(second, again, "one row per team");
        assert!(matches!(
            team_ref(&mut tx, "no\0such").await,
            Err(OkfError::UnknownName { .. })
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_session_actor_leaves_no_name_in_the_entry_once_its_user_is_forgotten() {
        let db = test_db().await;
        let new = NewUser {
            name: "alice",
            display_name: &crate::labels::DisplayName::parse("Alice Example").unwrap(),
        };
        users::create(&db.pool, &new, &AuditActor::cli(), now())
            .await
            .unwrap();
        let alice = users::find_auth(&db.pool, &user_id(&db.pool, "alice").await)
            .await
            .unwrap()
            .unwrap();
        // A real console-session principal: its `actor` is the user's name.
        let principal = Principal::for_session(&alice, Default::default());
        let requester = Requester {
            ip: None,
            source: Source::Console,
        };
        let mut tx = db.pool.begin().await.unwrap();
        record(
            &mut tx,
            &AuditActor::of(&principal, &requester),
            &entry(Action::CatalogCreated, Target::Catalog("main")),
            now(),
        )
        .await
        .unwrap();

        forget_person(&mut tx, &alice.id).await.unwrap();
        tx.commit().await.unwrap();

        // Every stored column of the entry, and every row it refers to.
        let stored: String = sqlx::query_scalar(
            "SELECT row_to_json(e)::text || coalesce(row_to_json(p)::text, '') \
                    || coalesce(row_to_json(c)::text, '') \
             FROM audit_entries e LEFT JOIN audit_people p ON p.id = e.actor_person_id \
             LEFT JOIN audit_credentials c ON c.id = e.actor_credential_ref \
             WHERE e.action = 'catalog.created'",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert!(
            !stored.contains("alice") && !stored.contains("Alice"),
            "{stored}"
        );
        assert!(stored.contains("console session"), "{stored}");
        assert!(stored.contains("deleted user #"), "{stored}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn forgetting_a_user_clears_its_credentials_and_keeps_every_entry() {
        let db = test_db().await;
        let new = NewUser {
            name: "alice",
            display_name: &crate::labels::DisplayName::parse("Alice Example").unwrap(),
        };
        users::create(&db.pool, &new, &AuditActor::cli(), now())
            .await
            .unwrap();
        let alice = user_id(&db.pool, "alice").await;
        // A legacy key's id is its name, and a code's name is the user's
        // choice: both identify alice until they are forgotten.
        store_key(&db.pool, "alice-laptop", Some(&alice)).await;
        store_code(&db.pool, "alice-phone", &alice).await;
        let actor = AuditActor {
            credential: Some("alice-laptop".to_string()),
            user_id: Some(alice.clone()),
            label: "alice-laptop".to_string(),
            ..admin_actor(&alice)
        };
        let mut tx = db.pool.begin().await.unwrap();
        // The target names no person: its owner comes from `sign_in_codes`.
        let id = record(
            &mut tx,
            &actor,
            &entry(Action::OauthCodeRevoked, Target::Code("alice-phone")),
            now(),
        )
        .await
        .unwrap();
        let person: i64 = sqlx::query_scalar("SELECT id FROM audit_people WHERE user_id = $1")
            .bind(&alice)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        assert_eq!(
            audit_credentials(&mut tx).await,
            [
                (
                    Some("alice-laptop".to_string()),
                    Some("alice-laptop".to_string()),
                    Some(person)
                ),
                (Some("alice-phone".to_string()), None, Some(person)),
            ],
            "both credentials resolved to alice's person"
        );

        // As user deletion will: forget first, then remove the user's rows.
        forget_person(&mut tx, &alice).await.unwrap();
        tx.commit().await.unwrap();
        for table in ["api_keys", "sign_in_codes"] {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "DELETE FROM {table} WHERE user_id = $1"
            )))
            .bind(&alice)
            .execute(&db.pool)
            .await
            .unwrap();
        }
        users::delete(
            &db.pool,
            "alice",
            users::DeleteOptions::default(),
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();

        assert_eq!(
            audit_credentials(&mut db.pool.acquire().await.unwrap()).await,
            [(None, None, Some(person)), (None, None, Some(person))],
            "the ids and labels are SQL NULL; the rows stay"
        );
        let stored: String = sqlx::query_scalar(
            "SELECT row_to_json(e)::text || row_to_json(a)::text || row_to_json(t)::text \
                    || row_to_json(p)::text \
             FROM audit_entries e \
             JOIN audit_credentials a ON a.id = e.actor_credential_ref \
             JOIN audit_credentials t ON t.id = e.target_credential_ref \
             JOIN audit_people p ON p.id = e.actor_person_id \
             WHERE e.id = $1",
        )
        .bind(id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert!(
            !stored.contains("alice") && !stored.contains("Alice"),
            "{stored}"
        );
        assert!(stored.contains("deleted user #"), "{stored}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn forgetting_a_user_clears_a_key_assigned_to_it_after_it_was_recorded() {
        let db = test_db().await;
        let new = NewUser {
            name: "alice",
            display_name: &crate::labels::DisplayName::default(),
        };
        users::create(&db.pool, &new, &AuditActor::cli(), now())
            .await
            .unwrap();
        let alice = user_id(&db.pool, "alice").await;
        // A legacy key with no user yet: its audit row has no person.
        store_key(&db.pool, "alice-legacy", None).await;
        let mut tx = db.pool.begin().await.unwrap();
        record(
            &mut tx,
            &AuditActor::cli(),
            &entry(Action::KeyRevoked, Target::Key("alice-legacy")),
            now(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(
            audit_credentials(&mut db.pool.acquire().await.unwrap()).await,
            [(Some("alice-legacy".to_string()), None, None)]
        );
        sqlx::query("UPDATE api_keys SET user_id = $1 WHERE id = 'alice-legacy'")
            .bind(&alice)
            .execute(&db.pool)
            .await
            .unwrap();

        let mut tx = db.pool.begin().await.unwrap();
        forget_person(&mut tx, &alice).await.unwrap();
        tx.commit().await.unwrap();

        assert_eq!(
            audit_credentials(&mut db.pool.acquire().await.unwrap()).await,
            [(None, None, None)],
            "found through the live key, though the audit row names no person"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_key_that_acted_before_it_was_assigned_leaves_no_name_once_its_user_is_forgotten() {
        let db = test_db().await;
        let new = NewUser {
            name: "alice",
            display_name: &crate::labels::DisplayName::parse("Alice Example").unwrap(),
        };
        users::create(&db.pool, &new, &AuditActor::cli(), now())
            .await
            .unwrap();
        let alice = user_id(&db.pool, "alice").await;
        // A legacy key with no user acts: its label names a person.
        store_key(&db.pool, "alice-laptop", None).await;
        let unassigned = AuditActor {
            credential: Some("alice-laptop".to_string()),
            user_id: None,
            label: "alice-laptop".to_string(),
            ..admin_actor(&alice)
        };
        let mut tx = db.pool.begin().await.unwrap();
        record(
            &mut tx,
            &unassigned,
            &entry(Action::SettingChanged, Target::Setting("product_name")),
            now(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        // Then the key is assigned to alice, and alice is forgotten.
        sqlx::query("UPDATE api_keys SET user_id = $1 WHERE id = 'alice-laptop'")
            .bind(&alice)
            .execute(&db.pool)
            .await
            .unwrap();
        let mut tx = db.pool.begin().await.unwrap();
        forget_person(&mut tx, &alice).await.unwrap();
        tx.commit().await.unwrap();

        // Every stored column of the entry, and every row it refers to.
        let stored: String = sqlx::query_scalar(
            "SELECT row_to_json(e)::text || coalesce(row_to_json(c)::text, '') \
             FROM audit_entries e \
             LEFT JOIN audit_credentials c ON c.id = e.actor_credential_ref \
             WHERE e.action = 'setting.changed'",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert!(!stored.contains("alice"), "{stored}");
        assert!(stored.contains("\"actor_label\":\"api key\""), "{stored}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_credential_target_of_an_admin_is_marked_operator_without_a_person() {
        let db = test_db().await;
        for name in ["staff", "alice"] {
            let new = NewUser {
                name,
                display_name: &crate::labels::DisplayName::default(),
            };
            users::create(&db.pool, &new, &AuditActor::cli(), now())
                .await
                .unwrap();
        }
        sqlx::query("UPDATE users SET is_admin = true WHERE name = 'staff'")
            .execute(&db.pool)
            .await
            .unwrap();
        let staff = user_id(&db.pool, "staff").await;
        let alice = user_id(&db.pool, "alice").await;
        store_key(&db.pool, "staff-laptop", Some(&staff)).await;
        store_code(&db.pool, "staff-phone", &staff).await;
        store_key(&db.pool, "alice-laptop", Some(&alice)).await;
        let mut tx = db.pool.begin().await.unwrap();
        // `Entry.person` is `None` for each: only the credential names its
        // owner.
        for target in [
            Target::Key("staff-laptop"),
            Target::Code("staff-phone"),
            Target::Key("alice-laptop"),
        ] {
            record(
                &mut tx,
                &AuditActor::cli(),
                &entry(Action::KeyRevoked, target),
                now(),
            )
            .await
            .unwrap();
        }
        tx.commit().await.unwrap();

        let marked: Vec<(Option<String>, bool)> = sqlx::query_as(
            "SELECT c.credential_id, e.target_operator FROM audit_entries e \
             JOIN audit_credentials c ON c.id = e.target_credential_ref ORDER BY e.id",
        )
        .fetch_all(&db.pool)
        .await
        .unwrap();
        assert_eq!(
            marked,
            [
                (Some("staff-laptop".to_string()), true),
                (Some("staff-phone".to_string()), true),
                (Some("alice-laptop".to_string()), false),
            ],
            "an admin's key or code hides from owners; a member's does not"
        );
    }

    /// Creates catalog `name` owned by team `owner` (none for `None`).
    async fn store_catalog(pool: &PgPool, name: &str, owner: Option<&str>) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO catalogs (name, created_at, owner_team_id) \
             VALUES ($1, now(), (SELECT id FROM teams WHERE name = $2)) RETURNING id",
        )
        .bind(name)
        .bind(owner)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    /// Grants key `key` the catalogs `catalogs`.
    async fn grant(pool: &PgPool, key: &str, catalogs: &[i64]) {
        sqlx::query(
            "INSERT INTO key_catalogs (key_id, catalog_id) SELECT $1, unnest($2::bigint[])",
        )
        .bind(key)
        .bind(catalogs)
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn catalog_teams_is_the_owner_teams_audit_id_or_nothing() {
        let db = test_db().await;
        let owned = store_catalog(&db.pool, "owned", Some(TEST_TEAM)).await;
        let orphan = store_catalog(&db.pool, "orphan", None).await;
        let mut tx = db.pool.begin().await.unwrap();
        let team = team_ref(&mut tx, TEST_TEAM).await.unwrap();

        let of_owned = catalog_teams(&mut tx, owned).await.unwrap();
        let of_orphan = catalog_teams(&mut tx, orphan).await.unwrap();
        let of_unknown = catalog_teams(&mut tx, orphan + 1000).await.unwrap();

        assert_eq!(of_owned, [team]);
        assert_eq!(of_orphan, Vec::<i64>::new(), "no owner, no team");
        assert_eq!(of_unknown, Vec::<i64>::new(), "no catalog, no team");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn key_teams_is_the_distinct_owners_of_its_grants_and_nothing_else() {
        let db = test_db().await;
        let family = NewTeam {
            name: "family",
            display_name: &crate::labels::DisplayName::default(),
            owner: TEST_USER,
        };
        teams::create(&db.pool, &family, &AuditActor::cli(), now())
            .await
            .unwrap();
        let staff = NewTeam {
            name: "staff",
            ..family
        };
        teams::create(&db.pool, &staff, &AuditActor::cli(), now())
            .await
            .unwrap();
        let mine = store_catalog(&db.pool, "mine", Some(TEST_TEAM)).await;
        let mine_too = store_catalog(&db.pool, "mine-too", Some(TEST_TEAM)).await;
        let theirs = store_catalog(&db.pool, "theirs", Some("family")).await;
        let orphan = store_catalog(&db.pool, "orphan", None).await;
        let me = user_id(&db.pool, TEST_USER).await;
        // Two grants with one owner; grants across owners (and one with no
        // owner); an all-catalogs key with no grants. The key's user also
        // owns `staff`, which no grant reaches.
        store_key(&db.pool, "shared", Some(&me)).await;
        grant(&db.pool, "shared", &[mine, mine_too]).await;
        store_key(&db.pool, "spread", Some(&me)).await;
        grant(&db.pool, "spread", &[theirs, mine, orphan]).await;
        store_key(&db.pool, "everything", Some(&me)).await;
        let mut tx = db.pool.begin().await.unwrap();
        let personal = team_ref(&mut tx, TEST_TEAM).await.unwrap();
        let family_ref = team_ref(&mut tx, "family").await.unwrap();
        let mut both = vec![personal, family_ref];
        both.sort_unstable();

        let shared = key_teams(&mut tx, "shared").await.unwrap();
        let spread = key_teams(&mut tx, "spread").await.unwrap();
        let everything = key_teams(&mut tx, "everything").await.unwrap();

        assert_eq!(shared, [personal], "one owner, listed once");
        assert_eq!(spread, both, "each owner once, in id order; no staff");
        assert_eq!(
            everything,
            Vec::<i64>::new(),
            "an all-catalogs key belongs to no team"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_user_target_is_a_person_and_forgetting_renames_it() {
        let db = test_db().await;
        let new = NewUser {
            name: "alice",
            display_name: &crate::labels::DisplayName::parse("Alice").unwrap(),
        };
        users::create(&db.pool, &new, &AuditActor::cli(), now())
            .await
            .unwrap();
        let alice = user_id(&db.pool, "alice").await;
        let mut tx = db.pool.begin().await.unwrap();
        record(
            &mut tx,
            &AuditActor::cli(),
            &entry(Action::UserCreated, Target::User(&alice)),
            now(),
        )
        .await
        .unwrap();

        forget_person(&mut tx, &alice).await.unwrap();
        tx.commit().await.unwrap();

        let (name, display, linked, target_name): (String, String, Option<String>, Option<String>) =
            sqlx::query_as(
                "SELECT p.name, p.display_name, p.user_id, e.target_name FROM audit_entries e \
                 JOIN audit_people p ON p.id = e.target_person_id \
                 ORDER BY e.id DESC LIMIT 1",
            )
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert!(name.starts_with("deleted user #"), "{name}");
        assert_eq!(display, "");
        assert_eq!(linked, None);
        assert_eq!(
            target_name, None,
            "a user target is stored only as a person"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_log_refuses_update_and_early_delete_and_allows_an_old_delete() {
        let db = test_db().await;
        let mut tx = db.pool.begin().await.unwrap();
        let setting = Target::Setting("product_name");
        let recent = record(
            &mut tx,
            &AuditActor::cli(),
            &entry(Action::SettingChanged, setting),
            Utc::now(),
        )
        .await
        .unwrap();
        let old_at = Utc::now() - TimeDelta::days(401);
        let old = record(
            &mut tx,
            &AuditActor::cli(),
            &entry(Action::SettingCleared, setting),
            old_at,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        let updated = sqlx::query("UPDATE audit_entries SET action = 'x.y' WHERE id = $1")
            .bind(recent)
            .execute(&db.pool)
            .await;
        let early = sqlx::query("DELETE FROM audit_entries WHERE id = $1")
            .bind(recent)
            .execute(&db.pool)
            .await;
        let late = sqlx::query("DELETE FROM audit_entries WHERE id = $1")
            .bind(old)
            .execute(&db.pool)
            .await
            .unwrap();
        let truncated = sqlx::query("TRUNCATE audit_entries CASCADE")
            .execute(&db.pool)
            .await;

        assert!(updated.is_err(), "UPDATE is refused");
        assert!(early.is_err(), "a recent entry cannot be deleted");
        assert_eq!(late.rows_affected(), 1, "an entry past 400 days can");
        assert!(truncated.is_err(), "TRUNCATE is refused");
        // `setting.%`: the fixture user's `user.created` entry is there too.
        let left: i64 =
            sqlx::query_scalar("SELECT count(*) FROM audit_entries WHERE action LIKE 'setting.%'")
                .fetch_one(&db.pool)
                .await
                .unwrap();
        assert_eq!(left, 1, "the recent entry survived every attempt");
    }

    /// Records one `setting.changed` entry dated `at`, in its own
    /// transaction.
    async fn entry_at(pool: &PgPool, at: DateTime<Utc>) -> i64 {
        let mut tx = pool.begin().await.unwrap();
        let id = record(
            &mut tx,
            &AuditActor::cli(),
            &entry(Action::SettingChanged, Target::Setting("product_name")),
            at,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        id
    }

    /// Tries to delete entry `id` as the prune would: whether the
    /// append-only trigger let it go.
    async fn deletes(pool: &PgPool, id: i64) -> bool {
        sqlx::query("DELETE FROM audit_entries WHERE id = $1")
            .bind(id)
            .execute(pool)
            .await
            .is_ok()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_configured_retention_moves_what_the_log_lets_go() {
        let db = test_db().await;
        crate::store::settings::set(
            &db.pool,
            crate::store::settings::SettingKey::AuditRetentionDays,
            "60",
            &AuditActor::cli(),
            Utc::now(),
        )
        .await
        .unwrap();
        let young = entry_at(&db.pool, Utc::now() - TimeDelta::days(59)).await;
        let old = entry_at(&db.pool, Utc::now() - TimeDelta::days(61)).await;

        assert!(!deletes(&db.pool, young).await, "59 days is inside 60");
        assert!(deletes(&db.pool, old).await, "61 days is past 60");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_log_keeps_30_days_whatever_the_stored_retention_says() {
        let db = test_db().await;
        // Below what the setting accepts: only a hand edit stores this.
        sqlx::query(
            "INSERT INTO server_settings (key, value, updated_at, updated_by) \
             VALUES ('audit_retention_days', '10', now(), 'hand')",
        )
        .execute(&db.pool)
        .await
        .unwrap();
        let young = entry_at(&db.pool, Utc::now() - TimeDelta::days(20)).await;
        let old = entry_at(&db.pool, Utc::now() - TimeDelta::days(31)).await;

        assert!(!deletes(&db.pool, young).await, "the 30-day floor holds");
        assert!(deletes(&db.pool, old).await, "31 days is past the floor");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn prune_deletes_entries_past_retention_and_keeps_the_rest() {
        let db = test_db().await;
        let mut tx = db.pool.begin().await.unwrap();
        let setting = |action| entry(action, Target::Setting("product_name"));
        record(
            &mut tx,
            &AuditActor::cli(),
            &setting(Action::SettingChanged),
            Utc::now() - TimeDelta::days(500),
        )
        .await
        .unwrap();
        record(
            &mut tx,
            &AuditActor::cli(),
            &setting(Action::SettingCleared),
            Utc::now(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        let pruned = prune(
            &db.pool,
            Utc::now() - TimeDelta::days(400) - TimeDelta::hours(1),
        )
        .await
        .unwrap();

        assert_eq!(pruned, 1);
        assert!(testing::entries(&db.pool, "setting.changed")
            .await
            .is_empty());
        assert_eq!(testing::entries(&db.pool, "setting.cleared").await.len(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_prune_reaching_into_retention_deletes_nothing() {
        let db = test_db().await;
        entry_at(&db.pool, Utc::now() - TimeDelta::days(500)).await;
        entry_at(&db.pool, Utc::now() - TimeDelta::days(10)).await;

        let refused = prune(&db.pool, Utc::now()).await;

        assert!(refused.is_err(), "the trigger refuses the whole statement");
        assert_eq!(testing::entries(&db.pool, "setting.changed").await.len(), 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_entry_rolls_back_with_the_change_it_records() {
        let db = test_db().await;
        let mut tx = db.pool.begin().await.unwrap();
        record(
            &mut tx,
            &AuditActor::cli(),
            &Entry {
                details: serde_json::json!({"to": "x"}),
                ..entry(Action::SettingChanged, Target::Setting("product_name"))
            },
            now(),
        )
        .await
        .unwrap();
        // The change fails: dropping the transaction rolls both back.
        drop(tx);

        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM audit_entries WHERE action LIKE 'setting.%'")
                .fetch_one(&db.pool)
                .await
                .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn only_a_provisioner_is_refused_an_admin_target() {
        let mut actor = AuditActor::cli();
        assert!(refuse_admin_target(&actor, true, "root").is_ok());
        actor.provisioner = true;
        assert!(refuse_admin_target(&actor, false, "alice").is_ok());
        assert!(matches!(
            refuse_admin_target(&actor, true, "root"),
            Err(OkfError::Forbidden { .. })
        ));
    }

    #[test]
    fn every_action_name_is_unique_and_well_formed() {
        let names: std::collections::HashSet<&str> =
            Action::ALL.iter().map(|action| action.as_str()).collect();
        assert_eq!(names.len(), Action::ALL.len());
        // The same pattern as the table's CHECK on `action`.
        let pattern = regex::Regex::new(r"^[a-z]+\.[a-z_]+$").unwrap();
        assert!(names.iter().all(|name| pattern.is_match(name)), "{names:?}");
        assert!(Action::ALL
            .iter()
            .all(|action| Action::parse(action.as_str()) == Some(*action)));
        assert_eq!(Action::parse("user.unknown"), None);
    }

    /// Records `team.suspended` on `team` (a personal team) as `actor`, the
    /// team resolved first through [`team_ref`], as a team write does.
    async fn record_on_team(pool: PgPool, team: String, actor: AuditActor) -> Result<(), OkfError> {
        let mut tx = pool.begin().await?;
        let team_id = team_ref(&mut tx, &team).await?;
        record(
            &mut tx,
            &actor,
            &Entry {
                teams: &[team_id],
                ..entry(Action::TeamSuspended, Target::Team(&team))
            },
            now(),
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn admins_acting_on_each_others_personal_teams_at_once_both_finish() {
        use crate::store::users::tests::{both_finish_while_a_person_is_held, two_admins};
        let db = test_db().await;
        let [(low_name, low), (high_name, high)] = two_admins(&db.pool).await;
        let held = low.user_id.clone().expect("a session actor has a user");
        // Each resolves the other's personal team (its person) first, then
        // records with itself as the actor.
        let first = record_on_team(db.pool.clone(), high_name, low);
        let second = record_on_team(db.pool.clone(), low_name, high);

        let (one, other) = both_finish_while_a_person_is_held(&db.pool, &held, first, second).await;

        one.expect("the first entry");
        other.expect("the second entry");
        let recorded = testing::entries(&db.pool, "team.suspended").await;
        assert_eq!(recorded.len(), 2);
        // The stale snapshots were refreshed by whichever entry could.
        let names: Vec<String> = sqlx::query_scalar(
            "SELECT display_name FROM audit_people WHERE name IN ('ann', 'ben')",
        )
        .fetch_all(&db.pool)
        .await
        .unwrap();
        assert!(names.iter().any(|name| name == "renamed"), "{names:?}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn admin_reads_are_one_entry_per_credential_catalog_and_hour_with_an_exact_count() {
        let db = test_db().await;
        crate::store::catalogs::create(
            &db.pool,
            &crate::store::catalogs::NewCatalog {
                name: "main",
                display_name: &crate::labels::DisplayName::default(),
                description: "",
                owner_team: TEST_TEAM,
            },
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();
        let actor = admin_actor(&user_id(&db.pool, TEST_USER).await);

        // Ten at once in one hour, then one in the next. `JoinSet` runs the
        // ten as concurrent tasks (the crate has no `futures` dependency).
        let mut reads = tokio::task::JoinSet::new();
        for _ in 0..10 {
            let (pool, actor) = (db.pool.clone(), actor.clone());
            reads.spawn(async move {
                let record = testing::access_record(&pool, &actor, "main", now()).await;
                record_admin_read(&pool, &record).await
            });
        }
        let joined = tokio::time::timeout(std::time::Duration::from_secs(30), async {
            while let Some(read) = reads.join_next().await {
                read.unwrap().unwrap();
            }
        });
        joined.await.expect("ten concurrent admin reads finish");
        let next_hour = now() + TimeDelta::hours(1);
        let record = testing::access_record(&db.pool, &actor, "main", next_hour).await;
        record_admin_read(&db.pool, &record).await.unwrap();

        let counts: Vec<i64> = sqlx::query_scalar(
            "SELECT c.requests FROM audit_entries e \
             JOIN audit_access_counts c ON c.entry_id = e.id \
             WHERE e.action = 'catalog.admin_read' ORDER BY e.id",
        )
        .fetch_all(&db.pool)
        .await
        .unwrap();
        assert_eq!(counts, [10, 1]);
        let entries = testing::entries(&db.pool, "catalog.admin_read").await;
        assert_eq!(entries[0].target.as_deref(), Some("main"));
        assert_eq!(entries[0].actor_label, "api key");
        assert_eq!(entries[0].details["hour"], "2026-11-13T12:00:00Z");
    }

    /// A provisioner key as admission fixes it, with no audit rows (the
    /// bucket then names the label), at `at`.
    fn provisioner_actor(at: DateTime<Utc>) -> AdmittedActor {
        AdmittedActor {
            source: Source::Api,
            credential: None,
            person: None,
            label: "api key".to_string(),
            operator: true,
            ip: Some("192.0.2.9".parse().unwrap()),
            at,
        }
    }

    fn refusal(route: &str, at: DateTime<Utc>) -> ProvisionerUse {
        ProvisionerUse {
            actor: provisioner_actor(at),
            event: ProvisionerEvent::Refused {
                method: "GET",
                route: route.to_string(),
                reason: RefusalReason::RouteNotAllowed,
            },
        }
    }

    /// `(target, requests)` of every entry with this action, oldest first.
    async fn counted(pool: &PgPool, action: &str) -> Vec<(Option<String>, i64)> {
        testing::entries(pool, action)
            .await
            .into_iter()
            .map(|entry| (entry.target, entry.requests.expect("a counted entry")))
            .collect()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn identical_provisioner_refusals_collapse_into_one_counted_entry_per_hour() {
        let db = test_db().await;
        // Twenty at once, then the next hour, then another reason.
        let mut writes = tokio::task::JoinSet::new();
        for _ in 0..20 {
            let (pool, usage) = (db.pool.clone(), refusal("/api/v1/keys", now()));
            writes.spawn(async move { record_provisioner_use(&pool, &usage).await });
        }
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            while let Some(write) = writes.join_next().await {
                write.unwrap().unwrap();
            }
        })
        .await
        .expect("twenty concurrent refusals finish");
        let later = now() + TimeDelta::hours(1);
        record_provisioner_use(&db.pool, &refusal("/api/v1/keys", later))
            .await
            .unwrap();
        let mut forbidden = refusal("/api/v1/users", now());
        forbidden.event = ProvisionerEvent::Refused {
            method: "GET",
            route: "/api/v1/users".to_string(),
            reason: RefusalReason::Forbidden,
        };
        record_provisioner_use(&db.pool, &forbidden).await.unwrap();

        assert_eq!(
            counted(&db.pool, "provisioner.refused").await,
            [
                (Some("GET /api/v1/keys".to_string()), 20),
                (Some("GET /api/v1/keys".to_string()), 1),
                (Some("GET /api/v1/users".to_string()), 1),
            ]
        );
        let entries = testing::entries(&db.pool, "provisioner.refused").await;
        assert_eq!(entries[0].target_kind, "route");
        assert!(entries[0].actor_operator);
        assert!(entries[0].teams.is_empty(), "owners never see it");
        assert_eq!(
            entries[0].details,
            serde_json::json!({
                "method": "GET",
                "route": "/api/v1/keys",
                "reason": "route_not_allowed",
                "hour": "2026-11-13T12:00:00Z",
            })
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_key_opens_a_bounded_number_of_entries_an_hour_and_the_rest_count_in_one() {
        let db = test_db().await;
        let extra = 7;
        for number in 0..PROVISIONER_ENTRIES_PER_HOUR + extra {
            let usage = refusal(&format!("/api/v1/route-{number}"), now());
            record_provisioner_use(&db.pool, &usage).await.unwrap();
        }
        // A route already opened keeps counting in its own entry.
        record_provisioner_use(&db.pool, &refusal("/api/v1/route-0", now()))
            .await
            .unwrap();
        // Another kind of event has its own overflow.
        for number in 0..3 {
            let usage = ProvisionerUse {
                actor: provisioner_actor(now()),
                event: ProvisionerEvent::Lookup {
                    matched: Some(format!("no-such-user-{number}")),
                },
            };
            record_provisioner_use(&db.pool, &usage).await.unwrap();
        }

        let refused = counted(&db.pool, "provisioner.refused").await;
        let budget = usize::try_from(PROVISIONER_ENTRIES_PER_HOUR).unwrap();
        // The budget's worth of routes, then one overflow entry.
        assert_eq!(refused.len(), budget + 1);
        assert_eq!(refused[0], (Some("GET /api/v1/route-0".to_string()), 2));
        assert_eq!(refused[budget], (Some("*".to_string()), extra));
        let overflow = testing::entries(&db.pool, "provisioner.refused").await;
        assert_eq!(overflow[budget].details["overflow"], true);
        // The three lookups share the key's hourly budget, so they overflow
        // too, into an entry of their own action.
        let lookups = counted(&db.pool, "user.looked_up").await;
        assert_eq!(lookups, [(Some("*".to_string()), 3)]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn two_first_requests_for_the_last_slots_kind_share_one_entry() {
        use crate::http::api::testing::wait_for_lock_waiters;
        let db = test_db().await;
        let last = PROVISIONER_ENTRIES_PER_HOUR - 1;
        for number in 0..last {
            let usage = refusal(&format!("/api/v1/route-{number}"), now());
            record_provisioner_use(&db.pool, &usage).await.unwrap();
        }
        // Hold the key's budget row, so both requests pass the first look
        // for their bucket (absent) and queue on the budget.
        let mut holder = db.pool.begin().await.unwrap();
        let cap = format!("provcap\u{1f}lapi key\u{1f}{}", now().timestamp());
        sqlx::query("SELECT 1 FROM audit_access_counts WHERE bucket = $1 FOR UPDATE")
            .bind(&cap)
            .fetch_one(&mut *holder)
            .await
            .unwrap();
        let mut writes = tokio::task::JoinSet::new();
        for _ in 0..2 {
            let (pool, usage) = (db.pool.clone(), refusal("/api/v1/the-last", now()));
            writes.spawn(async move { record_provisioner_use(&pool, &usage).await });
        }
        wait_for_lock_waiters(&db.pool, 2).await;
        holder.commit().await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            while let Some(write) = writes.join_next().await {
                write.unwrap().unwrap();
            }
        })
        .await
        .expect("both requests finish once the budget row is released");

        let refused = counted(&db.pool, "provisioner.refused").await;
        let budget = usize::try_from(PROVISIONER_ENTRIES_PER_HOUR).unwrap();
        assert_eq!(refused.len(), budget, "no overflow entry");
        assert_eq!(
            refused[budget - 1],
            (Some("GET /api/v1/the-last".to_string()), 2),
            "both counted in the regular entry"
        );
        assert!(refused
            .iter()
            .all(|(target, _)| target.as_deref() != Some("*")));
        // The loser gave its slot back: one more kind still gets an entry.
        record_provisioner_use(&db.pool, &refusal("/api/v1/one-more", now()))
            .await
            .unwrap();
        let after = counted(&db.pool, "provisioner.refused").await;
        assert_eq!(
            after.last(),
            Some(&(Some("*".to_string()), 1)),
            "the 51st kind overflows"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_lookup_names_the_matched_user_and_stores_nothing_else_about_it() {
        let db = test_db().await;
        let matched = user_id(&db.pool, TEST_USER).await;
        let found = ProvisionerUse {
            actor: provisioner_actor(now()),
            event: ProvisionerEvent::Lookup {
                matched: Some(matched),
            },
        };
        let missing = ProvisionerUse {
            actor: provisioner_actor(now()),
            event: ProvisionerEvent::Lookup { matched: None },
        };
        record_provisioner_use(&db.pool, &found).await.unwrap();
        record_provisioner_use(&db.pool, &found).await.unwrap();
        record_provisioner_use(&db.pool, &missing).await.unwrap();

        let entries = testing::entries(&db.pool, "user.looked_up").await;
        assert_eq!(entries.len(), 2);
        assert_eq!(
            (
                entries[0].target_kind.as_str(),
                entries[0].person.as_deref(),
                entries[0].requests
            ),
            ("user", Some(TEST_USER), Some(2))
        );
        assert_eq!(entries[0].details["outcome"], "found");
        assert_eq!(
            (
                entries[1].target_kind.as_str(),
                entries[1].target.as_deref(),
                entries[1].requests
            ),
            ("route", Some("GET /api/v1/users"), Some(1))
        );
        assert_eq!(entries[1].details["outcome"], "not_found");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn budget_counters_go_once_their_hour_ended_over_the_grace_before() {
        let db = test_db().await;
        let at = |time: &str| -> DateTime<Utc> { time.parse().unwrap() };
        let clock = at("2026-11-13T12:30:00Z");
        // Hour 10 ended at 11:00, over the hour's grace before 12:30; hour 11
        // ended at 12:00, within it; hour 12 is the current one.
        for time in [
            "2026-11-13T10:59:00Z",
            "2026-11-13T11:10:00Z",
            "2026-11-13T12:05:00Z",
        ] {
            record_provisioner_use(&db.pool, &refusal("/api/v1/keys", at(time)))
                .await
                .unwrap();
        }
        let counters = |pool: PgPool| async move {
            sqlx::query_scalar::<_, String>(
                "SELECT split_part(bucket, chr(31), 3) FROM audit_access_counts \
                 WHERE entry_id IS NULL ORDER BY 1",
            )
            .fetch_all(&pool)
            .await
            .unwrap()
        };
        assert_eq!(counters(db.pool.clone()).await.len(), 3, "one per hour");

        let removed = prune_budget_counters(&db.pool, clock).await.unwrap();

        assert_eq!(removed, 1);
        let kept = [at("2026-11-13T11:00:00Z"), at("2026-11-13T12:00:00Z")]
            .map(|hour| hour.timestamp().to_string());
        assert_eq!(counters(db.pool.clone()).await, kept);
        assert_eq!(
            counted(&db.pool, "provisioner.refused").await.len(),
            3,
            "the entries stay for the retention period"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_retention_prune_leaves_budget_counters_alone() {
        let db = test_db().await;
        let old: DateTime<Utc> = "2024-01-01T12:00:00Z".parse().unwrap();
        record_provisioner_use(&db.pool, &refusal("/api/v1/keys", old))
            .await
            .unwrap();

        let removed = prune(&db.pool, old + TimeDelta::days(1)).await.unwrap();

        assert_eq!(removed, 1);
        let counters: i64 =
            sqlx::query_scalar("SELECT count(*) FROM audit_access_counts WHERE entry_id IS NULL")
                .fetch_one(&db.pool)
                .await
                .unwrap();
        assert_eq!(counters, 1, "prune_budget_counters owns them");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_catalog_moved_or_created_again_within_the_hour_starts_a_new_admin_read_entry() {
        use crate::store::catalogs::{self, NewCatalog};
        let db = test_db().await;
        let actor = admin_actor(&user_id(&db.pool, TEST_USER).await);
        let cli = AuditActor::cli();
        teams::create(
            &db.pool,
            &NewTeam {
                name: "family",
                display_name: &crate::labels::DisplayName::default(),
                owner: TEST_USER,
            },
            &cli,
            now(),
        )
        .await
        .unwrap();
        // A local the closure borrows: a temporary made inside it would be
        // dropped when it returns, leaving the struct's reference dangling.
        let unset = crate::labels::DisplayName::default();
        let main = |owner_team| NewCatalog {
            name: "main",
            display_name: &unset,
            description: "",
            owner_team,
        };
        // One admin read at `minutes` past the hour.
        let read = |minutes: i64| {
            let (pool, actor) = (db.pool.clone(), actor.clone());
            async move {
                let at = now() + TimeDelta::minutes(minutes);
                let record = testing::access_record(&pool, &actor, "main", at).await;
                record_admin_read(&pool, &record).await.unwrap();
            }
        };
        catalogs::create(&db.pool, &main(TEST_TEAM), &cli, now())
            .await
            .unwrap();

        read(0).await;
        catalogs::set_owner(&db.pool, "main", "family", &cli, now())
            .await
            .unwrap();
        read(1).await;
        read(2).await;
        catalogs::delete(&db.pool, "main", true, &cli, now())
            .await
            .unwrap();
        catalogs::create(&db.pool, &main(TEST_TEAM), &cli, now())
            .await
            .unwrap();
        read(3).await;

        let reads: Vec<(Vec<i64>, i64)> = sqlx::query_as(
            "SELECT e.teams, c.requests FROM audit_entries e \
             JOIN audit_access_counts c ON c.entry_id = e.id \
             WHERE e.action = 'catalog.admin_read' ORDER BY e.id",
        )
        .fetch_all(&db.pool)
        .await
        .unwrap();
        let mut conn = db.pool.acquire().await.unwrap();
        let personal = team_ref(&mut conn, TEST_TEAM).await.unwrap();
        let family = team_ref(&mut conn, "family").await.unwrap();
        // Same hour throughout: one entry per (catalog identity, owner team).
        assert_eq!(
            reads,
            [(vec![personal], 1), (vec![family], 2), (vec![personal], 1)]
        );
    }

    /// An owner of `team` (an existing team owned by someone), as
    /// [`list`] takes it.
    async fn owner_of(pool: &PgPool, team: &str) -> Viewer {
        let mut conn = pool.acquire().await.unwrap();
        let audit_team = team_ref(&mut conn, team).await.unwrap();
        let team_id: String = sqlx::query_scalar("SELECT id FROM teams WHERE name = $1")
            .bind(team)
            .fetch_one(pool)
            .await
            .unwrap();
        Viewer::Owner {
            team_id,
            audit_team,
        }
    }

    /// Records one entry in its own transaction.
    async fn record_one(pool: &PgPool, actor: &AuditActor, entry: &Entry<'_>) {
        let mut tx = pool.begin().await.unwrap();
        record(&mut tx, actor, entry, now()).await.unwrap();
        tx.commit().await.unwrap();
    }

    /// A query with only an actor or a target filter.
    fn by(actor: Option<&'static str>, target: Option<&'static str>) -> AuditQuery<'static> {
        AuditQuery {
            actor,
            target,
            ..AuditQuery::default()
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn owners_never_see_an_operators_identity_ip_unlock_reason_or_unlocked_staff() {
        let db = test_db().await;
        let me = user_id(&db.pool, TEST_USER).await;
        users::create(
            &db.pool,
            &NewUser {
                name: "staff",
                display_name: &crate::labels::DisplayName::parse("Staff Member").unwrap(),
            },
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();
        let promote = users::AdminChange {
            on: true,
            allow_last: true,
        };
        users::set_admin(&db.pool, "staff", promote, &AuditActor::cli(), now())
            .await
            .unwrap();
        let staff = user_id(&db.pool, "staff").await;
        store_key(&db.pool, "staff-key", Some(&staff)).await;
        store_code(&db.pool, "staff-code", &staff).await;
        let mut conn = db.pool.acquire().await.unwrap();
        let team = team_ref(&mut conn, TEST_TEAM).await.unwrap();
        drop(conn);
        let actor = AuditActor {
            credential: Some("staff-key".to_string()),
            user_id: Some(staff.clone()),
            ..admin_actor(&me)
        };
        let unlocked = Entry {
            action: Action::CatalogUnlocked,
            target: Target::Catalog("main"),
            // The unlocked admin: operator staff.
            person: Some(&staff),
            teams: std::slice::from_ref(&team),
            details: serde_json::json!({"category": "support", "reason": "ticket 12", "unlock": 1}),
        };
        // The staff member's credentials as targets (a key, a grant, as
        // `keys::key_target` names an OAuth grant, and a code): each is
        // hidden with its id.
        let targets = [
            Target::Key("staff-key"),
            Target::Grant("staff-key"),
            Target::Code("staff-code"),
        ];
        for target in targets {
            let granted = Entry {
                teams: std::slice::from_ref(&team),
                ..entry(Action::KeyCatalogGranted, target)
            };
            record_one(&db.pool, &actor, &granted).await;
        }
        record_one(&db.pool, &actor, &unlocked).await;
        let owner_view = owner_of(&db.pool, TEST_TEAM).await;

        let owner = list_plain(&db.pool, &owner_view, &AuditQuery::default())
            .await
            .unwrap();
        // Owners cannot use the filters as an identity oracle.
        let probes = [
            by(None, Some("staff")),
            by(None, Some("staff-key")),
            by(Some("staff-key"), None),
            by(Some("staff"), None),
            by(Some("root"), None),
            by(Some("api key"), None),
        ];
        let mut owner_hits = Vec::new();
        let mut admin_hits = Vec::new();
        for probe in &probes {
            owner_hits.push(
                list_plain(&db.pool, &owner_view, probe)
                    .await
                    .unwrap()
                    .len(),
            );
            admin_hits.push(
                list_plain(&db.pool, &Viewer::Admin, probe)
                    .await
                    .unwrap()
                    .len(),
            );
        }
        // The fixture user's own entry is made at the real time, after every
        // entry the test records at `now()`: the window leaves it out, so
        // the newest entry is the last one recorded.
        let admin = list_plain(
            &db.pool,
            &Viewer::Admin,
            &AuditQuery {
                until: Some(now() + TimeDelta::seconds(1)),
                ..AuditQuery::default()
            },
        )
        .await
        .unwrap();

        assert_eq!(owner.len(), 4, "every entry is the team's");
        assert_eq!(owner[0].actor, ActorView::Operator);
        assert_eq!(owner[0].ip, None);
        assert_eq!(
            owner[0].target.person, None,
            "the unlocked staff member is hidden"
        );
        assert_eq!(
            owner[0].details,
            serde_json::json!({"category": "support", "unlock": 1})
        );
        let hidden: Vec<TargetView> = owner[1..]
            .iter()
            .map(|record| record.target.clone())
            .collect();
        let hide = |kind: &str| TargetView {
            kind: kind.to_string(),
            name: None,
            person: None,
            person_label: None,
            target_label: None,
        };
        assert_eq!(
            hidden,
            [hide("code"), hide("grant"), hide("key")],
            "staff credentials"
        );
        // The whole record, as the API would send it.
        let whole = serde_json::to_string(&owner).unwrap();
        assert!(
            !whole.contains("staff")
                && !whole.contains("Staff")
                && !whole.contains("ticket")
                && !whole.contains("192.0.2"),
            "{whole}"
        );
        assert_eq!(
            owner_hits, [0; 6],
            "no filter matches an operator for an owner"
        );
        // The same filters do match for an admin: the target person matches
        // the unlock and staff's own `user.created` and `user.admin_changed`;
        // the credential id the key and the grant; each actor filter all
        // four of staff's entries.
        assert_eq!(admin_hits, [3, 2, 4, 4, 4, 4]);
        assert_eq!(admin[0].details["reason"], "ticket 12");
        assert_eq!(admin[0].target.person.as_deref(), Some("staff"));
        assert_eq!(admin[0].ip.as_deref(), Some("192.0.2.7"));
        assert_eq!(
            admin[0].teams,
            [TEST_USER],
            "a personal team reads as its user"
        );
        assert_eq!(admin[1].target.name.as_deref(), Some("staff-code"));
        assert_eq!(admin[3].target.name.as_deref(), Some("staff-key"));
        let ActorView::Known {
            person, credential, ..
        } = &admin[0].actor
        else {
            panic!("an admin sees the operator: {:?}", admin[0].actor);
        };
        assert_eq!(
            (person.as_deref(), credential.as_deref()),
            (Some("staff"), Some("staff-key"))
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_admin_user_acting_through_a_non_admin_credential_is_an_operator_to_owners() {
        let db = test_db().await;
        users::create(
            &db.pool,
            &NewUser {
                name: "staff",
                display_name: &crate::labels::DisplayName::default(),
            },
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();
        let promote = users::AdminChange {
            on: true,
            allow_last: true,
        };
        users::set_admin(&db.pool, "staff", promote, &AuditActor::cli(), now())
            .await
            .unwrap();
        let staff = user_id(&db.pool, "staff").await;
        store_key(&db.pool, "staff-own", Some(&staff)).await;
        let mut conn = db.pool.acquire().await.unwrap();
        let team = team_ref(&mut conn, TEST_TEAM).await.unwrap();
        drop(conn);
        // A self-issued write key of the admin user: not an operator
        // credential by its access.
        let actor = AuditActor {
            credential: Some("staff-own".to_string()),
            user_id: Some(staff.clone()),
            label: "laptop".to_string(),
            operator: false,
            ..admin_actor(&staff)
        };
        let published = Entry {
            teams: std::slice::from_ref(&team),
            ..entry(Action::CatalogPublished, Target::Catalog("main"))
        };
        record_one(&db.pool, &actor, &published).await;
        let owner_view = owner_of(&db.pool, TEST_TEAM).await;

        let owner = list_plain(&db.pool, &owner_view, &AuditQuery::default())
            .await
            .unwrap();
        let probe = list_plain(&db.pool, &owner_view, &by(Some("staff"), None))
            .await
            .unwrap();

        assert_eq!(owner[0].actor, ActorView::Operator);
        assert!(probe.is_empty(), "the actor filter is no staff oracle");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn owners_see_only_their_members_ips_and_only_their_own_team() {
        let db = test_db().await;
        users::create(
            &db.pool,
            &NewUser {
                name: "bob",
                display_name: &crate::labels::DisplayName::default(),
            },
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();
        let me = user_id(&db.pool, TEST_USER).await;
        let bob = user_id(&db.pool, "bob").await;
        let mut conn = db.pool.acquire().await.unwrap();
        let mine = team_ref(&mut conn, TEST_TEAM).await.unwrap();
        let bobs = team_ref(&mut conn, "bob").await.unwrap();
        drop(conn);
        let session = |user_id: &str, ip: &str| AuditActor {
            source: Source::Console,
            credential: None,
            user_id: Some(user_id.to_string()),
            label: "someone".to_string(),
            via: Via::Session,
            operator: false,
            provisioner: false,
            ip: Some(ip.parse().unwrap()),
        };
        // A catalog moved from bob's personal team to TEST_USER's.
        let both = [bobs, mine];
        let moved = Entry {
            teams: &both,
            ..entry(Action::CatalogOwnerChanged, Target::Catalog("main"))
        };
        record_one(&db.pool, &session(&bob, "192.0.2.21"), &moved).await;
        record_one(&db.pool, &session(&me, "192.0.2.22"), &moved).await;
        // An operator who is also the team's member: still no IP.
        let mut operator = session(&me, "192.0.2.23");
        operator.operator = true;
        record_one(&db.pool, &operator, &moved).await;

        let owner = list_plain(
            &db.pool,
            &owner_of(&db.pool, TEST_TEAM).await,
            &AuditQuery::default(),
        )
        .await
        .unwrap();
        // The fixture user's own entry is made at the real time, after every
        // entry the test records at `now()`: the window leaves it out, so
        // the newest entry is the last one recorded.
        let admin = list_plain(
            &db.pool,
            &Viewer::Admin,
            &AuditQuery {
                until: Some(now() + TimeDelta::seconds(1)),
                ..AuditQuery::default()
            },
        )
        .await
        .unwrap();

        assert_eq!(owner.len(), 3);
        assert_eq!(owner[0].ip, None, "an operator, though a member");
        assert_eq!(
            owner[1].ip.as_deref(),
            Some("192.0.2.22"),
            "the team's own member"
        );
        assert_eq!(owner[2].ip, None, "not a member of the team");
        assert!(
            owner.iter().all(|record| record.teams == [TEST_USER]),
            "only the owner's own team"
        );
        assert_eq!(admin[0].teams, ["bob", TEST_USER]);
        assert_eq!(admin[0].ip.as_deref(), Some("192.0.2.23"));
        assert_eq!(admin[2].ip.as_deref(), Some("192.0.2.21"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_deleted_session_user_is_named_nowhere_in_the_admin_or_owner_view() {
        let db = test_db().await;
        let family = NewTeam {
            name: "family",
            display_name: &crate::labels::DisplayName::default(),
            owner: TEST_USER,
        };
        teams::create(&db.pool, &family, &AuditActor::cli(), now())
            .await
            .unwrap();
        users::create(
            &db.pool,
            &NewUser {
                name: "alice",
                display_name: &crate::labels::DisplayName::parse("Alice Example").unwrap(),
            },
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();
        let alice = user_id(&db.pool, "alice").await;
        // A legacy-style key whose id carries her name.
        store_key(&db.pool, "alice-laptop", Some(&alice)).await;
        let mut conn = db.pool.acquire().await.unwrap();
        let family_ref = team_ref(&mut conn, "family").await.unwrap();
        drop(conn);
        // The actor `AuditActor::of` builds for alice's console session: its
        // label is her name. It acts on a credential whose id carries the
        // name too.
        let actor = AuditActor {
            source: Source::Console,
            credential: Some("alice-laptop".to_string()),
            user_id: Some(alice.clone()),
            label: "alice".to_string(),
            via: Via::Session,
            operator: false,
            provisioner: false,
            ip: Some("192.0.2.8".parse().unwrap()),
        };
        let created = Entry {
            person: Some(&alice),
            teams: std::slice::from_ref(&family_ref),
            details: serde_json::json!({"self_issued": true}),
            ..entry(Action::KeyCreated, Target::Key("alice-laptop"))
        };
        record_one(&db.pool, &actor, &created).await;
        let before = list_plain(&db.pool, &Viewer::Admin, &AuditQuery::default())
            .await
            .unwrap();

        users::delete(
            &db.pool,
            "alice",
            users::DeleteOptions::default(),
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();

        // The fixture user's own entry is made at the real time, after every
        // entry the test records at `now()`: the window leaves it out, so
        // the newest entry is the last one recorded.
        let admin = list_plain(
            &db.pool,
            &Viewer::Admin,
            &AuditQuery {
                until: Some(now() + TimeDelta::seconds(1)),
                ..AuditQuery::default()
            },
        )
        .await
        .unwrap();
        let owner = list_plain(
            &db.pool,
            &owner_of(&db.pool, "family").await,
            &AuditQuery::default(),
        )
        .await
        .unwrap();
        // Before the deletion she is named (so the check below is not vacuous).
        assert!(serde_json::to_string(&before).unwrap().contains("alice"));
        for (view, records) in [("admin", &admin), ("owner", &owner)] {
            let whole = serde_json::to_string(records).unwrap();
            assert!(
                !whole.contains("alice") && !whole.contains("Alice"),
                "{view}: {whole}"
            );
        }
        assert!(owner.iter().any(|record| record.action == "key.created"));
        let admin_text = serde_json::to_string(&admin).unwrap();
        assert!(admin_text.contains("deleted user #"), "{admin_text}");
        assert!(admin_text.contains("credential #"), "{admin_text}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_new_team_under_an_old_name_cannot_read_the_old_teams_entries() {
        let db = test_db().await;
        users::create(
            &db.pool,
            &NewUser {
                name: "bob",
                display_name: &crate::labels::DisplayName::default(),
            },
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();
        // A local the closure borrows: a temporary made inside it would be
        // dropped when it returns, leaving the struct's reference dangling.
        let unset = crate::labels::DisplayName::default();
        let new_team = |owner: &'static str| NewTeam {
            name: "family",
            display_name: &unset,
            owner,
        };
        teams::create(&db.pool, &new_team(TEST_USER), &AuditActor::cli(), now())
            .await
            .unwrap();
        teams::delete(&db.pool, "family", &AuditActor::cli(), now())
            .await
            .unwrap();
        // Another customer's team, same name.
        teams::create(&db.pool, &new_team("bob"), &AuditActor::cli(), now())
            .await
            .unwrap();

        let seen = list_plain(
            &db.pool,
            &owner_of(&db.pool, "family").await,
            &AuditQuery::default(),
        )
        .await
        .unwrap();
        let by_name = list_plain(
            &db.pool,
            &Viewer::Admin,
            &AuditQuery {
                team: Some("family"),
                ..AuditQuery::default()
            },
        )
        .await
        .unwrap();

        let actions: Vec<&str> = seen.iter().map(|record| record.action.as_str()).collect();
        assert_eq!(
            actions,
            ["team.created"],
            "only the new team's own creation, never the old team's entries"
        );
        // An admin's filter by name spans every team that had it.
        let actions: Vec<&str> = by_name
            .iter()
            .map(|record| record.action.as_str())
            .collect();
        assert_eq!(actions, ["team.created", "team.deleted", "team.created"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_credential_viewer_sees_only_its_own_entries() {
        let db = test_db().await;
        let me = user_id(&db.pool, TEST_USER).await;
        store_key(&db.pool, "k1", Some(&me)).await;
        store_key(&db.pool, "k2", Some(&me)).await;
        let other = AuditActor {
            credential: Some("k2".to_string()),
            ..admin_actor(&me)
        };
        record_one(
            &db.pool,
            &admin_actor(&me),
            &entry(Action::SettingChanged, Target::Setting("a")),
        )
        .await;
        record_one(
            &db.pool,
            &other,
            &entry(Action::SettingChanged, Target::Setting("b")),
        )
        .await;

        let seen = list_plain(
            &db.pool,
            &Viewer::Credential("k1".to_string()),
            &AuditQuery::default(),
        )
        .await
        .unwrap();

        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].target.name.as_deref(), Some("a"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_filters_window_and_page_newest_first() {
        let db = test_db().await;
        let me = user_id(&db.pool, TEST_USER).await;
        let mut tx = db.pool.begin().await.unwrap();
        for (minutes, key) in [(0, "a"), (10, "b"), (20, "c")] {
            let at = now() + TimeDelta::minutes(minutes);
            record(
                &mut tx,
                &admin_actor(&me),
                &entry(Action::SettingChanged, Target::Setting(key)),
                at,
            )
            .await
            .unwrap();
        }
        tx.commit().await.unwrap();
        let names = |records: Vec<AuditRecord>| -> Vec<String> {
            records
                .into_iter()
                .filter_map(|record| record.target.name)
                .collect()
        };
        let query = |since, until, after, limit| AuditQuery {
            action: Some(Action::SettingChanged),
            since,
            until,
            after,
            limit,
            ..AuditQuery::default()
        };

        let first = list_plain(&db.pool, &Viewer::Admin, &query(None, None, None, 2))
            .await
            .unwrap();
        let rest = list_plain(
            &db.pool,
            &Viewer::Admin,
            &query(None, None, Some(first[1].cursor.clone()), 2),
        )
        .await
        .unwrap();
        let window = query(
            Some(now() + TimeDelta::minutes(5)),
            Some(now() + TimeDelta::minutes(20)),
            None,
            0,
        );
        let windowed = list_plain(&db.pool, &Viewer::Admin, &window).await.unwrap();

        assert_eq!(names(first), ["c", "b"]);
        assert_eq!(names(rest), ["a"]);
        assert_eq!(
            names(windowed),
            ["b"],
            "since is inclusive, until exclusive"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_filter_that_names_nothing_matches_nothing_without_sql() {
        let db = test_db().await;
        let queries = [
            AuditQuery {
                team: Some("a\0b"),
                ..AuditQuery::default()
            },
            AuditQuery {
                team: Some("Not A Name"),
                ..AuditQuery::default()
            },
            AuditQuery {
                actor: Some("a\0b"),
                ..AuditQuery::default()
            },
            AuditQuery {
                target: Some("a\0b"),
                ..AuditQuery::default()
            },
        ];

        for query in &queries {
            let seen = list_plain(&db.pool, &Viewer::Admin, query).await;
            assert_eq!(seen.map(|records| records.len()).ok(), Some(0), "{query:?}");
        }
    }

    #[test]
    fn parse_filters_checks_actions_and_times_and_drops_empty_values() {
        let good = FilterText {
            team: Some(""),
            action: Some("user.created"),
            actor: Some(""),
            since: Some("2026-11-13T12:00:00+01:00"),
            ..FilterText::default()
        };

        let query = parse_filters(&good).unwrap();
        let bad_action = parse_filters(&FilterText {
            action: Some("no.such"),
            ..good
        });
        let bad_time = parse_filters(&FilterText {
            until: Some("yesterday"),
            ..good
        });

        assert_eq!(query.team, None);
        assert_eq!(query.actor, None);
        assert_eq!(query.action, Some(Action::UserCreated));
        assert_eq!(query.since, Some("2026-11-13T11:00:00Z".parse().unwrap()));
        assert_eq!(query.page_size(), DEFAULT_PAGE);
        assert!(
            matches!(bad_action, Err(OkfError::InvalidPath { ref path, .. }) if path == "action")
        );
        assert!(matches!(bad_time, Err(OkfError::InvalidPath { ref path, .. }) if path == "until"));
        let size = |limit| {
            AuditQuery {
                limit,
                ..AuditQuery::default()
            }
            .page_size()
        };
        assert_eq!(
            [size(-1), size(0), size(7), size(10_000)],
            [DEFAULT_PAGE, DEFAULT_PAGE, 7, MAX_PAGE]
        );
    }
}

#[cfg(test)]
mod catalog_tests;
