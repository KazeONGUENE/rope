//! GraphQL stakeholder endpoint - spec v1.0 §6.3: "REST and GraphQL
//! endpoints" for disintermediated stakeholder access.
//!
//! One `POST /api/v1/ecosystem/stakeholder/graphql` route, authenticated
//! exactly like the REST gateway (grant bearer token or EIP-191 wallet
//! signature). The resolver set mirrors the REST facets - overview,
//! readings, diagnoses, approvals, project card - and every field is
//! filtered to the grant scope, so a stakeholder can compose precisely
//! the query they need in one round-trip instead of stitching REST calls.
//!
//! Sandbox keys resolve against the same deterministic synthetic stream
//! the REST gateway serves, so a GraphQL integration can be validated
//! end-to-end before touching live data.

use std::sync::Arc;

use async_graphql::{Context, EmptyMutation, EmptySubscription, Object, Schema, SimpleObject};

use crate::grants::AccessGrant;
use crate::registry::Registry;
use crate::simulation;
use crate::types::{now_ts, Project, TelemetryReading};

/// Everything a resolver needs, resolved once at auth time.
pub struct GqlSession {
    pub registry: Arc<Registry>,
    pub grant: AccessGrant,
    pub project: Project,
    pub sandbox: bool,
}

impl GqlSession {
    fn asset_category(&self, asset_id: &str) -> String {
        self.project
            .inventory
            .assets
            .iter()
            .find(|a| a.id == asset_id)
            .map(|a| a.category.clone())
            .unwrap_or_default()
    }

    /// The grant-scoped reading set - synthetic for sandbox sessions,
    /// live-journal otherwise.
    fn scoped_readings(&self) -> Vec<TelemetryReading> {
        let raw: Vec<TelemetryReading> = if self.sandbox {
            simulation::synth_history(&self.project, 120, now_ts())
        } else {
            let store = self.registry.live_store(&self.project.id);
            let s = store.read();
            s.readings.clone()
        };
        raw.into_iter()
            .filter(|r| {
                self.grant
                    .scope
                    .allows_asset(&r.asset_id, &self.asset_category(&r.asset_id))
            })
            .collect()
    }
}

#[derive(SimpleObject)]
struct GqlReading {
    asset_id: String,
    sensor_id: String,
    parameter: String,
    value: f64,
    unit: String,
    ts: i64,
    band: String,
}

impl From<TelemetryReading> for GqlReading {
    fn from(r: TelemetryReading) -> Self {
        Self {
            asset_id: r.asset_id,
            sensor_id: r.sensor_id,
            parameter: r.parameter,
            value: r.value,
            unit: r.unit,
            ts: r.ts,
            band: r.band,
        }
    }
}

#[derive(SimpleObject)]
struct GqlDiagnosis {
    asset_id: String,
    agent_id: String,
    diagnosis: String,
    recommendation: String,
    confidence: f64,
    ts: i64,
}

#[derive(SimpleObject)]
struct GqlApproval {
    subject: String,
    approved_by: String,
    note: String,
    ts: i64,
}

#[derive(SimpleObject)]
struct GqlProjectCard {
    id: String,
    name: String,
    country: String,
    region: String,
    status: String,
    simulation: bool,
    asset_count: i32,
    sensor_count: i32,
}

#[derive(SimpleObject)]
struct GqlOverview {
    readings_in_scope: i32,
    ok: i32,
    warning: i32,
    critical: i32,
    latest_ts: Option<i64>,
    sandbox: bool,
}

pub struct QueryRoot;

#[Object]
impl QueryRoot {
    /// The public project card the grant belongs to.
    async fn project(&self, ctx: &Context<'_>) -> async_graphql::Result<GqlProjectCard> {
        let s = ctx.data::<Arc<GqlSession>>()?;
        let def = s.project.definition.as_ref();
        Ok(GqlProjectCard {
            id: s.project.id.clone(),
            name: s.project.name(),
            country: def.map(|d| d.country.clone()).unwrap_or_default(),
            region: def.map(|d| d.region.clone()).unwrap_or_default(),
            status: format!("{:?}", s.project.status),
            simulation: s.project.simulation,
            asset_count: s.project.inventory.assets.len() as i32,
            sensor_count: s.project.inventory.sensors.len() as i32,
        })
    }

