-- =============================================================================
-- Datachain Rope - Retroactive CHECK Constraints for Existing Databases
-- 2026-07-26 counter-audit follow-up (docs/SECURITY_AUDIT_2026-07-25_FULL_WORKSPACE.md)
-- =============================================================================
--
-- FINDING: none of the money/stake/percentage/count columns created by
-- 01-init.sql or 02-federation-community.sql carried a CHECK constraint.
-- A bug anywhere in the write path (rope-indexer, dc-explorer, a future
-- admin tool) could silently persist a negative balance, a negative
-- transaction value, an out-of-range commission rate, or a negative vote
-- count, and Postgres would accept it without complaint. For a ledger
-- explorer this is a correctness/integrity backstop, not just hygiene:
-- CHECK constraints turn a class of application-layer bugs into an
-- immediate, loud INSERT/UPDATE failure instead of a silently corrupted
-- row that later confuses every reader of that table.
--
-- 01-init.sql and 02-federation-community.sql were updated in this same
-- change to declare these CHECK constraints inline for any *future* fresh
-- deployment (CREATE TABLE time). But per the header note already present
-- in 03-ai-agents.sql/04-least-privilege-roles.sql, Postgres only executes
-- files under /docker-entrypoint-initdb.d on a container's FIRST boot
-- against an EMPTY data volume — an already-initialized production
-- database (the actual dcscan database backing dcscan.io today) will never
-- re-run 01/02, so its existing tables would keep sailing without these
-- constraints unless retroactively altered. This file is that retroactive
-- ALTER TABLE pass; apply it once, manually, against the live database:
--
--     psql "$DATABASE_URL" -f deploy/init-db/05-check-constraints.sql
--
-- SAFETY: every ADD CONSTRAINT below is issued with NOT VALID, which
-- means Postgres does NOT scan existing rows at ALTER-TABLE time (no long
-- table lock, no risk of the migration itself failing because of a
-- pre-existing bad row) — it only starts enforcing the constraint on
-- INSERT/UPDATE going forward. The immediately-following
-- `VALIDATE CONSTRAINT` statements then perform the backfill scan under a
-- much lighter lock (ShareUpdateExclusiveLock, which does not block
-- concurrent reads/writes) and will report exactly which constraint (and
-- implicitly which rows) are pre-existing violations, if any -- that is a
-- genuine "you have a data-integrity bug to fix by hand" signal, not
-- something this migration should paper over by skipping validation.
--
-- IDEMPOTENT: every block below checks `pg_constraint` by name before
-- adding, and only VALIDATEs (a no-op if already valid) after — safe to
-- re-run against an already-migrated database, matching the pattern in
-- 03-ai-agents.sql / 04-least-privilege-roles.sql.
-- =============================================================================

