//! Council discover / join / membership.
//!
//! **Status**: scaffold — endpoints wired; council-platform HTTP calls + on-chain join port next.

use crate::error::ApiError;
use crate::middleware_auth::OperatorAuth;
use crate::state::AppState;
use actix_web::{get, post, web, HttpResponse, Responder};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct DiscoverReq {
    pub council_url: String,
}

#[derive(Serialize)]
pub struct DiscoverRes {
    pub council_url: String,
    pub council_public_key: String,
    pub channel_auth_id: String,
}

#[post("/dashboard/council/discover")]
pub async fn post_discover(
    _state: web::Data<AppState>,
    _auth: OperatorAuth,
    _body: web::Json<DiscoverReq>,
) -> Result<impl Responder, ApiError> {
    Err::<HttpResponse, _>(ApiError::NotImplemented)
}

#[post("/providers/{pk}/council/join")]
pub async fn post_join(
    _state: web::Data<AppState>,
    _auth: OperatorAuth,
    _path: web::Path<String>,
) -> Result<impl Responder, ApiError> {
    Err::<HttpResponse, _>(ApiError::NotImplemented)
}

#[get("/providers/{pk}/council/membership")]
pub async fn get_membership(
    _state: web::Data<AppState>,
    _auth: OperatorAuth,
    _path: web::Path<String>,
) -> Result<impl Responder, ApiError> {
    Err::<HttpResponse, _>(ApiError::NotImplemented)
}

#[post("/providers/{pk}/council/membership")]
pub async fn post_membership(
    _state: web::Data<AppState>,
    _auth: OperatorAuth,
    _path: web::Path<String>,
) -> Result<impl Responder, ApiError> {
    Err::<HttpResponse, _>(ApiError::NotImplemented)
}
