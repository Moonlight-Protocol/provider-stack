//! Runtime configuration loaded from environment per PLAN.md §Env var surface.

use anyhow::{anyhow, Result};
use std::env;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Config {
    pub port: u16,
    pub mode: String,
    pub log_level: String,
    pub database_url: String,
    pub network: String,
    pub network_fee: i64,
    pub stellar_rpc_url: String,
    pub transaction_expiration_offset: u32,
    pub event_watcher_interval: Duration,
    pub service_domain: String,
    pub service_auth_secret: String,
    pub provider_base_url: String,
    pub operator_public_key: String,
    pub pp_secret_key: String,
    pub challenge_ttl: Duration,
    pub session_ttl: Duration,
    pub mempool: MempoolConfig,
    pub bundle_max_operations: usize,
    pub allowed_origins: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct MempoolConfig {
    pub slot_capacity: usize,
    pub expensive_op_weight: u32,
    pub cheap_op_weight: u32,
    pub executor_interval: Duration,
    pub verifier_interval: Duration,
    pub ttl_check_interval: Duration,
    pub max_retry_attempts: u32,
    pub startup_max_bundle_age: Duration,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            port: env_or("PORT", "3000")?.parse()?,
            mode: env_or("MODE", "development")?,
            log_level: env_or("LOG_LEVEL", "INFO")?,
            database_url: required("DATABASE_URL")?,
            network: required("NETWORK")?,
            network_fee: required("NETWORK_FEE")?.parse()?,
            stellar_rpc_url: env::var("STELLAR_RPC_URL").unwrap_or_else(|_| {
                default_rpc_for_network(env::var("NETWORK").unwrap_or_default().as_str())
                    .to_string()
            }),
            transaction_expiration_offset: env_or("TRANSACTION_EXPIRATION_OFFSET", "1000")?
                .parse()?,
            event_watcher_interval: Duration::from_millis(
                env_or("EVENT_WATCHER_INTERVAL_MS", "30000")?.parse()?,
            ),
            service_domain: required("SERVICE_DOMAIN")?,
            service_auth_secret: required("SERVICE_AUTH_SECRET")?,
            provider_base_url: env::var("PROVIDER_BASE_URL").unwrap_or_else(|_| {
                format!("http://localhost:{}", env_or("PORT", "3000").unwrap())
            }),
            operator_public_key: required("OPERATOR_PUBLIC_KEY")?,
            pp_secret_key: required("PP_SECRET_KEY")?,
            challenge_ttl: Duration::from_secs(required("CHALLENGE_TTL")?.parse()?),
            session_ttl: Duration::from_secs(required("SESSION_TTL")?.parse()?),
            mempool: MempoolConfig {
                slot_capacity: required("MEMPOOL_SLOT_CAPACITY")?.parse()?,
                expensive_op_weight: required("MEMPOOL_EXPENSIVE_OP_WEIGHT")?.parse()?,
                cheap_op_weight: required("MEMPOOL_CHEAP_OP_WEIGHT")?.parse()?,
                executor_interval: Duration::from_millis(
                    required("MEMPOOL_EXECUTOR_INTERVAL_MS")?.parse()?,
                ),
                verifier_interval: Duration::from_millis(
                    required("MEMPOOL_VERIFIER_INTERVAL_MS")?.parse()?,
                ),
                ttl_check_interval: Duration::from_millis(
                    required("MEMPOOL_TTL_CHECK_INTERVAL_MS")?.parse()?,
                ),
                max_retry_attempts: required("MEMPOOL_MAX_RETRY_ATTEMPTS")?.parse()?,
                startup_max_bundle_age: Duration::from_millis(
                    env_or("MEMPOOL_STARTUP_MAX_BUNDLE_AGE_MS", "0")?.parse()?,
                ),
            },
            bundle_max_operations: required("BUNDLE_MAX_OPERATIONS")?.parse()?,
            allowed_origins: env::var("ALLOWED_ORIGINS")
                .unwrap_or_default()
                .split(',')
                .filter(|s| !s.is_empty())
                .map(|s| s.trim().to_string())
                .collect(),
        })
    }
}

fn required(key: &str) -> Result<String> {
    env::var(key).map_err(|_| anyhow!("required env var missing: {}", key))
}

fn env_or(key: &str, default: &str) -> Result<String> {
    Ok(env::var(key).unwrap_or_else(|_| default.to_string()))
}

fn default_rpc_for_network(network: &str) -> &'static str {
    match network {
        "mainnet" => "https://soroban-rpc.mainnet.stellar.gateway.fm",
        "testnet" => "https://soroban-testnet.stellar.org",
        "local" | "standalone" => "http://localhost:8000/soroban/rpc",
        _ => "http://localhost:8000/soroban/rpc",
    }
}
