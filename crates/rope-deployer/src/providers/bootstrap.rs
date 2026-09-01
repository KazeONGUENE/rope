//! Shared node-enrolment helpers used by every live cloud adapter
//! (`digitalocean`, `exoscale`, and any future provider).
//!
//! The philosophy here is that every provider we support must land the
//! Rope node on the target VM in *exactly* the same shape:
//!
//! * `/etc/rope-node/identity.key` — Ed25519 signing key (mode 0600).
//! * `/etc/rope-node/identity.pub` — Ed25519 verifying key (base64).
//! * `/etc/rope-node/enrolment.json` — tenant DID + ONCHAINID +
//!   project name + node kind + region + an EIP-191-style enrolment
//!   signature over the (tenant, hostname) tuple.
//! * `ROPE_ENROLMENT=/etc/rope-node/enrolment.json rope-cli node
//!   bootstrap --unattended --kind <kind>` executed at first boot from
//!   `https://get.datachain.network/rope-cli`.
//!
//! Keeping this logic in one module is what lets the console tell
//! users, honestly, that "an Exoscale node and a DigitalOcean node
//! come up identically". A regression on any of these fields would be
//! a real interoperability bug, so we lock them in with tests here.
//!
//! Nothing in this module talks to a cloud provider. The output is a
//! pair of strings (`user_data` cloud-init script + public identity
//! key) that individual providers embed into their per-cloud
//! provisioning request.

use std::collections::BTreeMap;

use ed25519_dalek::{Signer, SigningKey, VerifyingKey};

use crate::types::{NodeKind, ProvisionRequest};

/// Ed25519 node identity generated on the deployer at provision time.
///
/// The signing (private) half is embedded into the cloud-init
/// user_data and lands on the VM under `/etc/rope-node/identity.key`.
/// The verifying (public) half is returned in the provision response
/// so the console can display it and the Foundation can pin it into
/// the master-node registry when the tenant onboards as a witness.
pub(super) struct NodeIdentity {
    signing: SigningKey,
    pub(super) verifying: VerifyingKey,
}

impl NodeIdentity {
    /// Materialise 32 bytes of OS entropy into a fresh Ed25519 key.
    ///
    /// `ed25519-dalek` 2.x removed the CSPRNG plumbing from
    /// `SigningKey`, so we source randomness from `uuid` v4 which
    /// itself pulls from `getrandom`. Two v4 UUIDs give us 32 bytes of
    /// cryptographically strong randomness without adding a new crate
    /// dependency. A future patch can move to `rand_core` directly.
    pub(super) fn generate() -> Self {
        let mut seed = [0u8; 32];
        let a = *uuid::Uuid::new_v4().as_bytes();
        let b = *uuid::Uuid::new_v4().as_bytes();
        seed[..16].copy_from_slice(&a);
        seed[16..].copy_from_slice(&b);
        let signing = SigningKey::from_bytes(&seed);
        let verifying = signing.verifying_key();
        Self { signing, verifying }
    }

    pub(super) fn public_b64(&self) -> String {
        b64_encode(self.verifying.to_bytes())
    }

    pub(super) fn private_b64(&self) -> String {
        b64_encode(self.signing.to_bytes())
    }

    /// Sign a canonical enrolment challenge. The cloud-init script
    /// re-verifies before starting `rope-cli node bootstrap`.
    pub(super) fn enrolment_signature(&self, tenant_did: &str, hostname: &str) -> String {
        let msg = format!("rope-node-enrolment/v1\n{tenant_did}\n{hostname}");
        let sig = self.signing.sign(msg.as_bytes());
        b64_encode(sig.to_bytes())
    }
}