DO $$
BEGIN
    -- accounts
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'accounts_balance_check') THEN
        ALTER TABLE accounts ADD CONSTRAINT accounts_balance_check CHECK (balance >= 0) NOT VALID;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'accounts_nonce_check') THEN
        ALTER TABLE accounts ADD CONSTRAINT accounts_nonce_check CHECK (nonce >= 0) NOT VALID;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'accounts_stake_check') THEN
        ALTER TABLE accounts ADD CONSTRAINT accounts_stake_check CHECK (stake >= 0) NOT VALID;
    END IF;

    -- transactions
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'transactions_value_check') THEN
        ALTER TABLE transactions ADD CONSTRAINT transactions_value_check CHECK (value >= 0) NOT VALID;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'transactions_nonce_check') THEN
        ALTER TABLE transactions ADD CONSTRAINT transactions_nonce_check CHECK (nonce >= 0) NOT VALID;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'transactions_gas_limit_check') THEN
        ALTER TABLE transactions ADD CONSTRAINT transactions_gas_limit_check CHECK (gas_limit >= 0) NOT VALID;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'transactions_gas_price_check') THEN
        ALTER TABLE transactions ADD CONSTRAINT transactions_gas_price_check CHECK (gas_price >= 0) NOT VALID;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'transactions_gas_used_check') THEN
        ALTER TABLE transactions ADD CONSTRAINT transactions_gas_used_check CHECK (gas_used IS NULL OR gas_used >= 0) NOT VALID;
    END IF;

    -- token_transfers
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'token_transfers_value_check') THEN
        ALTER TABLE token_transfers ADD CONSTRAINT token_transfers_value_check CHECK (value >= 0) NOT VALID;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'token_transfers_log_index_check') THEN
        ALTER TABLE token_transfers ADD CONSTRAINT token_transfers_log_index_check CHECK (log_index >= 0) NOT VALID;
    END IF;

    -- validators
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'validators_stake_check') THEN
        ALTER TABLE validators ADD CONSTRAINT validators_stake_check CHECK (stake >= 0) NOT VALID;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'validators_commission_rate_check') THEN
        ALTER TABLE validators ADD CONSTRAINT validators_commission_rate_check CHECK (commission_rate >= 0 AND commission_rate <= 1) NOT VALID;
    END IF;

    -- minting_proposals
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'minting_proposals_amount_check') THEN
        ALTER TABLE minting_proposals ADD CONSTRAINT minting_proposals_amount_check CHECK (amount > 0) NOT VALID;
    END IF;

    -- network_stats
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'network_stats_total_strings_check') THEN
        ALTER TABLE network_stats ADD CONSTRAINT network_stats_total_strings_check CHECK (total_strings >= 0) NOT VALID;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'network_stats_total_transactions_check') THEN
        ALTER TABLE network_stats ADD CONSTRAINT network_stats_total_transactions_check CHECK (total_transactions >= 0) NOT VALID;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'network_stats_total_accounts_check') THEN
        ALTER TABLE network_stats ADD CONSTRAINT network_stats_total_accounts_check CHECK (total_accounts >= 0) NOT VALID;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'network_stats_active_validators_check') THEN
        ALTER TABLE network_stats ADD CONSTRAINT network_stats_active_validators_check CHECK (active_validators >= 0) NOT VALID;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'network_stats_total_stake_check') THEN
        ALTER TABLE network_stats ADD CONSTRAINT network_stats_total_stake_check CHECK (total_stake >= 0) NOT VALID;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'network_stats_strings_per_second_check') THEN
        ALTER TABLE network_stats ADD CONSTRAINT network_stats_strings_per_second_check CHECK (strings_per_second IS NULL OR strings_per_second >= 0) NOT VALID;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'network_stats_average_finality_ms_check') THEN
        ALTER TABLE network_stats ADD CONSTRAINT network_stats_average_finality_ms_check CHECK (average_finality_ms IS NULL OR average_finality_ms >= 0) NOT VALID;
    END IF;

    -- daily_stats
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'daily_stats_strings_created_check') THEN
        ALTER TABLE daily_stats ADD CONSTRAINT daily_stats_strings_created_check CHECK (strings_created >= 0) NOT VALID;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'daily_stats_transactions_count_check') THEN
        ALTER TABLE daily_stats ADD CONSTRAINT daily_stats_transactions_count_check CHECK (transactions_count >= 0) NOT VALID;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'daily_stats_unique_addresses_check') THEN
        ALTER TABLE daily_stats ADD CONSTRAINT daily_stats_unique_addresses_check CHECK (unique_addresses >= 0) NOT VALID;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'daily_stats_volume_check') THEN
        ALTER TABLE daily_stats ADD CONSTRAINT daily_stats_volume_check CHECK (volume >= 0) NOT VALID;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'daily_stats_gas_used_check') THEN
        ALTER TABLE daily_stats ADD CONSTRAINT daily_stats_gas_used_check CHECK (gas_used >= 0) NOT VALID;
    END IF;

    -- federations (only applied if 02-federation-community.sql has run on
    -- this database — that schema may not exist on every deployment)
    IF EXISTS (SELECT 1 FROM information_schema.tables WHERE table_name = 'federations') THEN
        IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'federations_vote_count_for_check') THEN
            ALTER TABLE federations ADD CONSTRAINT federations_vote_count_for_check CHECK (vote_count_for >= 0) NOT VALID;
        END IF;
        IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'federations_vote_count_against_check') THEN
            ALTER TABLE federations ADD CONSTRAINT federations_vote_count_against_check CHECK (vote_count_against >= 0) NOT VALID;
        END IF;
        IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'federations_data_wallets_count_check') THEN
            ALTER TABLE federations ADD CONSTRAINT federations_data_wallets_count_check CHECK (data_wallets_count >= 0) NOT VALID;
        END IF;
        IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'federations_data_wallets_generated_check') THEN
            ALTER TABLE federations ADD CONSTRAINT federations_data_wallets_generated_check CHECK (data_wallets_generated >= 0) NOT VALID;
        END IF;
        IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'federations_individual_chains_count_check') THEN
            ALTER TABLE federations ADD CONSTRAINT federations_individual_chains_count_check CHECK (individual_chains_count >= 0) NOT VALID;
        END IF;
        IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'federations_individual_chains_generated_check') THEN
            ALTER TABLE federations ADD CONSTRAINT federations_individual_chains_generated_check CHECK (individual_chains_generated >= 0) NOT VALID;
        END IF;
    END IF;

    -- communities
    IF EXISTS (SELECT 1 FROM information_schema.tables WHERE table_name = 'communities') THEN
        IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'communities_vote_count_for_check') THEN
            ALTER TABLE communities ADD CONSTRAINT communities_vote_count_for_check CHECK (vote_count_for >= 0) NOT VALID;
        END IF;
        IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'communities_vote_count_against_check') THEN
            ALTER TABLE communities ADD CONSTRAINT communities_vote_count_against_check CHECK (vote_count_against >= 0) NOT VALID;
        END IF;
        IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'communities_data_wallets_count_check') THEN
            ALTER TABLE communities ADD CONSTRAINT communities_data_wallets_count_check CHECK (data_wallets_count >= 0) NOT VALID;
        END IF;
        IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'communities_data_wallets_generated_check') THEN
            ALTER TABLE communities ADD CONSTRAINT communities_data_wallets_generated_check CHECK (data_wallets_generated >= 0) NOT VALID;
        END IF;
    END IF;

    -- project_submissions
    IF EXISTS (SELECT 1 FROM information_schema.tables WHERE table_name = 'project_submissions') THEN
        IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'project_submissions_funding_requested_check') THEN
            ALTER TABLE project_submissions ADD CONSTRAINT project_submissions_funding_requested_check CHECK (funding_requested >= 0) NOT VALID;
        END IF;
        IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'project_submissions_vote_count_for_check') THEN
            ALTER TABLE project_submissions ADD CONSTRAINT project_submissions_vote_count_for_check CHECK (vote_count_for >= 0) NOT VALID;
        END IF;
        IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'project_submissions_vote_count_against_check') THEN
            ALTER TABLE project_submissions ADD CONSTRAINT project_submissions_vote_count_against_check CHECK (vote_count_against >= 0) NOT VALID;
        END IF;
        IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'project_submissions_required_votes_check') THEN
            ALTER TABLE project_submissions ADD CONSTRAINT project_submissions_required_votes_check CHECK (required_votes >= 0) NOT VALID;
        END IF;
        IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'project_submissions_approval_threshold_check') THEN
            ALTER TABLE project_submissions ADD CONSTRAINT project_submissions_approval_threshold_check CHECK (approval_threshold >= 0 AND approval_threshold <= 1) NOT VALID;
        END IF;
    END IF;

    -- votes
    IF EXISTS (SELECT 1 FROM information_schema.tables WHERE table_name = 'votes') THEN
        IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'votes_voter_stake_check') THEN
            ALTER TABLE votes ADD CONSTRAINT votes_voter_stake_check CHECK (voter_stake >= 0) NOT VALID;
        END IF;
        IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'votes_vote_weight_check') THEN
            ALTER TABLE votes ADD CONSTRAINT votes_vote_weight_check CHECK (vote_weight >= 0) NOT VALID;
        END IF;
    END IF;

    -- diagnosis_records / maintenance_recommendations (confidence/score bounds)
    IF EXISTS (SELECT 1 FROM information_schema.tables WHERE table_name = 'diagnosis_records') THEN
        IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'diagnosis_records_confidence_score_check') THEN
            ALTER TABLE diagnosis_records ADD CONSTRAINT diagnosis_records_confidence_score_check CHECK (confidence_score IS NULL OR (confidence_score >= 0 AND confidence_score <= 1)) NOT VALID;
        END IF;
    END IF;
    IF EXISTS (SELECT 1 FROM information_schema.tables WHERE table_name = 'maintenance_recommendations') THEN
        IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'maintenance_recommendations_recommendation_score_check') THEN
            ALTER TABLE maintenance_recommendations ADD CONSTRAINT maintenance_recommendations_recommendation_score_check CHECK (recommendation_score IS NULL OR (recommendation_score >= 0 AND recommendation_score <= 1)) NOT VALID;
        END IF;
    END IF;
