//! KYC self-register — public (no JWT). Single signed-nonce flow, auto-approves on submit per
//! PR #107 of provider-platform. `/{pk}` in the URL is the PP public key; in single-PP shape it
//! must match the configured PP (env-derived); a mismatch returns 404.

use crate::error::ApiError;
use crate::state::AppState;
use actix_web::{post, web, HttpResponse, Responder};
use ed25519_dalek::SigningKey;
use provider_stack_core::auth::sep10::signing_key_from_seed;
use provider_stack_core::auth::sep43;
use provider_stack_persistence::{AccountRepo, AccountType, EntityRepo, EntityStatus, WalletUserRepo};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Deserialize)]
pub struct ChallengeReq {
    pub pubkey: String,
}

#[derive(Serialize)]
pub struct ChallengeRes {
    pub nonce: String,
}

#[post("/providers/{pk}/entities/challenge")]
pub async fn post_challenge(
    state: web::Data<AppState>,
    path: web::Path<String>,
    body: web::Json<ChallengeReq>,
) -> Result<impl Responder, ApiError> {
    ensure_pk_is_this_pp(&state, &path)?;
    let nonce = state.nonces.issue(&body.pubkey);
    Ok(HttpResponse::Ok().json(ChallengeRes { nonce }))
}

#[derive(Deserialize)]
pub struct RegisterReq {
    pub pubkey: String,
    pub name: Option<String>,
    pub jurisdictions: Option<Vec<String>>,
    pub nonce: String,
    pub signature: String,
}

#[derive(Serialize)]
pub struct RegisterRes {
    pub entity_id: String,
    pub status: &'static str,
}

#[post("/providers/{pk}/entities")]
pub async fn post_register(
    state: web::Data<AppState>,
    path: web::Path<String>,
    body: web::Json<RegisterReq>,
) -> Result<impl Responder, ApiError> {
    ensure_pk_is_this_pp(&state, &path)?;

    // Verify ed25519 signature over the nonce.
    sep43::verify_signature(&body.pubkey, &body.nonce, &body.signature)
        .map_err(|_| ApiError::Unauthorized)?;

    // Consume nonce — one-shot, must match the pubkey we issued it to.
    if !state.nonces.consume(&body.nonce, &body.pubkey) {
        return Err(ApiError::Unauthorized);
    }

    let entities = EntityRepo::new(state.pool.clone());
    let accounts = AccountRepo::new(state.pool.clone());
    let wallet_users = WalletUserRepo::new(state.pool.clone());

    // The entity id is the wallet pubkey strkey — single source of identity.
    let entity_id = body.pubkey.clone();

    // Auto-approve on first submission (matches provider-platform PR #107 behavior).
    let entity = match entities.find_by_id(&entity_id).await? {
        Some(existing) if existing.status == EntityStatus::Approved => existing,
        Some(_) => {
            entities.set_status(&entity_id, EntityStatus::Approved).await?;
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

    // Ensure a USER account exists for this entity.
    let existing_accounts = accounts.list_by_entity(&entity.id).await?;
    if !existing_accounts.iter().any(|a| a.account_type == AccountType::User) {
        let account_id = Uuid::new_v4().to_string();
        accounts
            .create(&account_id, AccountType::User, &entity.id, Some(&entity.id))
            .await?;
    }

    // Track the wallet pubkey for ops cross-reference.
    wallet_users.find_or_create(&body.pubkey).await?;

    Ok(HttpResponse::Created().json(RegisterRes {
        entity_id: entity.id,
        status: "APPROVED",
    }))
}

/// In single-PP shape the URL `pk` must equal our configured PP. Anything else is a 404
/// (caller asked for a provider that does not live here).
fn ensure_pk_is_this_pp(state: &AppState, pk: &str) -> Result<(), ApiError> {
    let signing: SigningKey = signing_key_from_seed(&state.config.pp_secret_key)?;
    let this_pp = format!(
        "{}",
        stellar_strkey::ed25519::PublicKey(signing.verifying_key().to_bytes())
    );
    if pk != this_pp {
        return Err(ApiError::NotFound);
    }
    Ok(())
}