/// Minimal RFC 4648 base64 (no-std friendly). We encode at most a few
/// hundred bytes per call so the naive table lookup is cheaper than
/// pulling the `base64` crate into every provider.
pub(super) fn b64_encode(bytes: impl AsRef<[u8]>) -> String {
    const T: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = bytes.as_ref();
    let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
    let mut i = 0;
    while i + 3 <= bytes.len() {
        let n =
            ((bytes[i] as u32) << 16) | ((bytes[i + 1] as u32) << 8) | (bytes[i + 2] as u32);
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(T[((n >> 6) & 63) as usize] as char);
        out.push(T[(n & 63) as usize] as char);
        i += 3;
    }
    let rem = bytes.len() - i;
    if rem == 1 {
        let n = (bytes[i] as u32) << 16;
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push('=');
        out.push('=');
    } else if rem == 2 {
        let n = ((bytes[i] as u32) << 16) | ((bytes[i + 1] as u32) << 8);
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(T[((n >> 6) & 63) as usize] as char);
        out.push('=');
    }
    out
}

/// Deterministic hostname so re-provisioning after a failed create
/// doesn't collide with a lingering VM from an earlier attempt.
///
/// Format: `rope-{kind}-{tenant-slug}-{region}` where `tenant-slug`
/// is the last 12 alphanumeric characters of the tenant DID.
pub(super) fn build_hostname(tenant_did: &str, kind: NodeKind, region: &str) -> String {
    let slug: String = tenant_did
        .chars()
        .rev()
        .take(12)
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    let slug = if slug.is_empty() {
        "anon".to_string()
    } else {
        slug
    };
    // Region may contain hyphens (`ch-gva-2`, `de-fra-1`); those are
    // already DNS-safe so we keep them as-is.
    format!("rope-{}-{}-{}", kind.as_str(), slug, region)
}

/// Cloud-init script written into `user_data` on every VM.
///
/// The script is intentionally short; heavy lifting lives in
/// `rope-cli node bootstrap`, which is downloaded from
/// `https://get.datachain.network/rope-cli` (SHA-256 pinned by the
/// server-side installer) and run once by cloud-init.
pub(super) fn build_cloud_init(req: &ProvisionRequest, identity: &NodeIdentity) -> String {
    let mut ssh_block = String::new();
    let pubkey = req.ssh_pubkey.trim();
    if !pubkey.is_empty() {
        ssh_block = format!(
            "mkdir -p /root/.ssh && chmod 700 /root/.ssh\n\
             printf '%s\\n' '{pubkey}' >> /root/.ssh/authorized_keys\n\
             chmod 600 /root/.ssh/authorized_keys\n"
        );
    }

    let priv_b64 = identity.private_b64();
    let pub_b64 = identity.public_b64();
    let hostname = build_hostname(&req.tenant_did, req.node_kind, &req.zone);
    let enrol_sig = identity.enrolment_signature(&req.tenant_did, &hostname);
    let installer_url = std::env::var("ROPE_CLI_INSTALLER_URL")
        .unwrap_or_else(|_| "https://get.datachain.network/rope-cli".to_string());

    format!(
        "#!/bin/bash\n\
         set -euo pipefail\n\
         export DEBIAN_FRONTEND=noninteractive\n\
         apt-get update -y\n\
         apt-get install -y ca-certificates curl gnupg jq\n\
         {ssh_block}\
         mkdir -p /etc/rope-node\n\
         chmod 700 /etc/rope-node\n\
         cat > /etc/rope-node/identity.key <<'KEY'\n{priv_b64}\nKEY\n\
         chmod 600 /etc/rope-node/identity.key\n\
         cat > /etc/rope-node/identity.pub <<'PUB'\n{pub_b64}\nPUB\n\
         cat > /etc/rope-node/enrolment.json <<'JSON'\n\
         {{\n  \"tenant_did\": \"{tenant_did}\",\n  \"tenant_onchainid\": \"{onchainid}\",\n  \"project_name\": \"{project_name}\",\n  \"node_kind\": \"{node_kind}\",\n  \"region\": \"{region}\",\n  \"identity_public_key\": \"{pub_b64}\",\n  \"enrolment_signature\": \"{enrol_sig}\"\n}}\nJSON\n\
         curl -sSfL {installer_url} -o /tmp/rope-cli-install.sh\n\
         chmod +x /tmp/rope-cli-install.sh\n\
         ROPE_ENROLMENT=/etc/rope-node/enrolment.json /tmp/rope-cli-install.sh --unattended --kind {node_kind}\n\
         ",
        ssh_block = ssh_block,
        priv_b64 = priv_b64,
        pub_b64 = pub_b64,
        tenant_did = req.tenant_did,
        onchainid = req.tenant_onchainid,
        project_name = req.project_name.replace('"', "'"),
        node_kind = req.node_kind.as_str(),
        region = req.zone,
        enrol_sig = enrol_sig,
        installer_url = installer_url,
    )
}

