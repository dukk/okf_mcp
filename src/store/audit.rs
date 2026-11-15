//! The audit log (spec "Part 5"): one row per administrative change,
//! catalog write, export and admin read, written by [`record`] inside the
//! transaction of the change it records. Immutable columns never hold a
//! person's name or a reusable name: people are `audit_people` ids, teams
//! `audit_teams` ids (one per `teams.id`, never reused), credentials
//! `audit_credentials` ids; [`forget_person`] clears what identifies a
//! deleted user in those rows. Never a secret: no key, token, password,
//! link, session id or email address goes into an entry.

use std::net::IpAddr;

use chrono::{DateTime, Utc};
use sqlx::{PgConnection, PgPool};

use crate::auth::{CredentialKind, Principal};
use crate::error::OkfError;
use crate::http::requester::Requester;
use crate::store::keys::Access;

/// How many days entries are kept with no `audit_retention_days` override
/// (the append-only trigger in `migrations/0012_audit.sql` uses the same).
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
    CatalogCreated => "catalog.created",
    CatalogDeleted => "catalog.deleted",
    CatalogOwnerChanged => "catalog.owner_changed",
    CatalogMemberAccessChanged => "catalog.member_access_changed",
    CatalogDescriptionChanged => "catalog.description_changed",
    CatalogImported => "catalog.imported",
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
pub async fn record(
    conn: &mut PgConnection,
    actor: &AuditActor,
    entry: &Entry<'_>,
    now: DateTime<Utc>,
) -> Result<i64, OkfError> {
    let target = entry.target.parts();
    let person = match entry.target {
        Target::User(user_id) => Some(user_id),
        _ => entry.person,
    };
    // The subquery resolves both people once (`audit_person` creates or
    // refreshes a row; NULL in, NULL out); the credentials and the target
    // team resolve to their audit ids the same way. The actor's credential
    // label goes to `audit_credentials` (mutable), the entry gets only
    // `stored_label`. `target_operator`: the target person, or the owner of
    // a target key, grant or code (which `audit_credential` resolves the
    // same way), is an admin user now, so owners never see who it is.
    Ok(sqlx::query_scalar(
        "INSERT INTO audit_entries (at, action, source, actor_credential_ref, actor_person_id, \
             actor_label, actor_operator, client_ip, target_kind, target_name, target_team_ref, \
             target_credential_ref, target_person_id, target_operator, teams, details) \
         SELECT $1, $2, $3, audit_credential($4, p.actor, $5), p.actor, $6, $7, $8::inet, $9, \
                $10, audit_team_ref((SELECT id FROM teams WHERE name = $11)), \
                audit_credential($12, p.target, NULL), p.target, \
                COALESCE((SELECT bool_or(is_admin) FROM users WHERE id IN ($13, \
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
    // `&mut *conn` reborrows the connection, so `conn` stays usable after.
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
pub async fn prune(pool: &PgPool, before: DateTime<Utc>) -> Result<u64, OkfError> {
    Ok(sqlx::query("DELETE FROM audit_entries WHERE at < $1")
        .bind(before)
        .execute(pool)
        .await?
        .rows_affected())
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
    /// Older than this entry id (the cursor).
    pub after: Option<i64>,
    /// At most this many ([`AuditQuery::page_size`]).
    pub limit: i64,
}

impl AuditQuery<'_> {
    /// How many entries a page of this query holds: `limit`, at most
    /// [`MAX_PAGE`], and [`DEFAULT_PAGE`] when it is 0 or less.
    ///
    /// # Returns
    /// A size from 1 to [`MAX_PAGE`].
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
    /// [`AuditQuery::after`].
    pub after: Option<i64>,
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
/// [`OkfError::InvalidPath`] (400) for an unknown action, or a time that is
/// not RFC 3339, naming the filter.
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
        after: text.after,
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
}

/// One entry as a reader sees it.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct AuditRecord {
    /// Its id (the cursor).
    pub id: i64,
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

/// The entries a viewer may read, newest first.
///
/// For an owner, the actor filter never matches an operator's entry and the
/// target filter never matches a hidden (operator) person or credential, so
/// neither can reveal who operator staff are.
///
/// # Arguments
/// * `pool` - the database.
/// * `viewer` - who reads.
/// * `query` - the filters.
///
/// # Returns
/// At most [`AuditQuery::page_size`] records. A filter that cannot name
/// anything (a malformed team name, a NUL) matches nothing, without SQL.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn list(
    pool: &PgPool,
    viewer: &Viewer,
    query: &AuditQuery<'_>,
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
    // `scope`: the one audit team an owner's entries must belong to.
    let (scope, team_name, team_id, credential) = match viewer {
        Viewer::Admin => (None, query.team, None, None),
        Viewer::Owner {
            team_id,
            audit_team,
        } => (Some(*audit_team), None, Some(team_id.as_str()), None),
        Viewer::Credential(id) => (None, query.team, None, Some(id.as_str())),
    };
    let owner = matches!(viewer, Viewer::Owner { .. });
    let rows = sqlx::query_as::<_, ListedRow>(LIST_SQL)
        .bind(scope)
        .bind(query.action.map(Action::as_str))
        .bind(query.actor)
        .bind(query.target)
        .bind(query.since)
        .bind(query.until)
        .bind(query.after)
        .bind(credential)
        .bind(team_id)
        .bind(owner)
        .bind(query.page_size())
        .bind(team_name)
        .fetch_all(pool)
        .await?;
    Ok(rows.into_iter().map(|row| row.into_record(owner)).collect())
}

/// The listing. `$1` an owner's audit team id, `$2` action, `$3` actor,
/// `$4` target, `$5` since, `$6` until, `$7` cursor, `$8` credential, `$9`
/// the owner's `teams.id`, `$10` owner view, `$11` limit, `$12` a team
/// name (every audit team that had it, a personal team by its person).
/// Newest first is `ORDER BY id DESC`, the primary key.
const LIST_SQL: &str = "\
    SELECT e.id, e.at, e.action, e.source, e.actor_operator, \
           COALESCE(ac.credential_id, 'credential #' || ac.id) AS actor_credential, \
           COALESCE(ac.label, e.actor_label) AS actor_label, \
           ap.name AS actor_person, host(e.client_ip) AS ip, e.target_kind, \
           COALESCE(e.target_name, tt.name, ttp.name, tc.credential_id, 'credential #' || tc.id) \
               AS target_name, \
           tp.name AS target_person, e.target_operator, \
           ARRAY(SELECT COALESCE(t.name, p.name) \
                 FROM unnest(e.teams) WITH ORDINALITY AS r (id, n) \
                 JOIN audit_teams t ON t.id = r.id \
                 LEFT JOIN audit_people p ON p.id = t.person_id \
                 WHERE $1::bigint IS NULL OR r.id = $1 ORDER BY r.n) AS teams, \
           e.details, c.requests, \
           ($9::text IS NOT NULL AND EXISTS ( \
               SELECT 1 FROM audit_people p JOIN team_members m ON m.user_id = p.user_id \
               WHERE p.id = e.actor_person_id AND m.team_id = $9)) AS actor_in_team \
    FROM audit_entries e \
    LEFT JOIN audit_credentials ac ON ac.id = e.actor_credential_ref \
    LEFT JOIN audit_people ap ON ap.id = e.actor_person_id \
    LEFT JOIN audit_people tp ON tp.id = e.target_person_id \
    LEFT JOIN audit_teams tt ON tt.id = e.target_team_ref \
    LEFT JOIN audit_people ttp ON ttp.id = tt.person_id \
    LEFT JOIN audit_credentials tc ON tc.id = e.target_credential_ref \
    LEFT JOIN audit_access_counts c ON c.entry_id = e.id \
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
      AND ($7::bigint IS NULL OR e.id < $7) \
      AND ($8::text IS NULL OR ac.credential_id = $8) \
    ORDER BY e.id DESC LIMIT $11";

/// One row of [`LIST_SQL`], before redaction.
// `FromRow` maps each selected column to the field of the same name.
#[derive(Debug, sqlx::FromRow)]
struct ListedRow {
    id: i64,
    at: DateTime<Utc>,
    action: String,
    source: String,
    actor_operator: bool,
    actor_credential: Option<String>,
    actor_label: String,
    actor_person: Option<String>,
    ip: Option<String>,
    target_kind: String,
    target_name: Option<String>,
    target_person: Option<String>,
    target_operator: bool,
    teams: Vec<String>,
    details: serde_json::Value,
    requests: Option<i64>,
    actor_in_team: bool,
}

impl ListedRow {
    /// The record a reader sees. For an owner: an operator actor becomes
    /// [`ActorView::Operator`] with no IP; other IPs show only for the
    /// team's own members; [`OWNER_REDACTED`] keys leave `details`; a target
    /// person who is operator staff (`target_operator`, such as the admin an
    /// unlock is for) is hidden, with a credential naming them; and `teams`
    /// holds only the owner's team (the SQL drops the others, which may be
    /// another customer's or a staff member's personal team). The stored
    /// entry is never changed.
    fn into_record(self, owner: bool) -> AuditRecord {
        let actor = if owner && self.actor_operator {
            ActorView::Operator
        } else {
            ActorView::Known {
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
                kind: self.target_kind,
                person: None,
            }
        } else {
            TargetView {
                kind: self.target_kind,
                name: self.target_name,
                person: self.target_person,
            }
        };
        AuditRecord {
            id: self.id,
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

    /// Every entry with this action, oldest first.
    pub(crate) async fn entries(pool: &PgPool, action: &str) -> Vec<RecordedEntry> {
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
            display_name: "",
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
            display_name: "Alice Example",
        };
        users::create(&db.pool, &new, &AuditActor::cli(), now())
            .await
            .unwrap();
        let alice = users::find_auth(&db.pool, &user_id(&db.pool, "alice").await)
            .await
            .unwrap()
            .unwrap();
        // A real console-session principal: its `actor` is the user's name.
        // (M13's three arguments; Task 7b adds the gates: pass
        // `Default::default()` once more.)
        let principal = Principal::for_session(&alice, Default::default(), Default::default());
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
            display_name: "Alice Example",
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
        users::delete(&db.pool, "alice", false, &AuditActor::cli(), now())
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
            display_name: "",
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
            display_name: "Alice Example",
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
                display_name: "",
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
            display_name: "",
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
            display_name: "Alice",
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
                display_name: "Staff Member",
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
        // A key of the staff member as a target: hidden with its id.
        let granted = Entry {
            teams: std::slice::from_ref(&team),
            ..entry(Action::KeyCatalogGranted, Target::Key("staff-key"))
        };
        record_one(&db.pool, &actor, &granted).await;
        record_one(&db.pool, &actor, &unlocked).await;
        let owner_view = owner_of(&db.pool, TEST_TEAM).await;

        let owner = list(&db.pool, &owner_view, &AuditQuery::default())
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
            owner_hits.push(list(&db.pool, &owner_view, probe).await.unwrap().len());
            admin_hits.push(list(&db.pool, &Viewer::Admin, probe).await.unwrap().len());
        }
        let admin = list(&db.pool, &Viewer::Admin, &AuditQuery::default())
            .await
            .unwrap();

        assert_eq!(owner.len(), 2, "both entries are the team's");
        assert_eq!(owner[0].actor, ActorView::Operator);
        assert_eq!(owner[0].ip, None);
        assert_eq!(owner[0].target.person, None, "the unlocked staff member is hidden");
        assert_eq!(
            owner[0].details,
            serde_json::json!({"category": "support", "unlock": 1})
        );
        assert_eq!(
            owner[1].target,
            TargetView {
                kind: "key".to_string(),
                name: None,
                person: None
            },
            "a staff member's key is hidden as a target"
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
        assert_eq!(owner_hits, [0; 6], "no filter matches an operator for an owner");
        // The same filters do match for an admin: the target person matches
        // the unlock and staff's own `user.created` and `user.admin_changed`;
        // each actor filter matches both staff-key entries.
        assert_eq!(admin_hits, [3, 1, 2, 2, 2, 2]);
        assert_eq!(admin[0].details["reason"], "ticket 12");
        assert_eq!(admin[0].target.person.as_deref(), Some("staff"));
        assert_eq!(admin[0].ip.as_deref(), Some("192.0.2.7"));
        assert_eq!(admin[0].teams, [TEST_USER], "a personal team reads as its user");
        assert_eq!(admin[1].target.name.as_deref(), Some("staff-key"));
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
    async fn owners_see_only_their_members_ips_and_only_their_own_team() {
        let db = test_db().await;
        users::create(
            &db.pool,
            &NewUser {
                name: "bob",
                display_name: "",
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

        let owner = list(&db.pool, &owner_of(&db.pool, TEST_TEAM).await, &AuditQuery::default())
            .await
            .unwrap();
        let admin = list(&db.pool, &Viewer::Admin, &AuditQuery::default())
            .await
            .unwrap();

        assert_eq!(owner.len(), 3);
        assert_eq!(owner[0].ip, None, "an operator, though a member");
        assert_eq!(owner[1].ip.as_deref(), Some("192.0.2.22"), "the team's own member");
        assert_eq!(owner[2].ip, None, "not a member of the team");
        assert!(owner.iter().all(|record| record.teams == [TEST_USER]), "only the owner's own team");
        assert_eq!(admin[0].teams, ["bob", TEST_USER]);
        assert_eq!(admin[0].ip.as_deref(), Some("192.0.2.23"));
        assert_eq!(admin[2].ip.as_deref(), Some("192.0.2.21"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_deleted_session_user_is_named_nowhere_in_the_admin_or_owner_view() {
        let db = test_db().await;
        let family = NewTeam {
            name: "family",
            display_name: "",
            owner: TEST_USER,
        };
        teams::create(&db.pool, &family, &AuditActor::cli(), now())
            .await
            .unwrap();
        users::create(
            &db.pool,
            &NewUser {
                name: "alice",
                display_name: "Alice Example",
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
        let before = list(&db.pool, &Viewer::Admin, &AuditQuery::default())
            .await
            .unwrap();

        users::delete(&db.pool, "alice", false, &AuditActor::cli(), now())
            .await
            .unwrap();

        let admin = list(&db.pool, &Viewer::Admin, &AuditQuery::default())
            .await
            .unwrap();
        let owner = list(&db.pool, &owner_of(&db.pool, "family").await, &AuditQuery::default())
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
                display_name: "",
            },
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();
        let new_team = |owner: &'static str| NewTeam {
            name: "family",
            display_name: "",
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

        let seen = list(&db.pool, &owner_of(&db.pool, "family").await, &AuditQuery::default())
            .await
            .unwrap();
        let by_name = list(
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
        let actions: Vec<&str> = by_name.iter().map(|record| record.action.as_str()).collect();
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
        record_one(&db.pool, &admin_actor(&me), &entry(Action::SettingChanged, Target::Setting("a"))).await;
        record_one(&db.pool, &other, &entry(Action::SettingChanged, Target::Setting("b"))).await;

        let seen = list(&db.pool, &Viewer::Credential("k1".to_string()), &AuditQuery::default())
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
            record(&mut tx, &admin_actor(&me), &entry(Action::SettingChanged, Target::Setting(key)), at)
                .await
                .unwrap();
        }
        tx.commit().await.unwrap();
        let names = |records: Vec<AuditRecord>| -> Vec<String> {
            records.into_iter().filter_map(|record| record.target.name).collect()
        };
        let query = |since, until, after, limit| AuditQuery {
            action: Some(Action::SettingChanged),
            since,
            until,
            after,
            limit,
            ..AuditQuery::default()
        };

        let first = list(&db.pool, &Viewer::Admin, &query(None, None, None, 2)).await.unwrap();
        let rest = list(&db.pool, &Viewer::Admin, &query(None, None, Some(first[1].id), 2))
            .await
            .unwrap();
        let window = query(Some(now() + TimeDelta::minutes(5)), Some(now() + TimeDelta::minutes(20)), None, 0);
        let windowed = list(&db.pool, &Viewer::Admin, &window).await.unwrap();

        assert_eq!(names(first), ["c", "b"]);
        assert_eq!(names(rest), ["a"]);
        assert_eq!(names(windowed), ["b"], "since is inclusive, until exclusive");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_filter_that_names_nothing_matches_nothing_without_sql() {
        let db = test_db().await;
        let queries = [
            AuditQuery { team: Some("a\0b"), ..AuditQuery::default() },
            AuditQuery { team: Some("Not A Name"), ..AuditQuery::default() },
            AuditQuery { actor: Some("a\0b"), ..AuditQuery::default() },
            AuditQuery { target: Some("a\0b"), ..AuditQuery::default() },
        ];

        for query in &queries {
            let seen = list(&db.pool, &Viewer::Admin, query).await;
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
        let bad_action = parse_filters(&FilterText { action: Some("no.such"), ..good });
        let bad_time = parse_filters(&FilterText { until: Some("yesterday"), ..good });

        assert_eq!(query.team, None);
        assert_eq!(query.actor, None);
        assert_eq!(query.action, Some(Action::UserCreated));
        assert_eq!(query.since, Some("2026-11-13T11:00:00Z".parse().unwrap()));
        assert_eq!(query.page_size(), DEFAULT_PAGE);
        assert!(matches!(bad_action, Err(OkfError::InvalidPath { ref path, .. }) if path == "action"));
        assert!(matches!(bad_time, Err(OkfError::InvalidPath { ref path, .. }) if path == "until"));
        let size = |limit| AuditQuery { limit, ..AuditQuery::default() }.page_size();
        assert_eq!([size(-1), size(0), size(7), size(10_000)], [DEFAULT_PAGE, DEFAULT_PAGE, 7, MAX_PAGE]);
    }
}
