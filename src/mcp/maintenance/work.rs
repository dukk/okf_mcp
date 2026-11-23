//! Report-only investigation leases, evidence reads and submissions.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::OkfError;
use crate::maintenance::types::{ClaimResult, Lease, Receipt, RevisionPin, RunState, Submission};
use crate::mcp::context::ToolContext;
use crate::store::audit::AccessKind;
use crate::store::keys::Access;
use crate::store::maintenance::{evidence, reports, runs};

use super::{actor, lease_path, missing_record, MaintenanceResult};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MaintenanceClaimParams {
    /// Required catalog id (the `name` from `list_catalogs`), which the key must reach; connection defaults do not apply.
    pub catalog: String,
    /// Caller-chosen retry key (1..=128 non-whitespace ASCII bytes); reuse only for identical input.
    pub idempotency_key: String,
    /// One bounded batch (default 10, minimum 1, maximum 50).
    #[schemars(range(min = 1, max = 50))]
    pub batch_size: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MaintenanceRenewParams {
    /// Required catalog id (the `name` from `list_catalogs`), which the key must reach; connection defaults do not apply.
    pub catalog: String,
    /// Opaque work ID returned by maintenance_claim.
    pub work_id: String,
    /// Current opaque lease token; must belong to the calling credential.
    pub token: String,
    /// Caller-chosen retry key (1..=128 non-whitespace ASCII bytes); reuse only for identical input.
    pub idempotency_key: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MaintenanceReadRevisionParams {
    /// Required catalog id (the `name` from `list_catalogs`), which the key must reach; connection defaults do not apply.
    pub catalog: String,
    /// Published concept path in this catalog; never an unpublished changeset path.
    pub path: String,
    /// Positive immutable revision number returned by a claim or history read.
    #[schemars(range(min = 1))]
    pub revision: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MaintenanceSubmitParams {
    /// Required catalog id (the `name` from `list_catalogs`), which the key must reach; connection defaults do not apply.
    pub catalog: String,
    /// Opaque work ID returned by maintenance_claim.
    pub work_id: String,
    /// Current opaque lease token; must belong to the calling credential.
    pub token: String,
    /// Caller-chosen retry key (1..=128 non-whitespace ASCII bytes); reuse only for identical input.
    pub idempotency_key: String,
    /// Revision-pinned assessment and proposed findings; never applies canonical edits.
    pub submission: Submission,
}

// The `schema_with` hook makes the `JsonSchema` derive emit a wrapper function
// of its own that cannot carry an attribute, and an exemption on the struct or
// the field does not reach it. The struct lives in its own module so the
// exemption covers exactly that module (as `maintenance::types::Submission`).
#[cfg_attr(
    dylint_lib = "hyp_tracing",
    allow(
        uninstrumented_fn,
        reason = "constant trait plumbing: the derive's schema_with hook returns a fixed schema"
    )
)]
mod close_params {
    use super::{close_state_schema, JsonSchema, RunState};
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
    #[serde(deny_unknown_fields)]
    pub struct MaintenanceCloseParams {
        /// Required catalog id (the `name` from `list_catalogs`), which the key must reach; connection defaults do not apply.
        pub catalog: String,
        /// Opaque run ID belonging to the named catalog.
        pub run_id: String,
        /// Requested terminal state; completion requires all work to have ended.
        #[schemars(schema_with = "close_state_schema")]
        pub state: RunState,
        /// Human-readable reason recorded with attribution; secrets and oversized text are refused.
        pub rationale: String,
        /// Caller-chosen retry key (1..=128 non-whitespace ASCII bytes); reuse only for identical input.
        pub idempotency_key: String,
    }
}

pub use close_params::MaintenanceCloseParams;

#[cfg_attr(
    dylint_lib = "hyp_tracing",
    allow(
        uninstrumented_fn,
        reason = "constant trait plumbing: the derive's schema_with hook returns a fixed schema"
    )
)]
fn close_state_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({"type": "string", "enum": ["completed", "failed", "cancelled"]})
}