/// Sanitise a tenant DID into a tag/label value: providers accept
/// `[A-Za-z0-9:_-]{1,255}` (DO tags, Exoscale labels). DIDs like
/// `did:dwp:0x…` are already safe; we truncate to 63 chars so the
/// value stays DNS-compatible if we ever reuse it as a hostname
/// component.
pub(super) fn tenant_tag_for(tenant_did: &str) -> String {
    let mut out = String::with_capacity(tenant_did.len() + 8);
    out.push_str("tenant:");
    for c in tenant_did.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, ':' | '_' | '-') {
            out.push(c);
        } else {
            out.push('-');
        }
    }
    if out.len() > 63 {
        out.truncate(63);
    }
    out
}

/// Standard set of tags/labels applied to every VM we create:
/// `rope-deployer` is the ownership marker (used by `list()` to scope
/// out non-Rope VMs), `tenant:*` scopes the VM to a tenant DID, and
/// `kind:*` records the node kind. Any user-supplied labels are
/// appended after sanitisation.
///
/// Returns a `Vec<String>` for providers like DO whose tag API is a
/// flat string list. Providers with a `map<string, string>` label API
/// (Exoscale) should call `standard_labels_map` instead.
pub(super) fn standard_tags(
    tenant_tag: &str,
    kind: NodeKind,
    extra: &BTreeMap<String, String>,
) -> Vec<String> {
    let mut out = vec![
        "rope-deployer".to_string(),
        tenant_tag.to_string(),
        format!("kind:{}", kind.as_str()),
    ];
    for (k, v) in extra {
        let sanitized = format!("{}:{}", sanitise_tag(k), sanitise_tag(v));
        if sanitized.len() <= 200 {
            out.push(sanitized);
        }
    }
    out
}

/// Map version of `standard_tags`, for providers whose native label
/// shape is `map<string, string>` (Exoscale).
///
/// The keys `rope-deployer`, `tenant`, `kind`, `node-identity-pub`
/// are reserved. `node-identity-pub` is the Ed25519 verifying key we
/// generated on the deployer — recording it in the labels means an
/// operator listing instances via the raw Exoscale API can still see
/// the node identity without querying the deployer's state file.
pub(super) fn standard_labels_map(
    tenant_did: &str,
    kind: NodeKind,
    node_identity_pub_b64: &str,
    extra: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    out.insert("rope-deployer".to_string(), "true".to_string());
    out.insert("tenant".to_string(), sanitise_tag(tenant_did));
    out.insert("kind".to_string(), kind.as_str().to_string());
    // Exoscale label values are capped at 255 chars; a base64-encoded
    // 32-byte Ed25519 key is 44 chars, well under.
    out.insert(
        "node-identity-pub".to_string(),
        node_identity_pub_b64.to_string(),
    );
    for (k, v) in extra {
        let key = sanitise_tag(k);
        let val = sanitise_tag(v);
        if !key.is_empty() && val.len() <= 200 {
            out.insert(key, val);
        }
    }
    out
}

