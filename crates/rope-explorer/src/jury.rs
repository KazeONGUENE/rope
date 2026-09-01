//! Verifiable random jury selection for governance cause votes.
//!
//! Mirrors `MintingGovernance::select_governors` (BLAKE3 iterative hash over
//! entropy + index) but operates on normalized EVM address strings and
//! selects `ceil(pool.len() * fraction_bps / 10000)` unique jurors.

/// Default jury fraction: 60% of the governance pool (6000 basis points).
pub const DEFAULT_JURY_FRACTION_BPS: u16 = 6000;

/// Normalize an EVM address to lowercase `0x` + 40 hex chars. Returns `None`
/// for invalid input.
pub fn normalize_address(raw: &str) -> Option<String> {
    let s = raw.trim();
    let body = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s);
    if body.len() != 40 || !body.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(format!("0x{}", body.to_ascii_lowercase()))
}

/// Deduplicate and sort `pool` for deterministic selection input.
pub fn normalize_pool(pool: &[String]) -> Vec<String> {
    let mut out: Vec<String> = pool
        .iter()
        .filter_map(|a| normalize_address(a))
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Select `ceil(pool.len() * fraction_bps / 10000)` unique addresses from
/// `pool` using verifiable entropy. `pool` should already be normalized
/// (call [`normalize_pool`] first).
pub fn select_jury(
    pool: &[String],
    entropy: &[u8; 32],
    fraction_bps: u16,
) -> (Vec<String>, [u8; 32]) {
    if pool.is_empty() || fraction_bps == 0 {
        let proof = selection_proof(entropy, fraction_bps, pool, &[]);
        return (Vec::new(), proof);
    }

    let target = ((pool.len() as u128 * fraction_bps as u128 + 9_999) / 10_000) as usize;
    let target = target.min(pool.len());

    let mut selected: Vec<String> = Vec::with_capacity(target);
    let mut selection_state = *entropy;
    let mut round: u32 = 0;

    while selected.len() < target {
        selection_state = *blake3::hash(&[&selection_state[..], &round.to_le_bytes()].concat()).as_bytes();
        round += 1;

        let index = u64::from_le_bytes(selection_state[0..8].try_into().unwrap()) as usize % pool.len();
        let mut candidate = pool[index].clone();

        let mut attempts = 0u32;
        while selected.contains(&candidate) && attempts < 100 {
            selection_state = *blake3::hash(&selection_state).as_bytes();
            let new_index =
                u64::from_le_bytes(selection_state[0..8].try_into().unwrap()) as usize % pool.len();
            candidate = pool[new_index].clone();
            attempts += 1;
        }

        if !selected.contains(&candidate) {
            selected.push(candidate);
        } else if round > pool.len() as u32 * 200 {
            // Safety valve: pool exhausted despite dedup (shouldn't happen).
            break;
        }
    }

    selected.sort();
    let proof = selection_proof(entropy, fraction_bps, pool, &selected);
    (selected, proof)
}

fn selection_proof(
    entropy: &[u8; 32],
    fraction_bps: u16,
    pool: &[String],
    selected: &[String],
) -> [u8; 32] {
    let mut preimage = Vec::new();
    preimage.extend_from_slice(entropy);
    preimage.extend_from_slice(&fraction_bps.to_le_bytes());
    for addr in pool {
        preimage.extend_from_slice(addr.as_bytes());
    }
    preimage.push(0);
    for addr in selected {
        preimage.extend_from_slice(addr.as_bytes());
    }
    *blake3::hash(&preimage).as_bytes()
}

/// Case-insensitive membership check against a jury list.
pub fn is_juror(jurors: &[String], addr: &str) -> bool {
    let Some(normalized) = normalize_address(addr) else {
        return false;
    };
    jurors.iter().any(|j| j == &normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_pool(n: usize) -> Vec<String> {
        (0..n)
            .map(|i| format!("0x{:040x}", i + 1))
            .collect()
    }

    #[test]
    fn select_jury_deterministic() {
        let pool = normalize_pool(&sample_pool(20));
        let entropy = [42u8; 32];
        let (a, proof_a) = select_jury(&pool, &entropy, DEFAULT_JURY_FRACTION_BPS);
        let (b, proof_b) = select_jury(&pool, &entropy, DEFAULT_JURY_FRACTION_BPS);
        assert_eq!(a, b);
        assert_eq!(proof_a, proof_b);
    }

    #[test]
    fn select_jury_size_ceil_60pct() {
        let pool = normalize_pool(&sample_pool(10));
        let entropy = [1u8; 32];
        let (jurors, _) = select_jury(&pool, &entropy, 6000);
        assert_eq!(jurors.len(), 6);
        assert!(jurors.iter().all(|j| pool.contains(j)));
        let unique: std::collections::HashSet<_> = jurors.iter().collect();
        assert_eq!(unique.len(), jurors.len());
    }

    #[test]
    fn select_jury_different_entropy_differs() {
        let pool = normalize_pool(&sample_pool(15));
        let (a, _) = select_jury(&pool, &[1u8; 32], 6000);
        let (b, _) = select_jury(&pool, &[2u8; 32], 6000);
        assert_ne!(a, b);
    }

    #[test]
    fn normalize_pool_dedupes_and_sorts() {
        let raw = vec![
            "0xBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB".to_string(),
            "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            "0xAaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
        ];
        let pool = normalize_pool(&raw);
        assert_eq!(pool.len(), 2);
        assert_eq!(pool[0], "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        assert_eq!(pool[1], "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    }

    #[test]
    fn is_juror_case_insensitive() {
        let jurors = vec!["0xabcdefabcdefabcdefabcdefabcdefabcdefabcd".to_string()];
        assert!(is_juror(
            &jurors,
            "0xABCDEFABCDEFABCDEFABCDEFABCDEFABCDEFABCD"
        ));
        assert!(!is_juror(&jurors, "0x0000000000000000000000000000000000000001"));
    }
}
