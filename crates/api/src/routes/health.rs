//! `/api/v1/health` — liveness + a bounded DB connectivity probe.
//!
//! A static `{status:"ok"}` masked a dead DB during a prod incident, so the
//! endpoint now runs a time-boxed `SELECT 1` against the app pool and reports
//! `deps.db`, returning 503 when Postgres is unreachable/unresponsive. Mirrors
//! the Deno provider-platform/council `checkDbHealth` reference.

use crate::state::AppState;
use actix_web::{get, web, HttpResponse, Responder};
use provider_stack_persistence::PgPool;
use serde::Serialize;
use std::time::Duration;

/// Upper bound on the `/health` DB probe. Kept well under the container/compose
/// healthcheck `timeout` so a slow or unreachable DB resolves to a fast 503
/// instead of hanging the health gate.
const DB_HEALTH_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Serialize)]
struct Deps {
    db: &'static str,
}

#[derive(Serialize)]
struct Health {
    status: &'static str,
    version: &'static str,
    deps: Deps,
}

/// Bounded connectivity probe: a time-boxed `SELECT 1` against the app pool.
///
/// Returns `"ok"` only if the query settles successfully within the timeout;
/// any connection failure or a timeout (unresponsive DB) returns `"error"`.
/// Never panics.
///
/// `SELECT 1` checks connectivity only — it does not depend on any migrated
/// schema, so a still-migrating boot with a reachable Postgres still reports
/// `"ok"`. That keeps the deploy/health gate from flapping during startup;
/// only a genuinely unreachable/unresponsive DB reports `"error"`.
async fn check_db_health(pool: &PgPool) -> &'static str {
    match tokio::time::timeout(DB_HEALTH_TIMEOUT, sqlx::query("SELECT 1").execute(pool)).await {
        Ok(Ok(_)) => "ok",
        _ => "error",
    }
}

#[get("/health")]
pub async fn get_health(state: web::Data<AppState>) -> impl Responder {
    let db = check_db_health(&state.pool).await;
    let healthy = db == "ok";

    let body = Health {
        status: if healthy { "ok" } else { "error" },
        version: env!("CARGO_PKG_VERSION"),
        deps: Deps { db },
    };

    if healthy {
        HttpResponse::Ok().json(body)
    } else {
        HttpResponse::ServiceUnavailable().json(body)
    }
}
