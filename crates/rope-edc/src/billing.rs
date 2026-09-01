//! Metering & billing against AccessGrant price terms - spec v1.0 §6.3:
//! "Metered and subscription access is billed automatically against the
//! grant's price terms".
//!
//! The gateway already meters every authorized request onto
//! `AccessGrant.calls`. This module turns those counters into billing
//! statements:
//!
//! * `statement_for` computes the open (not yet invoiced) amount from the
//!   grant's price model - pure, deterministic, auditable.
//! * Closing a statement (console action) anchors a `BillingStatement`
//!   knot on the project string and advances the invoiced watermark
//!   (`billed_calls`, `last_billed_at`), so the full invoicing history is
//!   on-chain and the open window restarts at zero.
//!
//! Settlement itself (FAT transfer, project-token transfer, or fiat via
//! an off-chain processor) is executed against the anchored statement -
//! the statement's knot hash is the invoice reference.

use serde::{Deserialize, Serialize};

use crate::grants::AccessGrant;
use crate::types::now_ts;

/// Subscription period label → seconds.
fn period_secs(period: &str) -> i64 {
    match period {
        "monthly" => 30 * 86_400,
        "quarterly" => 91 * 86_400,
        "yearly" => 365 * 86_400,
        // Unlabelled subscriptions default to monthly.
        _ => 30 * 86_400,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BillingStatement {
    pub grant_id: String,
    pub project_id: String,
    /// Price model the statement was computed under.
    pub price_model: String,
    pub currency: String,
    pub unit_amount: f64,
    /// Open window this statement covers [from, to] (unix seconds).
    pub period_start: i64,
    pub period_end: i64,
    /// Metered model: calls in the open window.
    pub metered_calls: u64,
    /// Subscription model: whole periods elapsed in the open window.
    pub periods_elapsed: u64,
    /// The amount owed for the open window.
    pub amount_due: f64,
    pub generated_at: i64,
}

/// Compute the open (un-invoiced) statement for a grant at `now`.
pub fn statement_for(grant: &AccessGrant, now: i64) -> BillingStatement {
    let window_start = if grant.last_billed_at > 0 {
        grant.last_billed_at
    } else {
        grant.effective_at.max(grant.created_at)
    };

    let open_calls = grant.calls.saturating_sub(grant.billed_calls);

    let (periods_elapsed, amount_due) = match grant.price.model.as_str() {
        "metered" => (0, open_calls as f64 * grant.price.amount),
        "subscription" => {
            let secs = period_secs(&grant.price.period);
            let periods = ((now - window_start).max(0) / secs) as u64;
            (periods, periods as f64 * grant.price.amount)
        }
        "one_time" => {
            // Owed exactly once: only while nothing has been invoiced yet.
            let due = if grant.last_billed_at == 0 { grant.price.amount } else { 0.0 };
            (0, due)
        }
        // "free" and anything unrecognized bills nothing.
        _ => (0, 0.0),
    };

    BillingStatement {
        grant_id: grant.id.clone(),
        project_id: grant.project_id.clone(),
        price_model: grant.price.model.clone(),
        currency: grant.price.currency.clone(),
        unit_amount: grant.price.amount,
        period_start: window_start,
        period_end: now,
        metered_calls: open_calls,
        periods_elapsed,
        amount_due,
        generated_at: now_ts(),
    }
}

/// The watermark a grant must advance to when `statement` is closed
/// (invoiced): `(billed_calls, last_billed_at)`.
///
/// * metered - everything counted on the statement is now invoiced.
/// * subscription - the watermark advances by whole periods only, so a
///   partially-elapsed period carries over to the next statement.
/// * one_time / free - the timestamp alone marks the invoice.
pub fn closed_watermark(grant: &AccessGrant, statement: &BillingStatement) -> (u64, i64) {
    match statement.price_model.as_str() {
        "metered" => (
            grant.billed_calls + statement.metered_calls,
            statement.period_end,
        ),
        "subscription" => {
            let advanced = statement.period_start
                + statement.periods_elapsed as i64 * period_secs(&grant.price.period);
            (grant.billed_calls, advanced.max(grant.last_billed_at))
        }
        _ => (grant.billed_calls, statement.period_end),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grants::{GrantPrice, GrantScope, Grantee, StakeholderClass};

    fn grant_with_price(model: &str, amount: f64, period: &str) -> AccessGrant {
        AccessGrant::new(
            "prj_x",
            Grantee { kind: "wallet".into(), value: "0xabc".into() },
            StakeholderClass::CommercialBuyer,
            GrantScope::default(),
            0,
            0,
            GrantPrice {
                model: model.into(),
                amount,
                currency: "FAT".into(),
                period: period.into(),
            },
            vec!["rest".into()],
            "0xowner",
            0,
        )
    }

    #[test]
    fn metered_billing() {
        let mut g = grant_with_price("metered", 0.25, "");
        g.calls = 1_000;
        g.billed_calls = 200;
        let s = statement_for(&g, now_ts());
        assert_eq!(s.metered_calls, 800);
        assert!((s.amount_due - 200.0).abs() < 1e-9);
    }

    #[test]
    fn subscription_billing_counts_whole_periods() {
        let mut g = grant_with_price("subscription", 500.0, "monthly");
        let now = now_ts();
        g.created_at = now - 65 * 86_400; // ~2.16 months ago
        g.effective_at = g.created_at;
        let s = statement_for(&g, now);
        assert_eq!(s.periods_elapsed, 2);
        assert!((s.amount_due - 1_000.0).abs() < 1e-9);
    }

    #[test]
    fn one_time_billed_once() {
        let mut g = grant_with_price("one_time", 5_000.0, "");
        let now = now_ts();
        let open = statement_for(&g, now);
        assert!((open.amount_due - 5_000.0).abs() < 1e-9);
        g.last_billed_at = now;
        let closed = statement_for(&g, now + 100);
        assert_eq!(closed.amount_due, 0.0);
    }

    #[test]
    fn free_grants_bill_nothing() {
        let mut g = grant_with_price("free", 0.0, "");
        g.calls = 10_000;
        let s = statement_for(&g, now_ts());
        assert_eq!(s.amount_due, 0.0);
    }

    #[test]
    fn watermark_advances_metered_and_subscription() {
        let now = now_ts();

        let mut m = grant_with_price("metered", 0.5, "");
        m.calls = 300;
        m.billed_calls = 100;
        let sm = statement_for(&m, now);
        let (bc, at) = closed_watermark(&m, &sm);
        assert_eq!(bc, 300);
        assert_eq!(at, now);

        let mut s = grant_with_price("subscription", 100.0, "monthly");
        s.created_at = now - 45 * 86_400; // 1.5 months
        s.effective_at = s.created_at;
        let ss = statement_for(&s, now);
        assert_eq!(ss.periods_elapsed, 1);
        let (_, at) = closed_watermark(&s, &ss);
        // Advances exactly one month past the window start, keeping the
        // half-elapsed period open.
        assert_eq!(at, s.created_at + 30 * 86_400);
    }
}
