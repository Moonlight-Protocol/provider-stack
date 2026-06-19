//! provider-stack-api: HTTP layer. Exposes `run_server` for the binary and
//! `routing::configure` for integration tests under `tests/`.

pub mod cli;
pub mod envelope;
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

    let events = EventBroadcaster::new(256, config.operator_public_key.clone());
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
        let mut cors = Cors::default()
            .allow_any_method()
            .allow_any_header()
            .max_age(3600);
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
    use opentelemetry::trace::TracerProvider as _;
    use opentelemetry::KeyValue;
    use opentelemetry_otlp::{WithExportConfig, WithHttpConfig};
    use opentelemetry_sdk::propagation::TraceContextPropagator;
    use opentelemetry_sdk::Resource;

    // Install the W3C `traceparent` propagator so `tracing-actix-web` can
    // extract incoming HTTP trace IDs and the spans we emit become children
    // of the SDK-side root span — the OTEL verify check that gates Suite 2.
    opentelemetry::global::set_text_map_propagator(TraceContextPropagator::new());

    let env_filter =
        EnvFilter::try_from_env("LOG_LEVEL").unwrap_or_else(|_| EnvFilter::new("info"));

    // OTLP exporter — endpoint set by OTEL_EXPORTER_OTLP_ENDPOINT, service name by
    // OTEL_SERVICE_NAME (the verify-otel-local.ts script filters by serviceName).
    let otlp_endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
        .unwrap_or_else(|_| "http://localhost:4318".into());
    let service_name =
        std::env::var("OTEL_SERVICE_NAME").unwrap_or_else(|_| "provider-platform".into());
    let otel_layer = match opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_endpoint(format!("{otlp_endpoint}/v1/traces"))
        .with_protocol(opentelemetry_otlp::Protocol::HttpBinary)
        .with_http_client(reqwest::Client::new())
        .build()
    {
        Ok(exporter) => {
            // Use the async-runtime batch processor with Tokio so reqwest's async
            // export doesn't block actix workers (which `with_simple_exporter`
            // does, causing the request-handler thread pool to starve).
            use opentelemetry_sdk::runtime;
            use opentelemetry_sdk::trace::span_processor_with_async_runtime::BatchSpanProcessor;
            let batch = BatchSpanProcessor::builder(exporter, runtime::Tokio).build();
            let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
                .with_span_processor(batch)
                .with_resource(
                    Resource::builder()
                        .with_attribute(KeyValue::new("service.name", service_name.clone()))
                        .build(),
                )
                .build();
            opentelemetry::global::set_tracer_provider(provider.clone());
            Some(tracing_opentelemetry::layer().with_tracer(provider.tracer("provider-stack")))
        }
        Err(e) => {
            eprintln!("[init_tracing] otlp exporter init failed: {e}; continuing without OTEL");
            None
        }
    };

    let registry = tracing_subscriber::registry()
        .with(env_filter)
        .with(fmt::layer().json());
    let _ = if let Some(layer) = otel_layer {
        registry.with(layer).try_init()
    } else {
        registry.try_init()
    };
}
