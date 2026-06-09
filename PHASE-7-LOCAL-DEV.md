# Phase 7 — local-dev integration

This document covers slotting `provider-stack` into the existing `local-dev/`
compose orchestration alongside the Deno `provider-platform`.

## What was added

- `local-dev/docker-compose.rust-provider.yml` — overlay compose file that adds
  a `provider-stack` service on port `${PROVIDER_STACK_PORT:-3030}` and a
  one-shot `provider-stack-db-bootstrap` sidecar that creates the
  `provider_stack_db` database in the shared Postgres.
- Reuses the same `db`, `jaeger`, and `stellar-local` services from
  `docker-compose.e2e.yml`. OTEL exports flow through Jaeger at `http://jaeger:4318`.

## Bringing it up

```bash
cd local-dev/

# Generate the runtime secrets the Rust stack needs.
docker run --rm provider-stack:smoke genkey               # → PP_SECRET_KEY=... PP_PUBLIC_KEY=...
docker run --rm provider-stack:smoke gen-service-secret   # → SERVICE_AUTH_SECRET=...

# Put them in .env alongside an OPERATOR_PUBLIC_KEY (the wallet that will
# obtain the SEP-43 dashboard JWT — typically your Freighter account on the
# standalone Stellar network).
cat > .env <<EOF
PP_SECRET_KEY=...
OPERATOR_PUBLIC_KEY=G...
SERVICE_AUTH_SECRET=...
EOF

# Bring the full stack up.
docker compose \
  -f docker-compose.e2e.yml \
  -f docker-compose.rust-provider.yml \
  up
```

`provider-stack` will be reachable at `http://localhost:3030`. The Deno
`provider-platform` container continues to run at `:3010` — the two operate
side-by-side, each with its own database.

## Pointing the testnet suites at provider-stack

```bash
PROVIDER_URL=http://localhost:3030 ./testnet/run-local.sh 1
```

## testnet/main.ts compatibility

`testnet/main.ts` exercises the SaaS-shape flow and calls
`POST /api/v1/dashboard/pp/register` (provider-platform line ~427). The Rust
`provider-stack` deliberately drops that endpoint — the PP key is provided via
the `PP_SECRET_KEY` env var at boot. Two adaptations exist:

1. **Fork the test for the Rust stack** — skip the registration step, derive
   the PP public key from the configured `PP_SECRET_KEY`, and continue from
   council join.
2. **Add a Rust-specific suite** (e.g. `testnet/main-rust.ts`) that exercises
   the same end-to-end flow against the single-PP shape. Council join, KYC
   self-register, SEP-10 entity auth, bundle submission all work as-is; only
   the PP setup step changes.

Either path is a follow-up — out of scope for the initial Rust stack repo.

## What's verified today

- The Rust binary boots cleanly under the e2e compose orchestration.
- `sqlx::migrate!()` runs the consolidated `0000_init.sql` against the shared
  Postgres on first boot.
- OTEL traces export to Jaeger at the shared sidecar.
- The 28 in-repo integration tests cover SEP-10, KYC, council, mempool,
  metrics, verifier, event-watcher, executor, and bundle-submission paths
  end-to-end.

## Known gaps before testnet/run-all.sh full pass

- testnet/main.ts adaptation as noted above (one-step diff).
- `provider-console` bundled SPA needs the `__CONSOLE_CONFIG__.apiBaseUrl`
  pointed at `/api/v1` (same origin) — already configured by default in
  `frontend/public/config.js` of this repo's frontend bundle, but the e2e
  compose also serves the console via its own container at a different port.
- WS events end-to-end test (verifier broadcast → bearer-authenticated WS
  subscriber) is not yet covered by an integration test in-repo. The
  underlying machinery is wired (verifier sends, WS upgrade reads
  Sec-WebSocket-Protocol bearer, subscribes to the broadcaster).
