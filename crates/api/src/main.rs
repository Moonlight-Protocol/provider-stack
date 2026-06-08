mod cli;
mod error;
mod middleware_auth;
mod routes;
mod state;

use actix_cors::Cors;
use actix_web::{web, App, HttpServer};
use anyhow::Result;
use provider_stack_core::{
    auth::sep43::NonceStore,
    config::Config,
    events::EventBroadcaster,
    pipelines,
};
use provider_stack_persistence::{connect, run_migrations};
use std::process::ExitCode;
use std::sync::Arc;
use tracing_actix_web::TracingLogger;
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

#[actix_web::main]
async fn main() -> ExitCode {
    // CLI subcommands short-circuit the server.
    let args: Vec<String> = std::env::args().collect();
    if args.len() >= 2 {
        return cli::run(&args[1]);
    }

    if let Err(e) = run_server().await {
        eprintln!("fatal: {e:?}");
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}

async fn run_server() -> Result<()> {
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
    let state_for_app = state.clone();
    HttpServer::new(move || {
        let mut cors = Cors::default()
            .allow_any_method()
            .allow_any_header()
            .max_age(3600);
        for origin in &allowed_origins {
            cors = cors.allowed_origin(origin);
        }
        App::new()
            .app_data(web::Data::new(state_for_app.clone()))
            .app_data(web::JsonConfig::default().limit(4 * 1024 * 1024))
            .wrap(TracingLogger::default())
            .wrap(cors)
            .service(
                web::scope("/api/v1")
                    .service(routes::health::get_health)
                    .service(routes::auth_dashboard::post_challenge)
                    .service(routes::auth_dashboard::post_verify)
                    .service(routes::auth_stellar::get_challenge)
                    .service(routes::auth_stellar::post_verify)
                    .service(routes::entities::post_challenge)
                    .service(routes::entities::post_register)
                    .service(routes::council::post_discover)
                    .service(routes::council::post_join)
                    .service(routes::council::get_membership)
                    .service(routes::council::post_membership)
                    .service(routes::bundles::post_submit)
                    .service(routes::bundles::list_entity)
                    .service(routes::bundles::get_entity_bundle)
                    .service(routes::operator::get_channels)
                    .service(routes::operator::get_mempool)
                    .service(routes::operator::get_treasury)
                    .service(routes::operator::get_utxos)
                    .service(routes::operator::get_transactions)
                    .service(routes::operator::get_transaction)
                    .service(routes::operator::get_bundles)
                    .service(routes::operator::get_bundle)
                    .service(routes::operator::get_audit_export)
                    .service(routes::operator::get_metrics)
                    .service(routes::events::ws_events),
            )
            // SPA falls through at the end — matches `/` and any unknown path → index.html
            .configure(routes::spa::configure)
    })
    .bind(&bind)?
    .run()
    .await?;

    Ok(())
}

fn init_tracing() {
    let env_filter = EnvFilter::try_from_env("LOG_LEVEL").unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::registry()
        .with(env_filter)
        .with(fmt::layer().json())
        .init();
}