    /// Band counts over the in-scope readings.
    async fn overview(&self, ctx: &Context<'_>) -> async_graphql::Result<GqlOverview> {
        let s = ctx.data::<Arc<GqlSession>>()?;
        let scoped = s.scoped_readings();
        Ok(GqlOverview {
            readings_in_scope: scoped.len() as i32,
            ok: scoped.iter().filter(|r| r.band == "ok").count() as i32,
            warning: scoped.iter().filter(|r| r.band == "warning").count() as i32,
            critical: scoped.iter().filter(|r| r.band == "critical").count() as i32,
            latest_ts: scoped.iter().map(|r| r.ts).max(),
            sandbox: s.sandbox,
        })
    }

    /// In-scope telemetry, newest first.
    async fn readings(
        &self,
        ctx: &Context<'_>,
        since: Option<i64>,
        parameter: Option<String>,
        asset_id: Option<String>,
        limit: Option<i32>,
    ) -> async_graphql::Result<Vec<GqlReading>> {
        let s = ctx.data::<Arc<GqlSession>>()?;
        if !s.grant.scope.allows_facet("readings") {
            return Err("grant scope does not include readings".into());
        }
        let limit = limit.unwrap_or(1_000).clamp(1, 5_000) as usize;
        let mut out: Vec<GqlReading> = s
            .scoped_readings()
            .into_iter()
            .filter(|r| since.map(|t| r.ts >= t).unwrap_or(true))
            .filter(|r| parameter.as_ref().map(|p| &r.parameter == p).unwrap_or(true))
            .filter(|r| asset_id.as_ref().map(|a| &r.asset_id == a).unwrap_or(true))
            .map(GqlReading::from)
            .collect();
        out.sort_by_key(|r| std::cmp::Reverse(r.ts));
        out.truncate(limit);
        Ok(out)
    }

    /// In-scope AI diagnoses, newest first. Empty for sandbox sessions
    /// (diagnoses are produced by real agents against real telemetry).
    async fn diagnoses(
        &self,
        ctx: &Context<'_>,
        limit: Option<i32>,
    ) -> async_graphql::Result<Vec<GqlDiagnosis>> {
        let s = ctx.data::<Arc<GqlSession>>()?;
        if !s.grant.scope.allows_facet("diagnoses") {
            return Err("grant scope does not include diagnoses".into());
        }
        if s.sandbox {
            return Ok(Vec::new());
        }
        let limit = limit.unwrap_or(200).clamp(1, 1_000) as usize;
        let store = s.registry.live_store(&s.project.id);
        let items: Vec<GqlDiagnosis> = {
            let st = store.read();
            st.diagnoses
                .iter()
                .filter(|d| {
                    s.grant
                        .scope
                        .allows_asset(&d.asset_id, &s.asset_category(&d.asset_id))
                })
                .rev()
                .take(limit)
                .map(|d| GqlDiagnosis {
                    asset_id: d.asset_id.clone(),
                    agent_id: d.agent_id.clone(),
                    diagnosis: d.diagnosis.clone(),
                    recommendation: d.recommendation.clone(),
                    confidence: d.confidence,
                    ts: d.ts,
                })
                .collect()
        };
        Ok(items)
    }