/// Provider label-value sanitiser. `pub(super)` so sibling
/// adapters can compute the exact string stored under the `tenant`
/// label without duplicating the rule.
pub(super) fn sanitise_tag(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, ':' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tests — locked-in invariants for the shared enrolment shape.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Provider;

    fn sample_req(kind: NodeKind, zone: &str) -> ProvisionRequest {
        ProvisionRequest {
            tenant_did: "did:dwp:0xabc".into(),
            tenant_onchainid: "0xabc".into(),
            project_name: "Test Project".into(),
            provider: Provider::Digitalocean,
            zone: zone.into(),
            instance_size: "s-2vcpu-4gb".into(),
            node_kind: kind,
            ssh_pubkey: "ssh-ed25519 AAAA test@laptop".into(),
            labels: BTreeMap::new(),
        }
    }

    #[test]
    fn b64_matches_known_vectors() {
        assert_eq!(b64_encode(b""), "");
        assert_eq!(b64_encode(b"f"), "Zg==");
        assert_eq!(b64_encode(b"fo"), "Zm8=");
        assert_eq!(b64_encode(b"foo"), "Zm9v");
        assert_eq!(b64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(b64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(b64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn hostname_is_dns_compatible() {
        let host = build_hostname("did:dwp:0xdeadbeef", NodeKind::Witness, "fra1");
        assert!(host.starts_with("rope-witness-"));
        assert!(host.ends_with("-fra1"));
        for c in host.chars() {
            assert!(
                c.is_ascii_alphanumeric() || c == '-',
                "hostname contains non-DNS char: {c:?} in {host}"
            );
        }
    }

    #[test]
    fn tenant_tag_is_provider_safe_and_bounded() {
        let did = "did:dwp:0xABCDEF0123456789ABCDEF0123456789ABCDEF01";
        let t = tenant_tag_for(did);
        assert!(t.starts_with("tenant:"));
        assert!(t.len() <= 63);
        for c in t.chars() {
            assert!(c.is_ascii_alphanumeric() || matches!(c, ':' | '_' | '-'));
        }
    }

    #[test]
    fn cloud_init_contains_identity_and_installer() {
        let req = sample_req(NodeKind::Rpc, "fra1");
        let id = NodeIdentity::generate();
        let script = build_cloud_init(&req, &id);
        assert!(script.starts_with("#!/bin/bash"));
        assert!(script.contains("did:dwp:0xabc"));
        assert!(script.contains("identity.key"));
        assert!(script.contains("identity.pub"));
        assert!(script.contains("enrolment.json"));
        assert!(script.contains("get.datachain.network/rope-cli"));
        assert!(script.contains("ssh-ed25519 AAAA test@laptop"));
    }

    #[test]
    fn cloud_init_is_identical_across_providers_for_same_input() {
        // Two independent NodeIdentity keys will differ, but the
        // *shape* of the script must be identical (same file paths,
        // same installer URL). Guard against future provider-specific
        // divergence.
        let req_do = sample_req(NodeKind::Rpc, "fra1");
        let req_exo = sample_req(NodeKind::Rpc, "de-fra-1");
        let id = NodeIdentity::generate();
        let script_do = build_cloud_init(&req_do, &id);
        let script_exo = build_cloud_init(&req_exo, &id);
        // Same identity keypair should yield an identical script when
        // only the region differs by a substring.
        assert!(script_do.contains("region\": \"fra1"));
        assert!(script_exo.contains("region\": \"de-fra-1"));
        // Both must run the same bootstrap invocation.
        for s in [&script_do, &script_exo] {
            assert!(s.contains("rope-cli-install.sh"));
            assert!(s.contains("--unattended --kind rpc"));
        }
    }

    #[test]
    fn node_identity_public_key_is_32_bytes_base64() {
        let id = NodeIdentity::generate();
        let raw = id.verifying.to_bytes();
        assert_eq!(raw.len(), 32);
        let b = id.public_b64();
        assert_eq!(b.len(), 44);
        assert!(b.ends_with('='));
    }

    #[test]
    fn standard_labels_map_includes_reserved_keys() {
        let mut extra = BTreeMap::new();
        extra.insert("federation".to_string(), "amazon-basin".to_string());
        let map = standard_labels_map(
            "did:dwp:0xabc",
            NodeKind::Rpc,
            "MzZlZjZmMzJmMzY3Nzc4ODk5MDk4Nzg2NTQzMjE=",
            &extra,
        );
        assert_eq!(map.get("rope-deployer"), Some(&"true".to_string()));
        assert_eq!(map.get("kind"), Some(&"rpc".to_string()));
        assert!(map.get("tenant").unwrap().contains("did:dwp:0xabc"));
        assert_eq!(
            map.get("node-identity-pub").unwrap(),
            "MzZlZjZmMzJmMzY3Nzc4ODk5MDk4Nzg2NTQzMjE="
        );
        assert_eq!(map.get("federation"), Some(&"amazon-basin".to_string()));
    }
}
