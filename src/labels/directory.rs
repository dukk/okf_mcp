//! [`Labels`]: every user's, team's and catalog's label, by slug, loaded
//! with the key cache (plan Decision 1). Every input to a label bumps
//! `admin_generation`, so this reloads exactly when a label can change.

use std::collections::HashMap;
use std::sync::Arc;

use sqlx::PgPool;

use super::{catalog_label, team_label, user_label};
use crate::error::OkfError;

/// A `users` row as the directory reads it.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct UserRow {
    /// Its slug.
    pub name: String,
    /// Its display name.
    pub display_name: String,
    /// Its email, if set.
    pub email: Option<String>,
}

/// A `teams` row as the directory reads it.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct TeamRow {
    /// Its slug.
    pub name: String,
    /// Its display name.
    pub display_name: String,
    /// The user slug whose personal team it is.
    pub personal_of: Option<String>,
}

/// A `catalogs` row as the directory reads it.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct CatalogRow {
    /// Its slug.
    pub name: String,
    /// Its display name.
    pub display_name: String,
    /// Its description.
    pub description: String,
    /// Its owner team's slug.
    pub owner_team: Option<String>,
}

/// One catalog's label and owner.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CatalogEntry {
    // The stored value: Tasks 5 and 7 return it as `display_name`, and a
    // label cannot tell whether it was unset.
    display_name: Arc<str>,
    label: Arc<str>,
    owner_team: Option<Arc<str>>,
}

/// Every label, by slug. Users and teams are separate maps: a user and its
/// personal team share a slug.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Labels {
    users: HashMap<String, Arc<str>>,
    teams: HashMap<String, Arc<str>>,
    catalogs: HashMap<String, CatalogEntry>,
    // user slug -> the slugs of its teams.
    memberships: HashMap<String, Vec<String>>,
}

impl Labels {
    /// Reads every user, team, catalog and membership.
    ///
    /// # Arguments
    /// * `pool` - the database.
    ///
    /// # Returns
    /// The directory.
    ///
    /// # Errors
    /// [`OkfError::Database`].
    #[tracing::instrument(level = "debug", skip_all, fields(db.system.name = "postgresql", db.operation.name = "labels.load"))]
    pub async fn load(pool: &PgPool) -> Result<Self, OkfError> {
        let (users, teams, catalogs, memberships) = tokio::try_join!(
            sqlx::query_as::<_, UserRow>(
                "SELECT name, display_name, email::text AS email FROM users"
            )
            .fetch_all(pool),
            sqlx::query_as::<_, TeamRow>(
                "SELECT t.name, t.display_name, u.name AS personal_of \
                 FROM teams t LEFT JOIN users u ON u.id = t.personal_of",
            )
            .fetch_all(pool),
            sqlx::query_as::<_, CatalogRow>(
                "SELECT c.name, c.display_name, c.description, t.name AS owner_team \
                 FROM catalogs c LEFT JOIN teams t ON t.id = c.owner_team_id",
            )
            .fetch_all(pool),
            sqlx::query_as::<_, (String, String)>(
                "SELECT u.name, t.name FROM team_members m \
                 JOIN users u ON u.id = m.user_id JOIN teams t ON t.id = m.team_id",
            )
            .fetch_all(pool),
        )?;
        Ok(Self::from_rows(users, teams, catalogs, memberships))
    }

    /// Builds the directory from rows (the unit tests' entry point).
    ///
    /// # Arguments
    /// * `users`, `teams`, `catalogs` - the rows.
    /// * `memberships` - `(user slug, team slug)` pairs.
    ///
    /// # Returns
    /// The directory.
    #[tracing::instrument(level = "debug", skip_all)]
    pub fn from_rows(
        users: Vec<UserRow>,
        teams: Vec<TeamRow>,
        catalogs: Vec<CatalogRow>,
        memberships: Vec<(String, String)>,
    ) -> Self {
        let users: HashMap<String, Arc<str>> = users
            .into_iter()
            .map(|row| {
                let label = user_label(&row.display_name, row.email.as_deref());
                (row.name, Arc::from(label))
            })
            .collect();
        let teams = teams
            .into_iter()
            .map(|row| {
                // A personal team whose user is gone (never: RESTRICT) would
                // fall back to its own display name.
                let owner = row.personal_of.as_deref().and_then(|user| users.get(user));
                let label = team_label(&row.display_name, owner.map(|label| &**label));
                (row.name, Arc::from(label))
            })
            .collect();
        let catalogs = catalogs
            .into_iter()
            .map(|row| {
                let entry = CatalogEntry {
                    label: Arc::from(catalog_label(&row.display_name, &row.description)),
                    display_name: Arc::from(row.display_name),
                    owner_team: row.owner_team.map(Arc::from),
                };
                (row.name, entry)
            })
            .collect();
        let mut by_user: HashMap<String, Vec<String>> = HashMap::new();
        for (user, team) in memberships {
            // `entry(..).or_default()` is Python's `setdefault(key, [])`.
            by_user.entry(user).or_default().push(team);
        }
        Self {
            users,
            teams,
            catalogs,
            memberships: by_user,
        }
    }
}