#[tracing::instrument(level = "debug", skip_all)]
pub(crate) async fn maintenance_claim(
    ctx: &ToolContext,
    params: MaintenanceClaimParams,
) -> Result<MaintenanceResult<ClaimResult>, OkfError> {
    let state = ctx.one(Some(&params.catalog)).await?;
    ctx.require_maintenance_report(&state).await?;
    let mut result = runs::claim(
        ctx.app().accounts(),
        ctx.app().main_store(),
        state.id(),
        &actor(ctx),
        &params.idempotency_key,
        params.batch_size.unwrap_or(10) as usize,
        ctx.app().config().maintenance_review_interval(state.name()),
        ctx.app().clock().now(),
    )
    .await?;
    for lease in &mut result.leases {
        lease_path(lease);
    }
    Ok(MaintenanceResult::new(state.name(), result))
}

#[tracing::instrument(level = "debug", skip_all)]
pub(crate) async fn maintenance_renew(
    ctx: &ToolContext,
    params: MaintenanceRenewParams,
) -> Result<MaintenanceResult<Lease>, OkfError> {
    let state = ctx.one(Some(&params.catalog)).await?;
    ctx.require_maintenance_report(&state).await?;
    let mut result = runs::renew(
        ctx.app().main_store(),
        state.id(),
        &actor(ctx),
        &params.work_id,
        &params.token,
        &params.idempotency_key,
        ctx.app().clock().now(),
    )
    .await
    .map_err(missing_record)?;
    lease_path(&mut result);
    Ok(MaintenanceResult::new(state.name(), result))
}

#[tracing::instrument(level = "debug", skip_all)]
pub(crate) async fn maintenance_read_revision(
    ctx: &ToolContext,
    params: MaintenanceReadRevisionParams,
) -> Result<MaintenanceResult<evidence::PinnedConcept>, OkfError> {
    let state = ctx.one(Some(&params.catalog)).await?;
    // Refused before `admit` below, which ignores refusals.
    ctx.require_contents(Access::Read, &state).await?;
    let pin = RevisionPin {
        path: params.path,
        revision: params.revision,
    };
    // Normalize once at the boundary; the store's exact-byte reader takes a stored path.
    let pin = crate::maintenance::validate::pin(&pin)?;
    // It answers a document's text at a revision: a read of the contents,
    // admitted once the request is valid. The other tools here claim,
    // renew, submit or close work and read none, so they admit nothing.
    ctx.admit(&state, AccessKind::Read).await?;
    let mut result = evidence::read(ctx.app().main_store(), state.id(), &pin).await?;
    super::pin_path(&mut result.pin);
    Ok(MaintenanceResult::new(state.name(), result))
}

#[tracing::instrument(level = "debug", skip_all)]
pub(crate) async fn maintenance_submit(
    ctx: &ToolContext,
    params: MaintenanceSubmitParams,
) -> Result<MaintenanceResult<Receipt>, OkfError> {
    let state = ctx.one(Some(&params.catalog)).await?;
    ctx.require_maintenance_report(&state).await?;
    let result = reports::submit(
        ctx.app().main_store(),
        state.id(),
        &actor(ctx),
        &params.work_id,
        &params.token,
        &params.idempotency_key,
        &params.submission,
        ctx.app().clock().now(),
    )
    .await?;
    Ok(MaintenanceResult::new(state.name(), result))
}

