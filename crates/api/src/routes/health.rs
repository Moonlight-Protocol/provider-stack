use actix_web::{get, HttpResponse, Responder};
use serde::Serialize;

#[derive(Serialize)]
struct Health {
    status: &'static str,
    version: &'static str,
}

#[get("/health")]
pub async fn get_health() -> impl Responder {
    HttpResponse::Ok().json(Health {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
    })
}
