# provider-stack

Rust self-host Privacy Provider stack for Moonlight Protocol. Single Docker image; backend + frontend bundled.

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

## Build

```sh
make build       # npm build + cargo build
```

## Docker

```sh
docker build -t provider-stack:dev .
```

## Env vars

See PLAN.md §Env var surface.