END
$$;

-- Validate every constraint just added. VALIDATE CONSTRAINT is a no-op
-- (near-instant) if the constraint is already valid (e.g. this file is
-- being re-run), and takes only ShareUpdateExclusiveLock (does not block
-- concurrent reads/writes) when it does have to scan. If any of these
-- raise, it means the live table already contains data that violates the
-- new invariant — that is a real bug to investigate by hand, not
-- something to silently work around here.
ALTER TABLE accounts VALIDATE CONSTRAINT accounts_balance_check;
ALTER TABLE accounts VALIDATE CONSTRAINT accounts_nonce_check;
ALTER TABLE accounts VALIDATE CONSTRAINT accounts_stake_check;
ALTER TABLE transactions VALIDATE CONSTRAINT transactions_value_check;
ALTER TABLE transactions VALIDATE CONSTRAINT transactions_nonce_check;
ALTER TABLE transactions VALIDATE CONSTRAINT transactions_gas_limit_check;
ALTER TABLE transactions VALIDATE CONSTRAINT transactions_gas_price_check;
ALTER TABLE transactions VALIDATE CONSTRAINT transactions_gas_used_check;
ALTER TABLE token_transfers VALIDATE CONSTRAINT token_transfers_value_check;
ALTER TABLE token_transfers VALIDATE CONSTRAINT token_transfers_log_index_check;
ALTER TABLE validators VALIDATE CONSTRAINT validators_stake_check;
ALTER TABLE validators VALIDATE CONSTRAINT validators_commission_rate_check;
ALTER TABLE minting_proposals VALIDATE CONSTRAINT minting_proposals_amount_check;
ALTER TABLE network_stats VALIDATE CONSTRAINT network_stats_total_strings_check;
ALTER TABLE network_stats VALIDATE CONSTRAINT network_stats_total_transactions_check;
ALTER TABLE network_stats VALIDATE CONSTRAINT network_stats_total_accounts_check;
ALTER TABLE network_stats VALIDATE CONSTRAINT network_stats_active_validators_check;
ALTER TABLE network_stats VALIDATE CONSTRAINT network_stats_total_stake_check;
ALTER TABLE network_stats VALIDATE CONSTRAINT network_stats_strings_per_second_check;
ALTER TABLE network_stats VALIDATE CONSTRAINT network_stats_average_finality_ms_check;
ALTER TABLE daily_stats VALIDATE CONSTRAINT daily_stats_strings_created_check;
ALTER TABLE daily_stats VALIDATE CONSTRAINT daily_stats_transactions_count_check;
ALTER TABLE daily_stats VALIDATE CONSTRAINT daily_stats_unique_addresses_check;
ALTER TABLE daily_stats VALIDATE CONSTRAINT daily_stats_volume_check;
ALTER TABLE daily_stats VALIDATE CONSTRAINT daily_stats_gas_used_check;