impl Labels {
    /// A user's label.
    ///
    /// # Arguments
    /// * `slug` - the user's slug.
    ///
    /// # Returns
    /// The label, or `None` for a user not loaded (created since).
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "trivial accessor or conversion: returns or maps a value, a span would record nothing new"
        )
    )]
    pub fn user(&self, slug: &str) -> Option<&str> {
        self.users.get(slug).map(|label| &**label)
    }

    /// A team's label.
    ///
    /// # Arguments
    /// * `slug` - the team's slug.
    ///
    /// # Returns
    /// The label, or `None` for a team not loaded.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "trivial accessor or conversion: returns or maps a value, a span would record nothing new"
        )
    )]
    pub fn team(&self, slug: &str) -> Option<&str> {
        self.teams.get(slug).map(|label| &**label)
    }

    /// A catalog's label (not told apart from a duplicate: see
    /// [`Labels::catalog_choices`]).
    ///
    /// # Arguments
    /// * `slug` - the catalog's slug.
    ///
    /// # Returns
    /// The label, or `None` for a catalog not loaded.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "trivial accessor or conversion: returns or maps a value, a span would record nothing new"
        )
    )]
    pub fn catalog(&self, slug: &str) -> Option<&str> {
        self.catalogs.get(slug).map(|entry| &*entry.label)
    }

    /// A catalog's stored display name, empty when unset.
    ///
    /// # Arguments
    /// * `slug` - the catalog's slug.
    ///
    /// # Returns
    /// The stored value, or `None` for a catalog not loaded.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "trivial accessor or conversion: returns or maps a value, a span would record nothing new"
        )
    )]
    pub fn catalog_display_name(&self, slug: &str) -> Option<&str> {
        self.catalogs.get(slug).map(|entry| &*entry.display_name)
    }

    /// The label of an actor string, when the actor is a user slug (plan
    /// Decision 5: any other actor, such as `claude-code/<model>`, is shown
    /// as written).
    ///
    /// # Arguments
    /// * `actor` - the audit actor text.
    ///
    /// # Returns
    /// The user's label, or `None`.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "trivial accessor or conversion: returns or maps a value, a span would record nothing new"
        )
    )]
    pub fn actor(&self, actor: &str) -> Option<&str> {
        self.user(actor)
    }

    /// A catalog's owner team slug.
    ///
    /// # Arguments
    /// * `slug` - the catalog's slug.
    ///
    /// # Returns
    /// The owner team's slug; `None` when unassigned or not loaded.
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "trivial accessor or conversion: returns or maps a value, a span would record nothing new"
        )
    )]
    pub fn catalog_owner(&self, slug: &str) -> Option<&str> {
        self.catalogs
            .get(slug)
            .and_then(|entry| entry.owner_team.as_deref())
    }

    /// The slugs of a user's teams.
    ///
    /// # Arguments
    /// * `user` - the user's slug.
    ///
    /// # Returns
    /// An iterator over the team slugs (empty for an unknown user).
    #[cfg_attr(
        dylint_lib = "hyp_tracing",
        allow(
            uninstrumented_fn,
            reason = "trivial accessor or conversion: returns or maps a value, a span would record nothing new"
        )
    )]
    pub fn teams_of(&self, user: &str) -> impl Iterator<Item = &str> {
        self.memberships
            .get(user)
            .into_iter()
            .flatten()
            .map(String::as_str)
    }

    /// The text to show for each of some catalogs, every text distinct.
    ///
    /// A label shared by several of them is followed by its owner team's
    /// label; whatever still collides, including a literal label shaped
    /// like a generated one, moves to `{label} (id: {slug})` until no text
    /// occurs twice. A fallback ends in its own slug, which has no space or
    /// `)`, so two fallbacks never collide and the loop ends.
    ///
    /// # Arguments
    /// * `slugs` - the catalogs to show, in order.
    ///
    /// # Returns
    /// `(slug, shown text)` pairs in the input order. An unknown slug is
    /// shown as itself.
    #[tracing::instrument(level = "debug", skip_all)]
    pub fn catalog_choices<'a>(
        &self,
        slugs: impl IntoIterator<Item = &'a str>,
    ) -> Vec<(String, String)> {
        struct Choice<'a> {
            slug: &'a str,
            label: &'a str,
            shown: String,
            on_fallback: bool,
        }
        let mut choices: Vec<Choice<'_>> = slugs
            .into_iter()
            .map(|slug| {
                let label = self.catalog(slug).unwrap_or(slug);
                Choice {
                    slug,
                    label,
                    shown: label.to_string(),
                    on_fallback: false,
                }
            })
            .collect();
        let mut label_counts: HashMap<&str, usize> = HashMap::with_capacity(choices.len());
        for choice in &choices {
            *label_counts.entry(choice.label).or_default() += 1;
        }
        for choice in &mut choices {
            if label_counts
                .get(choice.label)
                .is_some_and(|count| *count > 1)
            {
                let owner = self
                    .catalog_owner(choice.slug)
                    .and_then(|team| self.team(team))
                    .unwrap_or("no team");
                choice.shown = format!("{} ({owner})", choice.label);
            }
        }
        loop {
            let mut shown_counts: HashMap<String, usize> = HashMap::with_capacity(choices.len());
            for choice in &choices {
                *shown_counts.entry(choice.shown.clone()).or_default() += 1;
            }
            let mut changed = false;
            for choice in &mut choices {
                let duplicated = shown_counts
                    .get(&choice.shown)
                    .is_some_and(|count| *count > 1);
                if duplicated && !choice.on_fallback {
                    choice.shown = format!("{} (id: {})", choice.label, choice.slug);
                    choice.on_fallback = true;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        choices
            .into_iter()
            .map(|choice| (choice.slug.to_string(), choice.shown))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Labels {
        Labels::from_rows(
            vec![
                UserRow {
                    name: "u-1".into(),
                    display_name: "Alice".into(),
                    email: Some("alice@example.com".into()),
                },
                UserRow {
                    name: "u-2".into(),
                    display_name: "".into(),
                    email: Some("bob@example.com".into()),
                },
            ],
            vec![
                TeamRow {
                    name: "u-1".into(),
                    display_name: "".into(),
                    personal_of: Some("u-1".into()),
                },
                TeamRow {
                    name: "acme".into(),
                    display_name: "Acme".into(),
                    personal_of: None,
                },
            ],
            vec![
                CatalogRow {
                    name: "m-1".into(),
                    display_name: "".into(),
                    description: "Personal memory".into(),
                    owner_team: Some("u-1".into()),
                },
                CatalogRow {
                    name: "m-2".into(),
                    display_name: "Personal memory".into(),
                    description: "".into(),
                    owner_team: Some("acme".into()),
                },
            ],
            vec![("u-1".into(), "acme".into()), ("u-1".into(), "u-1".into())],
        )
    }

    #[test]
    fn labels_follow_the_rule() {
        let labels = sample();
        assert_eq!(labels.user("u-2"), Some("bob"));
        assert_eq!(labels.team("u-1"), Some("Alice's team"));
        assert_eq!(labels.catalog("m-1"), Some("Personal memory"));
        assert_eq!(labels.user("u-9"), None);
    }

    #[test]
    fn an_actor_resolves_only_when_it_is_a_user_slug() {
        let labels = sample();
        assert_eq!(labels.actor("u-1"), Some("Alice"));
        assert_eq!(labels.actor("claude-code/model"), None);
    }

    #[test]
    fn duplicate_catalog_labels_are_told_apart_by_owner_team() {
        let labels = sample();
        let choices = labels.catalog_choices(["m-1", "m-2"]);
        assert_eq!(choices[0].1, "Personal memory (Alice's team)");
        assert_eq!(choices[1].1, "Personal memory (Acme)");
        let alone = labels.catalog_choices(["m-2"]);
        assert_eq!(alone[0].1, "Personal memory");
    }

    #[test]
    fn same_owner_duplicates_fall_back_to_the_id() {
        let labels = Labels::from_rows(
            vec![],
            vec![TeamRow {
                name: "acme".into(),
                display_name: "Acme".into(),
                personal_of: None,
            }],
            vec![
                CatalogRow {
                    name: "m-1".into(),
                    display_name: "Notes".into(),
                    description: "".into(),
                    owner_team: Some("acme".into()),
                },
                CatalogRow {
                    name: "m-2".into(),
                    display_name: "Notes".into(),
                    description: "".into(),
                    owner_team: Some("acme".into()),
                },
            ],
            vec![],
        );
        let choices = labels.catalog_choices(["m-1", "m-2"]);
        assert_eq!(choices[0].1, "Notes (id: m-1)");
        assert_eq!(choices[1].1, "Notes (id: m-2)");
    }

    #[test]
    fn a_literal_label_shaped_like_a_fallback_still_gets_a_distinct_choice() {
        // m-3's own display name is exactly m-1's fallback text.
        let labels = Labels::from_rows(
            vec![],
            vec![TeamRow {
                name: "acme".into(),
                display_name: "Acme".into(),
                personal_of: None,
            }],
            vec![
                CatalogRow {
                    name: "m-1".into(),
                    display_name: "Notes".into(),
                    description: "".into(),
                    owner_team: Some("acme".into()),
                },
                CatalogRow {
                    name: "m-2".into(),
                    display_name: "Notes".into(),
                    description: "".into(),
                    owner_team: Some("acme".into()),
                },
                CatalogRow {
                    name: "m-3".into(),
                    display_name: "Notes (id: m-1)".into(),
                    description: "".into(),
                    owner_team: Some("acme".into()),
                },
            ],
            vec![],
        );
        let choices = labels.catalog_choices(["m-1", "m-2", "m-3"]);
        assert_eq!(choices[0].1, "Notes (id: m-1)");
        assert_eq!(choices[1].1, "Notes (id: m-2)");
        assert_eq!(choices[2].1, "Notes (id: m-1) (id: m-3)");
    }

    #[test]
    fn owner_teams_sharing_a_label_fall_back_to_the_id() {
        let labels = Labels::from_rows(
            vec![],
            vec![
                TeamRow {
                    name: "acme-1".into(),
                    display_name: "Acme".into(),
                    personal_of: None,
                },
                TeamRow {
                    name: "acme-2".into(),
                    display_name: "Acme".into(),
                    personal_of: None,
                },
            ],
            vec![
                CatalogRow {
                    name: "m-1".into(),
                    display_name: "Notes".into(),
                    description: "".into(),
                    owner_team: Some("acme-1".into()),
                },
                CatalogRow {
                    name: "m-2".into(),
                    display_name: "Notes".into(),
                    description: "".into(),
                    owner_team: Some("acme-2".into()),
                },
                CatalogRow {
                    name: "m-3".into(),
                    display_name: "Other".into(),
                    description: "".into(),
                    owner_team: Some("acme-1".into()),
                },
            ],
            vec![],
        );
        let choices = labels.catalog_choices(["m-1", "m-2", "m-3"]);
        assert_eq!(choices[0].1, "Notes (id: m-1)");
        assert_eq!(choices[1].1, "Notes (id: m-2)");
        assert_eq!(choices[2].1, "Other");
    }

    #[hegel::test]
    fn catalog_choices_are_always_distinct(tc: hegel::TestCase) {
        use hegel::generators;
        // A small pool of labels, including ones shaped like the generated
        // suffixes ("(Same)", "(id: m-N)"), so every kind of collision is
        // common: plain duplicates, owner-suffix collisions and literal
        // labels equal to another catalog's fallback.
        const POOL: [&str; 6] = [
            "L0",
            "L1",
            "L0 (Same)",
            "L0 (id: m-0)",
            "L0 (id: m-1)",
            "L0 (id: m-1) (id: m-2)",
        ];
        let count = tc.draw(generators::integers::<usize>().min_value(1).max_value(8));
        let rows: Vec<CatalogRow> = (0..count)
            .map(|i| {
                let label = tc.draw(generators::integers::<usize>().max_value(POOL.len() - 1));
                let team = tc.draw(generators::integers::<u8>().max_value(1));
                CatalogRow {
                    name: format!("m-{i}"),
                    display_name: POOL[label].to_string(),
                    description: String::new(),
                    owner_team: Some(format!("t-{team}")),
                }
            })
            .collect();
        let teams = (0..2)
            .map(|t| TeamRow {
                name: format!("t-{t}"),
                display_name: "Same".into(),
                personal_of: None,
            })
            .collect();
        let slugs: Vec<String> = rows.iter().map(|row| row.name.clone()).collect();
        let labels = Labels::from_rows(vec![], teams, rows, vec![]);
        let choices = labels.catalog_choices(slugs.iter().map(String::as_str));
        let distinct: std::collections::HashSet<&str> =
            choices.iter().map(|(_, label)| label.as_str()).collect();
        assert_eq!(distinct.len(), choices.len());
    }

    #[test]
    fn the_directory_keeps_a_catalogs_raw_display_name() {
        let labels = sample();
        // m-1 has no display name; its label comes from the description.
        assert_eq!(labels.catalog_display_name("m-1"), Some(""));
        assert_eq!(labels.catalog("m-1"), Some("Personal memory"));
        assert_eq!(labels.catalog_display_name("m-2"), Some("Personal memory"));
        assert_eq!(labels.catalog_display_name("m-9"), None);
    }

    #[test]
    fn teams_of_lists_a_users_teams() {
        let labels = sample();
        let mut teams: Vec<&str> = labels.teams_of("u-1").collect();
        teams.sort_unstable();
        assert_eq!(teams, ["acme", "u-1"]);
    }
}
