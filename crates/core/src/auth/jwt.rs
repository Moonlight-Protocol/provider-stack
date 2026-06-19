use crate::error::CoreError;
use chrono::Utc;
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum JwtKind {
    /// SEP-43 operator dashboard token. `sub` is the operator wallet pubkey.
    Operator,
    /// SEP-10 entity token. `sub` is the entity wallet pubkey.
    Entity,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JwtClaims {
    pub iss: String,
    pub sub: String,
    pub iat: i64,
    pub exp: i64,
    pub kind: JwtKind,
    pub session_id: String,
}

pub fn mint_token(
    secret: &[u8],
    issuer: &str,
    subject: &str,
    kind: JwtKind,
    session_id: &str,
    ttl_secs: i64,
) -> Result<String, CoreError> {
    let now = Utc::now().timestamp();
    let claims = JwtClaims {
        iss: issuer.to_string(),
        sub: subject.to_string(),
        iat: now,
        exp: now + ttl_secs,
        kind,
        session_id: session_id.to_string(),
    };
    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(secret),
    )
    .map_err(|e| CoreError::Jwt(e.to_string()))
}

pub fn verify_token(secret: &[u8], token: &str) -> Result<JwtClaims, CoreError> {
    let validation = Validation::default();
    let data = decode::<JwtClaims>(token, &DecodingKey::from_secret(secret), &validation)
        .map_err(|e| CoreError::Jwt(e.to_string()))?;
    Ok(data.claims)
}
