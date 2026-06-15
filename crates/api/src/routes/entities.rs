//! KYC self-register — public (no JWT). Single signed-nonce flow, auto-approves on submit per
//! PR #107 of provider-platform. `/{pk}` in the URL is the PP public key; in single-PP shape it
//! must match the configured PP (env-derived); a mismatch returns 404.
//!
//! Wire shape matches Deno reference: request fields camelCase, signedChallenge is
//! `{ nonce, signature }`, success response wrapped in `{ data: ... }`.

use crate::envelope::Data;
use crate::error::ApiError;
use crate::state::AppState;
use crate::middleware_auth::OperatorAuth;
use actix_web::{get, post, web, HttpResponse, Responder};
use chrono::{DateTime, Utc};
use provider_stack_core::auth::sep43;
use provider_stack_persistence::{AccountRepo, AccountType, EntityRepo, EntityStatus, WalletUserRepo};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChallengeReq {
    pub pubkey: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChallengePayload {
    pub nonce: String,
}

#[post("/provider/entities/challenge")]
pub async fn post_challenge(
    state: web::Data<AppState>,
    body: web::Json<ChallengeReq>,
) -> Result<impl Responder, ApiError> {
    let nonce = state.nonces.issue(&body.pubkey);
    Ok(HttpResponse::Ok().json(Data::new(ChallengePayload { nonce })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignedChallenge {
    pub nonce: String,
    pub signature: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterReq {
    pub pubkey: String,
    pub name: Option<String>,
    pub jurisdictions: Option<Vec<String>>,
    pub signed_challenge: SignedChallenge,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterPayload {
    pub entity_id: String,
    pub status: &'static str,
}

#[post("/provider/entities")]
pub async fn post_register(
    state: web::Data<AppState>,
    body: web::Json<RegisterReq>,
) -> Result<impl Responder, ApiError> {
    sep43::verify_signature(&body.pubkey, &body.signed_challenge.nonce, &body.signed_challenge.signature)
        .map_err(|_| ApiError::Unauthorized)?;

    if !state.nonces.consume(&body.signed_challenge.nonce, &body.pubkey) {
        return Err(ApiError::Unauthorized);
    }

    let entities = EntityRepo::new(state.pool.clone());
    let accounts = AccountRepo::new(state.pool.clone());
    let wallet_users = WalletUserRepo::new(state.pool.clone());

    let entity_id = body.pubkey.clone();

    let entity = match entities.find_by_id(&entity_id).await? {
        Some(existing) if existing.status == EntityStatus::Approved => existing,
        Some(_) => {
            // Pre-existing row from record_interaction at the SEP-10 connect gate
            // (PR #118): UNVERIFIED with no identity fields. Approving here is also
            // when name + jurisdictions become known, so the operator entities view
            // doesn't have to handle null names for KYC-approved entities.
            entities
                .approve_with_identity(
                    &entity_id,
                    body.name.as_deref(),
                    body.jurisdictions.as_deref(),
                )
                .await?;
            entities.find_by_id(&entity_id).await?.expect("just updated")
        }
        None => {
            entities
                .create(
                    &entity_id,
                    EntityStatus::Approved,
                    body.name.as_deref(),
                    body.jurisdictions.as_deref(),
                    Some(&entity_id),
                )
                .await?
        }
    };

    let existing_accounts = accounts.list_by_entity(&entity.id).await?;
    if !existing_accounts.iter().any(|a| a.account_type == AccountType::User) {
        let account_id = Uuid::new_v4().to_string();
        accounts
            .create(&account_id, AccountType::User, &entity.id, Some(&entity.id))
            .await?;
    }

    wallet_users.find_or_create(&body.pubkey).await?;

    Ok(HttpResponse::Created().json(Data::new(RegisterPayload {
        entity_id: entity.id,
        status: "APPROVED",
    })))
}

/// One row of the operator-facing entities view — see PR #118
/// (`provider-platform/src/http/v1/entities/get.ts`).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntityListRow {
    pub pubkey: String,
    pub status: String,
    pub name: Option<String>,
    pub jurisdictions: Option<Vec<String>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// `GET /api/v1/provider/entities` — operator view of every entity that has
/// interacted with this PP (KYC-approved + unauthorized pubkeys recorded at
/// the SEP-10 connect + bundle-submit 403 gates).
#[get("/provider/entities")]
#[tracing::instrument(name = "P_ListEntities", skip_all)]
pub async fn get_list(
    state: web::Data<AppState>,
    _auth: OperatorAuth,
) -> Result<impl Responder, ApiError> {
    let entities = provider_stack_persistence::EntityRepo::new(state.pool.clone());
    let rows = entities.list_all_by_updated().await?;
    let payload: Vec<EntityListRow> = rows
        .into_iter()
        .map(|e| EntityListRow {
            pubkey: e.id,
            status: status_to_string(e.status),
            name: e.name,
            jurisdictions: e.jurisdictions,
            created_at: e.created_at,
            updated_at: e.updated_at,
        })
        .collect();
    Ok(HttpResponse::Ok().json(Data::new(payload)))
}

fn status_to_string(s: provider_stack_persistence::EntityStatus) -> String {
    use provider_stack_persistence::EntityStatus::*;
    match s {
        Unverified => "UNVERIFIED",
        Approved => "APPROVED",
        Pending => "PENDING",
        Blocked => "BLOCKED",
    }
    .to_string()
}
