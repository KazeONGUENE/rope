//! Project registry — persistent store + on-chain anchoring.
//!
//! Persistence model: one JSON store file, written atomically
//! (tmp + rename) on every mutation. Project counts per node are small
//! (one node typically hosts one project, at most a handful), so a full
//! rewrite is both simpler and more robust than incremental formats.
//!
//! On-chain model (best-effort, same pattern as the production
//! node-request queue in `rope-explorer`):
//!
//! * project genesis + lifecycle events → knots on the **project's own
//!   string** (`Project.wallet`),
//! * the public project card → `EcosystemProjectRegistered` knot on the
//!   **well-known registry wallet** (`EDC_REGISTRY_WALLET`, default
//!   `0x…ec01`) that dcscan.io reads to auto-list every project,
//! * grants → `AccessGrantIssued` / `AccessGrantRevoked` knots on the
//!   project's string.
//!
//! The local store is the read cache; the chain is the durable,
//! replicated, auditable source that a fresh node can rebuild from.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use crate::grants::{AccessGrant, ApiKeyRecord};
use crate::types::{
    now_ts, ApprovalEvent, DiagnosisEvent, Project, ReportRecord, TelemetryReading,
};

/// Well-known wallet whose personal-ledger string is the public
/// ecosystem-project directory consumed by dcscan.io.
pub fn registry_wallet() -> String {
    std::env::var("EDC_REGISTRY_WALLET")
        .unwrap_or_else(|_| "0x000000000000000000000000000000000000ec01".to_string())
}

/// Loopback rope-node RPC endpoint (V11-internal path).
pub fn rope_rpc_url() -> String {
    std::env::var("EDC_ROPE_RPC").unwrap_or_else(|_| "http://127.0.0.1:8545".to_string())
}

/// Maximum readings kept in the hot in-memory ring per project.
const TELEMETRY_RING_CAP: usize = 5_000;
/// Maximum diagnosis / approval events kept per project.
const EVENTS_CAP: usize = 1_000;

#[derive(Debug, Default, Serialize, Deserialize)]
struct StoreFile {
    projects: Vec<Project>,
    grants: Vec<AccessGrant>,
    keys: Vec<ApiKeyRecord>,
}

/// Live per-project telemetry + event store (ring-buffered in memory,
/// mirrored to a JSONL journal so history survives restarts).
#[derive(Default)]
pub struct LiveStore {
    pub readings: Vec<TelemetryReading>,
    pub diagnoses: Vec<DiagnosisEvent>,
    pub approvals: Vec<ApprovalEvent>,
    pub reports: Vec<ReportRecord>,
}

pub struct Registry {
    path: PathBuf,
    journal_dir: PathBuf,
    exports_root: PathBuf,
    projects: RwLock<HashMap<String, Project>>,
    grants: RwLock<HashMap<String, AccessGrant>>,
    keys: RwLock<HashMap<String, ApiKeyRecord>>, // by token digest
    live: RwLock<HashMap<String, Arc<RwLock<LiveStore>>>>,
    http: reqwest::Client,
}