#[tracing::instrument(level = "debug", skip_all)]
pub(crate) async fn maintenance_close(
    ctx: &ToolContext,
    params: MaintenanceCloseParams,
) -> Result<MaintenanceResult<runs::RunSummary>, OkfError> {
    let state = ctx.one(Some(&params.catalog)).await?;
    // Ordinary Write is an operator override, never a client-supplied option.
    // It acts on the catalog as a write, so it needs the unlock a write
    // would (M14 Task 9b); without one, only the caller's own run closes.
    // Checked against the row of this very incarnation
    // (`ToolContext::require_contents`), so an unlock of a deleted catalog
    // does not make an operator of the replacement's runs.
    let operator = ctx.require_contents(Access::Write, &state).await.is_ok();
    if !operator {
        ctx.require_maintenance_report(&state).await?;
    }
    if params.run_id.trim().is_empty() {
        return Err(OkfError::Refused {
            reason: "maintenance ID must not be blank".into(),
        });
    }
    let owner: Option<String> = sqlx::query_scalar(
        "SELECT credential_id FROM maintenance_runs WHERE catalog_id=$1 AND id=$2",
    )
    .bind(state.id())
    .bind(&params.run_id)
    .fetch_optional(ctx.app().main_store().executor())
    .await?;
    let owner = owner.ok_or_else(super::not_found)?;
    let own = owner == ctx.principal().key_id;
    if own {
        ctx.require_maintenance_report(&state).await?;
    } else if !operator {
        return Err(super::not_found());
    }
    // Own-run retries keep the same store payload even if ordinary access changes.
    let result = runs::close(
        ctx.app().main_store(),
        state.id(),
        &actor(ctx),
        &params.run_id,
        params.state,
        &params.rationale,
        !own && operator,
        &params.idempotency_key,
        ctx.app().clock().now(),
    )
    .await
    .map_err(missing_record)?;
    Ok(MaintenanceResult::new(state.name(), result))
}

/// M13 Task 3: every maintenance work tool checks access through
/// `Principal::require` before it reads its ids, so a suspended catalog is
/// refused to a non-admin reporter with `Suspended`, stays unknown to a
/// reporter that does not reach it, and still answers an admin.
#[cfg(test)]
mod suspension_tests {
    use super::*;

    use crate::config::Config;
    use crate::maintenance::types::{Assessment, WorkOutcome};
    use crate::mcp::testing::{
        app_with, cached_principal, context, stored_secret, suspend_catalogs, TestApp,
    };