-- The federations/communities/project_submissions/votes/diagnosis_records/
-- maintenance_recommendations validations are conditional on those tables
-- existing (02-federation-community.sql is not applied on every
-- deployment), so they are wrapped the same way the ADD CONSTRAINT calls
-- were above.
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM information_schema.tables WHERE table_name = 'federations') THEN
        ALTER TABLE federations VALIDATE CONSTRAINT federations_vote_count_for_check;
        ALTER TABLE federations VALIDATE CONSTRAINT federations_vote_count_against_check;
        ALTER TABLE federations VALIDATE CONSTRAINT federations_data_wallets_count_check;
        ALTER TABLE federations VALIDATE CONSTRAINT federations_data_wallets_generated_check;
        ALTER TABLE federations VALIDATE CONSTRAINT federations_individual_chains_count_check;
        ALTER TABLE federations VALIDATE CONSTRAINT federations_individual_chains_generated_check;
    END IF;
    IF EXISTS (SELECT 1 FROM information_schema.tables WHERE table_name = 'communities') THEN
        ALTER TABLE communities VALIDATE CONSTRAINT communities_vote_count_for_check;
        ALTER TABLE communities VALIDATE CONSTRAINT communities_vote_count_against_check;
        ALTER TABLE communities VALIDATE CONSTRAINT communities_data_wallets_count_check;
        ALTER TABLE communities VALIDATE CONSTRAINT communities_data_wallets_generated_check;
    END IF;
    IF EXISTS (SELECT 1 FROM information_schema.tables WHERE table_name = 'project_submissions') THEN
        ALTER TABLE project_submissions VALIDATE CONSTRAINT project_submissions_funding_requested_check;
        ALTER TABLE project_submissions VALIDATE CONSTRAINT project_submissions_vote_count_for_check;
        ALTER TABLE project_submissions VALIDATE CONSTRAINT project_submissions_vote_count_against_check;
        ALTER TABLE project_submissions VALIDATE CONSTRAINT project_submissions_required_votes_check;
        ALTER TABLE project_submissions VALIDATE CONSTRAINT project_submissions_approval_threshold_check;
    END IF;
    IF EXISTS (SELECT 1 FROM information_schema.tables WHERE table_name = 'votes') THEN
        ALTER TABLE votes VALIDATE CONSTRAINT votes_voter_stake_check;
        ALTER TABLE votes VALIDATE CONSTRAINT votes_vote_weight_check;
    END IF;
    IF EXISTS (SELECT 1 FROM information_schema.tables WHERE table_name = 'diagnosis_records') THEN
        ALTER TABLE diagnosis_records VALIDATE CONSTRAINT diagnosis_records_confidence_score_check;
    END IF;
    IF EXISTS (SELECT 1 FROM information_schema.tables WHERE table_name = 'maintenance_recommendations') THEN
        ALTER TABLE maintenance_recommendations VALIDATE CONSTRAINT maintenance_recommendations_recommendation_score_check;
    END IF;
END
$$;