    /// Governance approvals, newest first. Empty for sandbox sessions.
    async fn approvals(
        &self,
        ctx: &Context<'_>,
        limit: Option<i32>,
    ) -> async_graphql::Result<Vec<GqlApproval>> {
        let s = ctx.data::<Arc<GqlSession>>()?;
        if !s.grant.scope.allows_facet("approvals") {
            return Err("grant scope does not include approvals".into());
        }
        if s.sandbox {
            return Ok(Vec::new());
        }
        let limit = limit.unwrap_or(200).clamp(1, 1_000) as usize;
        let store = s.registry.live_store(&s.project.id);
        let items: Vec<GqlApproval> = {
            let st = store.read();
            st.approvals
                .iter()
                .rev()
                .take(limit)
                .map(|a| GqlApproval {
                    subject: a.subject.clone(),
                    approved_by: a.approved_by.clone(),
                    note: a.note.clone(),
                    ts: a.ts,
                })
                .collect()
        };
        Ok(items)
    }
}

/// Execute one GraphQL request under an authenticated session.
pub async fn execute(
    session: GqlSession,
    request: async_graphql::Request,
) -> async_graphql::Response {
    let schema = Schema::build(QueryRoot, EmptyMutation, EmptySubscription)
        .data(Arc::new(session))
        .limit_depth(8)
        .limit_complexity(512)
        .finish();
    schema.execute(request).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grants::{GrantPrice, GrantScope, Grantee, StakeholderClass};
    use crate::types::Project;

    fn session(sandbox: bool) -> (GqlSession, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let registry = Registry::open(dir.path()).unwrap();
        let mut project = Project::new("GraphQL test", "0xowner");
        crate::simulation::apply_template(&mut project, "agri_estate");
        let pid = project.id.clone();
        registry.insert_project(project.clone());
        if !sandbox {
            let now = now_ts();
            for i in 0..5 {
                registry.push_reading(TelemetryReading {
                    project_id: pid.clone(),
                    asset_id: project.inventory.sensors[0].parent_asset_id.clone(),
                    sensor_id: project.inventory.sensors[0].id.clone(),
                    parameter: project.inventory.sensors[0].parameter.clone(),
                    value: 40.0,
                    unit: "%".into(),
                    ts: now - 100 + i,
                    band: "ok".into(),
                    anchor: String::new(),
                });
            }
        }
        let grant = AccessGrant::new(
            &pid,
            Grantee { kind: "public".into(), value: "*".into() },
            StakeholderClass::Investor,
            GrantScope::default(),
            0,
            0,
            GrantPrice::default(),
            vec!["rest".into()],
            "0xowner",
            0,
        );
        (GqlSession { registry, grant, project, sandbox }, dir)
    }

    #[tokio::test]
    async fn query_overview_and_readings_live() {
        let (s, _dir) = session(false);
        let resp = execute(
            s,
            async_graphql::Request::new(
                "{ overview { readingsInScope sandbox } readings(limit: 3) { value band } }",
            ),
        )
        .await;
        assert!(resp.errors.is_empty(), "{:?}", resp.errors);
        let data = resp.data.into_json().unwrap();
        assert_eq!(data["overview"]["readingsInScope"], 5);
        assert_eq!(data["overview"]["sandbox"], false);
        assert_eq!(data["readings"].as_array().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn sandbox_serves_synthetic() {
        let (s, _dir) = session(true);
        let resp = execute(
            s,
            async_graphql::Request::new("{ overview { readingsInScope sandbox } }"),
        )
        .await;
        assert!(resp.errors.is_empty(), "{:?}", resp.errors);
        let data = resp.data.into_json().unwrap();
        assert_eq!(data["overview"]["sandbox"], true);
        // Synthetic history: 120 points per declared sensor.
        assert!(data["overview"]["readingsInScope"].as_i64().unwrap() > 0);
    }

    #[tokio::test]
    async fn scope_enforced_in_graphql() {
        let (mut s, _dir) = session(false);
        s.grant.scope = GrantScope {
            facets: vec!["readings".into()],
            asset_ids: vec![],
            categories: vec![],
        };
        let resp = execute(
            s,
            async_graphql::Request::new("{ approvals { subject } }"),
        )
        .await;
        assert!(!resp.errors.is_empty());
        assert!(resp.errors[0].message.contains("approvals"));
    }
}