    /// Each work tool on `catalog` as the key behind `secret`, as `(tool,
    /// error kind or "ok")`. The ids are made up: access is checked first.
    async fn every_tool(
        test: &TestApp,
        secret: &str,
        catalog: &str,
    ) -> Vec<(&'static str, String)> {
        fn outcome<T>(result: Result<T, OkfError>) -> String {
            result.map_or_else(|error| error.kind().to_string(), |_| "ok".to_string())
        }
        let ctx = context(test, cached_principal(test, secret).await, None);
        let catalog = catalog.to_string();
        let submission = Submission {
            outcome: WorkOutcome::Completed,
            assessment: Assessment::NotAssessed,
            summary: "Nothing to report.".to_string(),
            evidence: Vec::new(),
            findings: Vec::new(),
            reported_tokens: None,
        };
        vec![
            (
                "maintenance_claim",
                outcome(
                    maintenance_claim(
                        &ctx,
                        MaintenanceClaimParams {
                            catalog: catalog.clone(),
                            idempotency_key: "claim".to_string(),
                            batch_size: Some(1),
                        },
                    )
                    .await,
                ),
            ),
            (
                "maintenance_renew",
                outcome(
                    maintenance_renew(
                        &ctx,
                        MaintenanceRenewParams {
                            catalog: catalog.clone(),
                            work_id: "no-such-work".to_string(),
                            token: "no-such-token".to_string(),
                            idempotency_key: "renew".to_string(),
                        },
                    )
                    .await,
                ),
            ),
            (
                "maintenance_read_revision",
                outcome(
                    maintenance_read_revision(
                        &ctx,
                        MaintenanceReadRevisionParams {
                            catalog: catalog.clone(),
                            path: "a.md".to_string(),
                            revision: 1,
                        },
                    )
                    .await,
                ),
            ),
            (
                "maintenance_submit",
                outcome(
                    maintenance_submit(
                        &ctx,
                        MaintenanceSubmitParams {
                            catalog: catalog.clone(),
                            work_id: "no-such-work".to_string(),
                            token: "no-such-token".to_string(),
                            idempotency_key: "submit".to_string(),
                            submission,
                        },
                    )
                    .await,
                ),
            ),
            (
                "maintenance_close",
                outcome(
                    maintenance_close(
                        &ctx,
                        MaintenanceCloseParams {
                            catalog,
                            run_id: "no-such-run".to_string(),
                            state: RunState::Cancelled,
                            rationale: "Stop.".to_string(),
                            idempotency_key: "close".to_string(),
                        },
                    )
                    .await,
                ),
            ),
        ]
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn every_work_tool_refuses_a_suspended_catalog_but_to_an_admin() {
        let note = "---\ntype: Note\ntitle: A\ndescription: A note.\n---\n# A\n";
        let test = app_with(
            &[("pg", &[("a.md", note)]), ("other", &[])],
            Config::default(),
        )
        .await;
        let reporter = stored_secret(&test, "reporter", Access::Write, &["pg"], true).await;
        let stranger = stored_secret(&test, "stranger", Access::Write, &["other"], true).await;
        let root = stored_secret(&test, "root", Access::Admin, &[], true).await;
        suspend_catalogs(&test, &["pg"]).await;

        let suspended = every_tool(&test, &reporter, "pg").await;
        let strangers = every_tool(&test, &stranger, "pg").await;
        let admins = every_tool(&test, &root, "pg").await;

        for (tool, kind) in &suspended {
            assert_eq!(kind, "Suspended", "{tool} as the catalog's reporter");
        }
        for (tool, kind) in &strangers {
            assert_eq!(
                kind, "CatalogNotSelected",
                "{tool}: an unreached catalog never reads as suspended"
            );
        }
        for (tool, kind) in &admins {
            assert_ne!(kind, "Suspended", "{tool} as an admin");
        }
        assert_eq!(admins[0].1, "ok", "an admin still claims");
        assert_eq!(admins[2].1, "ok", "an admin still reads a revision");
    }

    /// M14 Task 9b, the locked twin: with `require_unlock` on, operator
    /// staff are refused every work tool with `UnlockRequired` at the
    /// selection (before the tools' own checks, M14 Decision 24), the
    /// maintenance runner needs no unlock, and a catalog whose owners
    /// turned maintenance off refuses the runner with `MaintenanceOff`.
    #[tokio::test(flavor = "multi_thread")]
    async fn every_work_tool_needs_an_unlock_but_for_the_maintenance_runner() {
        use crate::mcp::testing::{require_unlock, staff_key, staff_root};
        let note = "---\ntype: Note\ntitle: A\ndescription: A note.\n---\n# A\n";
        let test = app_with(
            &[("pg", &[("a.md", note)]), ("quiet", &[("a.md", note)])],
            Config::default(),
        )
        .await;
        let staff = staff_root(&test).await;
        let runner = staff_key(&test, "runner", Access::Read, true).await;
        sqlx::query("UPDATE catalogs SET maintenance = false WHERE name = 'quiet'")
            .execute(test.app.pool())
            .await
            .unwrap();
        // `require_unlock` bumps the generation, so its poll loads both.
        require_unlock(&test).await;

        let locked = every_tool(&test, &staff, "pg").await;
        let exempt = every_tool(&test, &runner, "pg").await;
        let opted_out = every_tool(&test, &runner, "quiet").await;

        for (tool, kind) in &locked {
            assert_eq!(kind, "UnlockRequired", "{tool} as operator staff");
        }
        for (tool, kind) in &exempt {
            assert_ne!(kind, "UnlockRequired", "{tool} as the runner");
        }
        assert_eq!(exempt[0].1, "ok", "the runner claims");
        assert_eq!(exempt[2].1, "ok", "the runner reads a revision");
        for (tool, kind) in &opted_out {
            assert_eq!(kind, "Forbidden", "{tool}: maintenance is off");
        }
    }
}

#[cfg(test)]
mod submit_schema_tests {
    use super::*;

    #[test]
    fn the_submission_property_is_an_inline_object_with_its_full_shape() {
        let schema = schemars::schema_for!(MaintenanceSubmitParams).to_value();
        let property = &schema["properties"]["submission"];

        assert_eq!(property["type"], "object");
        assert!(property.get("$ref").is_none());
        let properties = property["properties"].as_object().expect("properties");
        for name in [
            "outcome",
            "assessment",
            "summary",
            "evidence",
            "findings",
            "reported_tokens",
        ] {
            assert!(properties.contains_key(name), "missing {name}");
        }
        assert_eq!(property["properties"]["outcome"]["type"], "string");
        let required = property["required"].as_array().expect("required");
        assert!(required.iter().any(|name| name == "summary"));
        assert_eq!(property["additionalProperties"], false);
    }
}