impl Registry {
    /// Open (or create) the registry at `dir`.
    pub fn open(dir: impl Into<PathBuf>) -> anyhow::Result<Arc<Self>> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)?;
        let journal_dir = dir.join("journal");
        std::fs::create_dir_all(&journal_dir)?;
        let exports_root = dir.join("exports");
        std::fs::create_dir_all(&exports_root)?;
        let path = dir.join("edc-store.json");

        let file: StoreFile = if path.exists() {
            let raw = std::fs::read_to_string(&path)?;
            serde_json::from_str(&raw).unwrap_or_default()
        } else {
            StoreFile::default()
        };

        let reg = Arc::new(Self {
            path,
            journal_dir,
            exports_root,
            projects: RwLock::new(
                file.projects.into_iter().map(|p| (p.id.clone(), p)).collect(),
            ),
            grants: RwLock::new(
                file.grants.into_iter().map(|g| (g.id.clone(), g)).collect(),
            ),
            keys: RwLock::new(
                file.keys
                    .into_iter()
                    .map(|k| (k.token_digest.clone(), k))
                    .collect(),
            ),
            live: RwLock::new(HashMap::new()),
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("reqwest client"),
        });
        reg.replay_journals();
        Ok(reg)
    }

    // -- persistence --------------------------------------------------------

    fn persist(&self) {
        let file = StoreFile {
            projects: self.projects.read().values().cloned().collect(),
            grants: self.grants.read().values().cloned().collect(),
            keys: self.keys.read().values().cloned().collect(),
        };
        let tmp = self.path.with_extension("json.tmp");
        match serde_json::to_vec_pretty(&file) {
            Ok(bytes) => {
                if let Err(e) = std::fs::write(&tmp, &bytes)
                    .and_then(|_| std::fs::rename(&tmp, &self.path))
                {
                    tracing::error!("edc store persist failed: {e}");
                }
            }
            Err(e) => tracing::error!("edc store serialize failed: {e}"),
        }
    }

    fn journal_path(&self, project_id: &str) -> PathBuf {
        self.journal_dir.join(format!("{project_id}.jsonl"))
    }

    /// Directory holding the scheduled bulk-export extracts for a grant.
    pub fn exports_dir(&self, grant_id: &str) -> PathBuf {
        self.exports_root.join(grant_id)
    }

    fn journal_append(&self, project_id: &str, kind: &str, value: &serde_json::Value) {
        let line = serde_json::json!({"kind": kind, "v": value}).to_string();
        let path = self.journal_path(project_id);
        use std::io::Write;
        match std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            Ok(mut f) => {
                let _ = writeln!(f, "{line}");
            }
            Err(e) => tracing::error!("journal append failed for {project_id}: {e}"),
        }
    }

    /// Rebuild the in-memory live stores from the per-project journals.
    fn replay_journals(&self) {
        let ids: Vec<String> = self.projects.read().keys().cloned().collect();
        for id in ids {
            let path = self.journal_path(&id);
            let Ok(raw) = std::fs::read_to_string(&path) else {
                continue;
            };
            let store = self.live_store(&id);
            let mut s = store.write();
            for line in raw.lines() {
                let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                    continue;
                };
                match v.get("kind").and_then(|k| k.as_str()) {
                    Some("reading") => {
                        if let Ok(r) = serde_json::from_value::<TelemetryReading>(
                            v["v"].clone(),
                        ) {
                            s.readings.push(r);
                        }
                    }
                    Some("diagnosis") => {
                        if let Ok(d) =
                            serde_json::from_value::<DiagnosisEvent>(v["v"].clone())
                        {
                            s.diagnoses.push(d);
                        }
                    }
                    Some("approval") => {
                        if let Ok(a) =
                            serde_json::from_value::<ApprovalEvent>(v["v"].clone())
                        {
                            s.approvals.push(a);
                        }
                    }
                    Some("report") => {
                        if let Ok(r) =
                            serde_json::from_value::<ReportRecord>(v["v"].clone())
                        {
                            s.reports.push(r);
                        }
                    }
                    _ => {}
                }
            }
            let excess = s.readings.len().saturating_sub(TELEMETRY_RING_CAP);
            if excess > 0 {
                s.readings.drain(..excess);
            }
            let excess = s.diagnoses.len().saturating_sub(EVENTS_CAP);
            if excess > 0 {
                s.diagnoses.drain(..excess);
            }
            let excess = s.approvals.len().saturating_sub(EVENTS_CAP);
            if excess > 0 {
                s.approvals.drain(..excess);
            }
            let excess = s.reports.len().saturating_sub(EVENTS_CAP);
            if excess > 0 {
                s.reports.drain(..excess);
            }
        }
    }

    // -- projects -----------------------------------------------------------

    pub fn insert_project(&self, project: Project) {
        self.projects.write().insert(project.id.clone(), project);
        self.persist();
    }

    pub fn update_project<F: FnOnce(&mut Project)>(
        &self,
        id: &str,
        f: F,
    ) -> Option<Project> {
        let updated = {
            let mut map = self.projects.write();
            let p = map.get_mut(id)?;
            f(p);
            p.updated_at = now_ts();
            Some(p.clone())
        };
        if updated.is_some() {
            self.persist();
        }
        updated
    }

    pub fn get_project(&self, id: &str) -> Option<Project> {
        self.projects.read().get(id).cloned()
    }

    pub fn list_projects(&self) -> Vec<Project> {
        let mut v: Vec<Project> = self.projects.read().values().cloned().collect();
        v.sort_by_key(|p| std::cmp::Reverse(p.created_at));
        v
    }

    // -- grants & keys ------------------------------------------------------

    pub fn insert_grant(&self, grant: AccessGrant) {
        self.grants.write().insert(grant.id.clone(), grant);
        self.persist();
    }

    pub fn update_grant<F: FnOnce(&mut AccessGrant)>(
        &self,
        id: &str,
        f: F,
    ) -> Option<AccessGrant> {
        let updated = {
            let mut map = self.grants.write();
            let g = map.get_mut(id)?;
            f(g);
            Some(g.clone())
        };
        if updated.is_some() {
            self.persist();
        }
        updated
    }

    pub fn get_grant(&self, id: &str) -> Option<AccessGrant> {
        self.grants.read().get(id).cloned()
    }

    pub fn grants_for_project(&self, project_id: &str) -> Vec<AccessGrant> {
        let mut v: Vec<AccessGrant> = self
            .grants
            .read()
            .values()
            .filter(|g| g.project_id == project_id)
            .cloned()
            .collect();
        v.sort_by_key(|g| std::cmp::Reverse(g.created_at));
        v
    }

    pub fn insert_key(&self, key: ApiKeyRecord) {
        self.keys.write().insert(key.token_digest.clone(), key);
        self.persist();
    }

    /// Resolve a presented bearer token to its (usable) grant plus the
    /// key's sandbox flag. Increments the metering counters on success —
    /// sandbox traffic is deliberately NOT metered (spec v1.0 §6.3: the
    /// sandbox exists to validate an integration before it bills).
    pub fn authorize_token(&self, token: &str) -> Option<(AccessGrant, bool)> {
        let digest = crate::grants::token_digest(token);
        let (grant_id, sandbox) = {
            let keys = self.keys.read();
            let rec = keys.get(&digest)?;
            if rec.revoked {
                return None;
            }
            (rec.grant_id.clone(), rec.sandbox)
        };
        let now = now_ts();
        let grant = {
            let grants = self.grants.read();
            let g = grants.get(&grant_id)?;
            if !g.is_usable(now) {
                return None;
            }
            g.clone()
        };
        if sandbox {
            return Some((grant, true));
        }
        // Metering (spec v2.0 §5.3) — counted on every authorized request.
        let metered = self.update_grant(&grant_id, |g| {
            g.calls += 1;
            g.last_used_at = now;
        });
        Some((metered.unwrap_or(grant), false))
    }

    /// Resolve a verified stakeholder wallet (EIP-191 wallet-signature
    /// auth, spec v1.0 §6.3) to its usable grant. The grant must name the
    /// wallet directly (`grantee.kind == "wallet"`). Metered like any
    /// bearer-token request.
    pub fn authorize_wallet(&self, wallet: &str) -> Option<AccessGrant> {
        let w = wallet.to_lowercase();
        let now = now_ts();
        let grant = {
            let grants = self.grants.read();
            grants
                .values()
                .filter(|g| {
                    g.grantee.kind == "wallet"
                        && g.grantee.value.to_lowercase() == w
                        && g.is_usable(now)
                })
                // Deterministic pick: the most recently created usable grant.
                .max_by_key(|g| g.created_at)
                .cloned()?
        };
        let gid = grant.id.clone();
        let metered = self.update_grant(&gid, |g| {
            g.calls += 1;
            g.last_used_at = now;
        });
        Some(metered.unwrap_or(grant))
    }

    /// Revoke a grant and every key minted from it.
    pub fn revoke_grant(&self, grant_id: &str) -> Option<AccessGrant> {
        let g = self.update_grant(grant_id, |g| {
            g.status = crate::grants::GrantStatus::Revoked;
            g.revoked_at = now_ts();
        })?;
        {
            let mut keys = self.keys.write();
            for k in keys.values_mut() {
                if k.grant_id == grant_id {
                    k.revoked = true;
                }
            }
        }
        self.persist();
        Some(g)
    }

    // -- live facet data ----------------------------------------------------

    pub fn live_store(&self, project_id: &str) -> Arc<RwLock<LiveStore>> {
        let mut map = self.live.write();
        map.entry(project_id.to_string())
            .or_insert_with(|| Arc::new(RwLock::new(LiveStore::default())))
            .clone()
    }

    pub fn push_reading(&self, reading: TelemetryReading) {
        self.journal_append(
            &reading.project_id,
            "reading",
            &serde_json::to_value(&reading).unwrap_or_default(),
        );
        let store = self.live_store(&reading.project_id);
        let mut s = store.write();
        s.readings.push(reading);
        if s.readings.len() > TELEMETRY_RING_CAP {
            let excess = s.readings.len() - TELEMETRY_RING_CAP;
            s.readings.drain(..excess);
        }
    }

    pub fn push_diagnosis(&self, ev: DiagnosisEvent) {
        self.journal_append(
            &ev.project_id,
            "diagnosis",
            &serde_json::to_value(&ev).unwrap_or_default(),
        );
        let store = self.live_store(&ev.project_id);
        let mut s = store.write();
        s.diagnoses.push(ev);
        if s.diagnoses.len() > EVENTS_CAP {
            let excess = s.diagnoses.len() - EVENTS_CAP;
            s.diagnoses.drain(..excess);
        }
    }

    pub fn push_approval(&self, ev: ApprovalEvent) {
        self.journal_append(
            &ev.project_id,
            "approval",
            &serde_json::to_value(&ev).unwrap_or_default(),
        );
        let store = self.live_store(&ev.project_id);
        let mut s = store.write();
        s.approvals.push(ev);
        if s.approvals.len() > EVENTS_CAP {
            let excess = s.approvals.len() - EVENTS_CAP;
            s.approvals.drain(..excess);
        }
    }

    pub fn push_report(&self, report: ReportRecord) {
        self.journal_append(
            &report.project_id,
            "report",
            &serde_json::to_value(&report).unwrap_or_default(),
        );
        let store = self.live_store(&report.project_id);
        let mut s = store.write();
        s.reports.push(report);
        if s.reports.len() > EVENTS_CAP {
            let excess = s.reports.len() - EVENTS_CAP;
            s.reports.drain(..excess);
        }
    }

    /// All grants across every project — used by the export/billing
    /// schedulers.
    pub fn all_grants(&self) -> Vec<AccessGrant> {
        self.grants.read().values().cloned().collect()
    }

    // -- on-chain anchoring (best-effort, loopback rope-node) ----------------

    async fn rope_call(&self, body: serde_json::Value) -> Option<serde_json::Value> {
        let rpc = rope_rpc_url();
        match self.http.post(&rpc).json(&body).send().await {
            Ok(resp) => resp.json::<serde_json::Value>().await.ok(),
            Err(e) => {
                tracing::warn!("rope-node unreachable at {rpc}: {e}");
                None
            }
        }
    }

    /// Idempotently create a personal ledger for `wallet`.
    pub async fn ensure_ledger(&self, wallet: &str) {
        let body = serde_json::json!({
            "jsonrpc": "2.0", "id": 1,
            "method": "rope_createPersonalLedger",
            "params": [wallet],
        });
        let _ = self.rope_call(body).await;
    }

    /// Anchor an interaction knot on `wallet`'s string. Returns the knot
    /// hash on success. Best-effort by design: local persistence already
    /// succeeded before this runs.
    pub async fn anchor(
        &self,
        wallet: &str,
        interaction_type: &str,
        description: String,
        metadata: serde_json::Value,
    ) -> Option<String> {
        self.ensure_ledger(wallet).await;
        let body = serde_json::json!({
            "jsonrpc": "2.0", "id": 2,
            "method": "rope_appendToLedger",
            "params": [wallet, {
                "interaction_type": interaction_type,
                "description": description,
                "metadata": metadata,
            }],
        });
        let resp = self.rope_call(body).await?;
        let hash = resp
            .get("result")
            .and_then(|r| r.get("hash"))
            .and_then(|h| h.as_str())
            .map(|s| s.to_string());
        if hash.is_none() {
            tracing::warn!("anchor rejected by rope-node: {resp}");
        }
        hash
    }

    /// Anchor the public project card on the registry wallet so dcscan.io
    /// auto-lists the project (spec v2.0 §8).
    pub async fn anchor_public_card(&self, project: &Project) -> Option<String> {
        let card = project.public_card();
        self.anchor(
            &registry_wallet(),
            "EcosystemProjectRegistered",
            card.to_string(),
            serde_json::json!({
                "project_id": project.id,
                "name": project.name(),
                "status": serde_json::to_value(project.status).unwrap_or_default(),
                "wallet": project.wallet,
            }),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grants::{mint_key, AccessGrant, GrantPrice, GrantScope, Grantee, StakeholderClass};

    fn temp_registry() -> (Arc<Registry>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let reg = Registry::open(dir.path()).unwrap();
        (reg, dir)
    }

    #[test]
    fn project_crud_and_persistence() {
        let dir = tempfile::tempdir().unwrap();
        {
            let reg = Registry::open(dir.path()).unwrap();
            let p = Project::new("Den Haag EAM", "0xowner");
            let id = p.id.clone();
            reg.insert_project(p);
            reg.update_project(&id, |p| {
                if let Some(d) = p.definition.as_mut() {
                    d.country = "NL".to_string();
                }
            });
        }
        // Re-open: state survives.
        let reg = Registry::open(dir.path()).unwrap();
        let all = reg.list_projects();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].definition.as_ref().unwrap().country, "NL");
    }

    #[test]
    fn token_authorization_and_metering() {
        let (reg, _dir) = temp_registry();
        let g = AccessGrant::new(
            "prj_1",
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
        let gid = g.id.clone();
        reg.insert_grant(g);
        let (rec, token) = mint_key(&gid, "test", false);
        reg.insert_key(rec);

        let (authed, sandbox) =
            reg.authorize_token(&token).expect("token should authorize");
        assert_eq!(authed.id, gid);
        assert!(!sandbox);
        // Metering incremented.
        assert_eq!(reg.get_grant(&gid).unwrap().calls, 1);
        // Wrong token fails.
        assert!(reg.authorize_token("edc_deadbeef").is_none());
    }

    #[test]
    fn sandbox_token_not_metered() {
        let (reg, _dir) = temp_registry();
        let g = AccessGrant::new(
            "prj_1",
            Grantee { kind: "wallet".into(), value: "0xabc".into() },
            StakeholderClass::Investor,
            GrantScope::default(),
            0,
            0,
            GrantPrice::default(),
            vec!["rest".into()],
            "0xowner",
            0,
        );
        let gid = g.id.clone();
        reg.insert_grant(g);
        let (rec, token) = mint_key(&gid, "sandbox", true);
        reg.insert_key(rec);

        let (_, sandbox) = reg.authorize_token(&token).expect("must authorize");
        assert!(sandbox);
        // Sandbox calls never bill against the grant meter.
        assert_eq!(reg.get_grant(&gid).unwrap().calls, 0);
    }

    #[test]
    fn wallet_signature_grant_resolution() {
        let (reg, _dir) = temp_registry();
        let g = AccessGrant::new(
            "prj_1",
            Grantee { kind: "wallet".into(), value: "0xAbCd000000000000000000000000000000000001".into() },
            StakeholderClass::Regulator,
            GrantScope::default(),
            0,
            0,
            GrantPrice::default(),
            vec!["rest".into()],
            "0xowner",
            0, // no timelock delay in test
        );
        let gid = g.id.clone();
        reg.insert_grant(g);

        // Case-insensitive match, metered.
        let hit = reg
            .authorize_wallet("0xabcd000000000000000000000000000000000001")
            .expect("wallet grant must resolve");
        assert_eq!(hit.id, gid);
        assert_eq!(reg.get_grant(&gid).unwrap().calls, 1);
        // Unknown wallet fails.
        assert!(reg.authorize_wallet("0x00000000000000000000000000000000000000ff").is_none());
    }

    #[test]
    fn revoking_grant_kills_keys() {
        let (reg, _dir) = temp_registry();
        let g = AccessGrant::new(
            "prj_1",
            Grantee { kind: "wallet".into(), value: "0xabc".into() },
            StakeholderClass::CommercialBuyer,
            GrantScope::default(),
            0,
            0,
            GrantPrice { model: "metered".into(), amount: 0.1, currency: "FAT".into(), period: String::new() },
            vec!["rest".into()],
            "0xowner",
            0,
        );
        let gid = g.id.clone();
        reg.insert_grant(g);
        let (rec, token) = mint_key(&gid, "buyer", false);
        reg.insert_key(rec);
        assert!(reg.authorize_token(&token).is_some());

        reg.revoke_grant(&gid);
        assert!(reg.authorize_token(&token).is_none());
    }

    #[test]
    fn telemetry_journal_replay() {
        let dir = tempfile::tempdir().unwrap();
        let pid;
        {
            let reg = Registry::open(dir.path()).unwrap();
            let p = Project::new("Sensors", "0xowner");
            pid = p.id.clone();
            reg.insert_project(p);
            reg.push_reading(TelemetryReading {
                project_id: pid.clone(),
                asset_id: "a1".into(),
                sensor_id: "s1".into(),
                parameter: "soil_moisture".into(),
                value: 42.0,
                unit: "%".into(),
                ts: now_ts(),
                band: "ok".into(),
                anchor: String::new(),
            });
        }
        let reg = Registry::open(dir.path()).unwrap();
        let store = reg.live_store(&pid);
        assert_eq!(store.read().readings.len(), 1);
        assert_eq!(store.read().readings[0].value, 42.0);
    }
}
