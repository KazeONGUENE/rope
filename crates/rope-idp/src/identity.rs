//! Identity resolution — turns a verified Datawallet+ auth user into
//! the full ecosystem claim set (name, DID, wallets, primary address).

use crate::jwt::WalletClaim;
use crate::supabase::{GoTrueUser, SupabaseClient, SupabaseError};

/// The resolved identity, ready to be embedded in a token.
#[derive(Debug)]
pub struct ResolvedIdentity {
    pub sub: String,
    pub email: String,
    pub name: String,
    pub did: String,
    pub primary_address: Option<String>,
    pub wallets: Vec<WalletClaim>,
    pub public_key: Option<String>,
}

/// Wallet types that indicate a Datachain Rope-native wallet; these are
/// preferred as the primary address when no explicit default is set.
const ROPE_NATIVE_TYPES: [&str; 3] = ["DATACHAIN", "DC", "ROPE"];

fn display_name_from(user: &GoTrueUser) -> String {
    let meta = &user.user_metadata;
    for key in ["full_name", "name", "display_name"] {
        if let Some(v) = meta.get(key).and_then(|v| v.as_str()) {
            let trimmed = v.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }
    let first = meta.get("first_name").and_then(|v| v.as_str()).unwrap_or("");
    let last = meta.get("last_name").and_then(|v| v.as_str()).unwrap_or("");
    let combined = format!("{first} {last}").trim().to_string();
    combined
}

fn pick_primary(wallets: &[WalletClaim]) -> Option<String> {
    if let Some(w) = wallets.iter().find(|w| w.is_default) {
        return Some(w.address.clone());
    }
    if let Some(w) = wallets.iter().find(|w| {
        ROPE_NATIVE_TYPES
            .iter()
            .any(|t| w.wallet_type.eq_ignore_ascii_case(t))
    }) {
        return Some(w.address.clone());
    }
    wallets.first().map(|w| w.address.clone())
}

/// Resolve the full ecosystem identity for a verified GoTrue user.
///
/// Enrichment lookups are best-effort: a missing `tanastok_users` or
/// `profiles` row never fails authentication — the token simply carries
/// fewer claims.
pub async fn resolve(
    supabase: &SupabaseClient,
    user: &GoTrueUser,
) -> Result<ResolvedIdentity, SupabaseError> {
    let sub = user.id.clone();
    let email = user.email.clone().unwrap_or_default();
    let mut name = display_name_from(user);

    let app_user = supabase.app_user_by_auth_id(&sub).await.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "tanastok_users lookup failed");
        None
    });
    let profile = supabase.profile_by_auth_id(&sub).await.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "profiles lookup failed");
        None
    });

    let mut wallets: Vec<WalletClaim> = Vec::new();
    if let Some(app) = &app_user {
        match supabase.wallets_by_app_user(&app.id).await {
            Ok(rows) => {
                wallets = rows
                    .into_iter()
                    .map(|w| WalletClaim {
                        address: w.address,
                        wallet_type: w.wallet_type.unwrap_or_else(|| "UNKNOWN".into()),
                        is_default: w.is_default.unwrap_or(false),
                        verified: w.verified.unwrap_or(false),
                    })
                    .collect();
            }
            Err(e) => tracing::warn!(error = %e, "wallets lookup failed"),
        }
    }

    let mut public_key = None;
    let mut profile_did = None;
    if let Some(p) = &profile {
        public_key = p.public_key.clone().filter(|s| !s.is_empty());
        profile_did = p.did.clone().filter(|s| !s.is_empty());
        if name.is_empty() {
            name = p
                .display_name
                .clone()
                .or_else(|| p.username.clone())
                .unwrap_or_default();
        }
    }

    let did = app_user
        .as_ref()
        .and_then(|a| a.did.clone())
        .filter(|s| !s.is_empty())
        .or(profile_did)
        .unwrap_or_else(|| format!("did:web:datawallet.plus:{sub}"));

    let primary_address = pick_primary(&wallets);

    Ok(ResolvedIdentity {
        sub,
        email,
        name,
        did,
        primary_address,
        wallets,
        public_key,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wallet(addr: &str, wtype: &str, is_default: bool) -> WalletClaim {
        WalletClaim {
            address: addr.into(),
            wallet_type: wtype.into(),
            is_default,
            verified: false,
        }
    }

    #[test]
    fn primary_prefers_explicit_default() {
        let ws = vec![
            wallet("0xaaa", "ETHEREUM", false),
            wallet("0xbbb", "XDC", true),
        ];
        assert_eq!(pick_primary(&ws).as_deref(), Some("0xbbb"));
    }

    #[test]
    fn primary_prefers_rope_native_over_first() {
        let ws = vec![
            wallet("0xaaa", "ETHEREUM", false),
            wallet("0xbbb", "DATACHAIN", false),
        ];
        assert_eq!(pick_primary(&ws).as_deref(), Some("0xbbb"));
    }

    #[test]
    fn primary_falls_back_to_first() {
        let ws = vec![
            wallet("0xaaa", "ETHEREUM", false),
            wallet("0xbbb", "XDC", false),
        ];
        assert_eq!(pick_primary(&ws).as_deref(), Some("0xaaa"));
    }

    #[test]
    fn primary_none_when_no_wallets() {
        assert_eq!(pick_primary(&[]), None);
    }
}
