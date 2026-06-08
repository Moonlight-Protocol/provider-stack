//! provider-stack-api: HTTP layer. Exposes `run_server` for the binary and
//! `routing::configure` for integration tests under `tests/`.

pub mod cli;
pub mod error;
pub mod middleware_auth;
pub mod routes;
pub mod routing;
pub mod state;

use actix_cors::Cors;
use actix_web::{web, App, HttpServer};
use anyhow::Result;
use provider_stack_core::{
    auth::sep43::NonceStore, config::Config, events::EventBroadcaster, pipelines,
};
use provider_stack_persistence::{connect, run_migrations};
use std::sync::Arc;
use tracing_actix_web::TracingLogger;
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

pub async fn run_server() -> Result<()> {
    dotenvy::dotenv().ok();
    init_tracing();

    let config = Arc::new(Config::from_env()?);
    let pool = connect(&config.database_url).await?;
    run_migrations(&pool).await?;

    let events = EventBroadcaster::default();
    let nonces = Arc::new(NonceStore::new(config.challenge_ttl));

    let state = state::AppState {
        config: config.clone(),
        pool: pool.clone(),
        events: events.clone(),
        nonces,
    };

    let pipeline_handles = pipelines::spawn_all(config.clone(), pool, events);
    tracing::info!("spawned {} pipelines", pipeline_handles.len());

    let bind = format!("0.0.0.0:{}", config.port);
    tracing::info!(%bind, "listening");

    let allowed_origins = config.allowed_origins.clone();
    HttpServer::new(move || {
        let mut cors = Cors::default().allow_any_method().allow_any_header().max_age(3600);
        for origin in &allowed_origins {
            cors = cors.allowed_origin(origin);
        }
        App::new()
            .app_data(web::Data::new(state.clone()))
            .app_data(web::JsonConfig::default().limit(4 * 1024 * 1024))
            .wrap(TracingLogger::default())
            .wrap(cors)
            .configure(routing::configure)
    })
    .bind(&bind)?
    .run()
    .await?;

    Ok(())
}

pub fn init_tracing() {
    let env_filter =
        EnvFilter::try_from_env("LOG_LEVEL").unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::registry()
        .with(env_filter)
        .with(fmt::layer().json())
        .try_init();
}
