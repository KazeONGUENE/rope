-- =============================================================================
-- Datachain Rope - Least-Privilege Database Roles
-- M12 (2026-07-25 security audit)
-- =============================================================================
--
-- FINDING (M12): none of the committed SQL migrations (01-init.sql,
-- 02-federation-community.sql, 03-ai-agents.sql) issue a single GRANT,
-- REVOKE, or CREATE ROLE statement. Access control on this database is
-- entirely implicit: every service that talks to Postgres today
-- (dc-explorer, rope-indexer) connects as the single `dcscan` role, which
-- is also the *database owner* (see docker-compose.yml's
-- `POSTGRES_USER: dcscan`) — i.e. every consumer of this database,
-- including a purely read-only future consumer, would otherwise have to
-- be handed full DDL/DML/ownership privileges just to run a SELECT.
--
-- WHAT THIS MIGRATION DOES
--
-- 1. Tightens the PUBLIC pseudo-role's default privileges on this
--    database and the `public` schema (defense-in-depth: any future role
--    created without an explicit GRANT should start from zero, not from
--    Postgres's historically permissive PUBLIC defaults).
-- 2. Provisions a genuine least-privilege `dcscan_readonly` role scoped
--    to SELECT-only on every table in `public`, including tables created
--    by *future* migrations (via `ALTER DEFAULT PRIVILEGES`), so any
--    read-only consumer added later (a reporting dashboard, an
--    analytics job, CERBER's own audit/read path, an ops runbook) can be
--    handed a role that is structurally incapable of writing, instead of
--    reusing the `dcscan` owner credential out of convenience.
--
-- WHAT THIS MIGRATION DELIBERATELY DOES NOT DO
--
-- `dcscan` (the existing app-owner role used by dc-explorer and
-- rope-indexer today) is NOT touched, demoted, or replaced. Both
-- existing services perform DML (dc-explorer reads +  writes cached
-- stats/agent rows; rope-indexer writes indexed chain data) through that
-- single role today, and there is currently no second, narrower-scoped
-- write role to split them into without a coordinated application-level
-- change to each service's DATABASE_URL — that is real future work
-- (tracked below), not something this migration should attempt silently.
-- This migration is purely additive: it changes nothing about how
-- `dcscan` behaves or what it can do, and it never disables or narrows
-- an already-working credential path.
--
-- `dcscan_readonly` is created WITHOUT `LOGIN` and WITHOUT a password.
-- A role with no login capability cannot authenticate at all — creating
-- it here is inert by construction, exactly like any freshly provisioned,
-- not-yet-assigned Postgres role. There is intentionally no secret baked
-- into this file (this migration ships in the public git history via
-- docker-entrypoint-initdb.d, same constraint that drove the M3 fix to
-- Dockerfile.indexer). To actually hand out read-only access to a real
-- consumer, an operator must run, on the live database, out-of-band:
--
--     ALTER ROLE dcscan_readonly WITH LOGIN PASSWORD '<a freshly generated secret>';
--
-- and then give that consumer a DATABASE_URL built from that role
-- instead of `dcscan`. Until that `ALTER ROLE` is run, `dcscan_readonly`
-- exists in the role catalog but nothing can authenticate as it — this
-- migration does not open any new attack surface by itself.
--
-- =============================================================================
-- 1. Tighten PUBLIC's implicit default privileges
-- =============================================================================

-- Postgres historically grants PUBLIC (i.e. every role, with zero
-- explicit GRANT) the ability to CREATE new objects in the `public`
-- schema and CONNECT to any database. Neither default is needed here:
-- the only roles that should ever touch this database are the ones
-- explicitly provisioned below (`dcscan`, the pre-existing owner, and
-- `dcscan_readonly`, provisioned here). Revoking these two defaults is
-- standard Postgres least-privilege hardening and has zero effect on
-- `dcscan`, which retains every privilege it already had as the
-- database/schema owner.
REVOKE CREATE ON SCHEMA public FROM PUBLIC;
REVOKE ALL ON DATABASE dcscan FROM PUBLIC;

-- =============================================================================
-- 2. Provision the read-only role (inert until an operator sets a
--    password — see the file header above)
-- =============================================================================

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'dcscan_readonly') THEN
        -- NOLOGIN: this role cannot authenticate until an operator
        -- explicitly runs `ALTER ROLE dcscan_readonly WITH LOGIN
        -- PASSWORD '...'` on the live database. See file header.
        CREATE ROLE dcscan_readonly WITH NOLOGIN;
    END IF;
END
$$;

COMMENT ON ROLE dcscan_readonly IS
    'M12 (2026-07-25 audit): least-privilege SELECT-only role for future '
    'read-only database consumers (reporting, analytics, audit tooling). '
    'Ships NOLOGIN by construction — an operator must run ALTER ROLE ... '
    'WITH LOGIN PASSWORD to activate it for a real consumer. Never grant '
    'this role INSERT/UPDATE/DELETE/DDL; provision a separate role for '
    'that instead of widening this one.';

-- Allow the role to open a connection and see the schema (necessary
-- prerequisites for SELECT to do anything useful; neither grants any
-- data access by itself).
GRANT CONNECT ON DATABASE dcscan TO dcscan_readonly;
GRANT USAGE ON SCHEMA public TO dcscan_readonly;

-- SELECT on every table that exists in `public` right now (idempotent —
-- re-running this migration against an already-migrated database is
-- always safe, matching the `IF NOT EXISTS` pattern used by the other
-- migrations in this directory).
GRANT SELECT ON ALL TABLES IN SCHEMA public TO dcscan_readonly;

-- SELECT on every table any *future* migration creates in `public`,
-- as long as that migration runs as the `dcscan` role (the default for
-- everything under docker-entrypoint-initdb.d, and for any manually
-- applied migration run with the same connection the app uses). Without
-- this, a future 05-*.sql migration would silently reintroduce the
-- M12 finding for its own new tables.
ALTER DEFAULT PRIVILEGES FOR ROLE dcscan IN SCHEMA public
    GRANT SELECT ON TABLES TO dcscan_readonly;

-- Explicitly confirm the role has no write/DDL capability of any kind
-- (belt-and-suspenders — Postgres roles start with zero privileges by
-- default, but this makes the invariant self-documenting and gives any
-- future migration author an obvious place to see what "least privilege"
-- means here, rather than relying on the absence of a GRANT elsewhere).
REVOKE INSERT, UPDATE, DELETE, TRUNCATE, REFERENCES, TRIGGER
    ON ALL TABLES IN SCHEMA public FROM dcscan_readonly;
REVOKE CREATE, USAGE ON SCHEMA public FROM dcscan_readonly;
GRANT USAGE ON SCHEMA public TO dcscan_readonly;

-- =============================================================================
-- FOLLOW-UP WORK (not attempted in this migration — requires an
-- application-level DATABASE_URL change per service, out of scope for a
-- pure-SQL migration; tracked here so it isn't lost)
-- =============================================================================
--
-- - Split `dcscan` (currently both the DB owner AND the sole app-writer)
--   into a narrower `dcscan_app` role that holds only the DML privileges
--   dc-explorer/rope-indexer actually issue (SELECT/INSERT/UPDATE on the
--   specific tables each service touches), leaving `dcscan` as a
--   migration-only owner role that application services never connect
--   as day-to-day.
-- - Wire a real consumer to `dcscan_readonly` (e.g. a CERBER audit-read
--   path, or a future analytics/reporting service) and set its password
--   via a deploy-time secret, following the same pattern already used
--   for `POSTGRES_PASSWORD` in docker-compose.yml.
