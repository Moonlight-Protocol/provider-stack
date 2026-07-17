//! Route service registration, kept in one place so integration tests can mount the
//! same /api/v1 surface the binary serves.

use crate::routes;
use actix_web::web;

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("/api/v1")
            .service(routes::health::get_health)
            .service(routes::auth_dashboard::post_challenge)
            .service(routes::auth_dashboard::post_verify)
            .service(routes::dashboard_pp::get_info)
            .service(routes::auth_stellar::get_challenge)
            .service(routes::auth_stellar::post_verify)
            .service(routes::entities::post_challenge)
            .service(routes::entities::post_register)
            .service(routes::entities::get_list)
            .service(routes::council::post_discover)
            .service(routes::council::post_join)
            .service(routes::council::get_membership)
            .service(routes::council::post_membership)
            .service(routes::bundles::post_submit)
            .service(routes::bundles::list_entity_channels)
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
    .configure(routes::spa::configure);
}
