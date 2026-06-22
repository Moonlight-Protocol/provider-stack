# provider-stack

Rust self-host Privacy Provider stack for Moonlight Protocol. Shipped as a `docker compose` stack (app image with backend + frontend bundled, plus its Postgres).

Spec: `pm-theahaco/docs/rust-provider-stack/PLAN.md`.

## Layout

```
crates/
  api/          actix-web binary
  core/         domain pipelines + SEP-10/SEP-43
  persistence/  sqlx repositories
  sdk/          soroban-client + soroban-core re-exports
migrations/     sqlx .sql migration files
frontend/       vanilla TS SPA (moved from provider-console)
```

## Self-host (recommended): docker compose

`docker compose up` brings up the app **plus its Postgres**, wired together. The
Stellar RPC stays **external** — point `STELLAR_RPC_URL` at a real testnet/mainnet
RPC in your `.env`.

```sh
cp .env.example .env                                 # then fill in the secrets

# generate the two key secrets (no Postgres needed):
docker compose run --rm --no-deps app genkey             # -> PP_SECRET_KEY + PP_PUBLIC_KEY
docker compose run --rm --no-deps app gen-service-secret # -> SERVICE_AUTH_SECRET

docker compose up --build                            # app + Postgres, migrations on boot
```

The app serves on `http://localhost:${PORT:-3000}` (UI + `GET /health`). Postgres
data persists in the `pgdata` volume. `.env.example` enumerates every variable
(required vs optional, secrets, and which point at the external RPC).

Observability is optional: the app emits OTLP traces but silently continues if
nothing is listening. Uncomment the `jaeger` service in `docker-compose.yml` and
set `OTEL_EXPORTER_OTLP_ENDPOINT=http://jaeger:4318` to collect them.

## Build

```sh
make build           # deno frontend build + cargo build
make docker          # build the single app image (compose uses this Dockerfile)
make compose-up      # cp .env.example -> .env if missing, then docker compose up --build
```

> The standalone single image (`make docker` / `docker build -t provider-stack:dev .`)
> still builds — compose builds the `app` service from the same `Dockerfile` — but
> it **cannot run on its own**: the app needs a Postgres it does not ship. Use the
> compose path above for a working stack.

## Env vars

See `.env.example` (authoritative, enumerated from `crates/core/src/config.rs`)
and PLAN.md §Env var surface.
