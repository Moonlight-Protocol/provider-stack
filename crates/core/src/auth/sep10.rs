//! SEP-10 entity auth: two-leg Stellar transaction-signed challenge.
//!
//! Flow:
//!   1. `GET /stellar/auth?account=G...` → server builds a Stellar tx with one ManageData op
//!      ("provider-stack auth: <nonce>"), time-bounded, signed by the server account.
//!      Returns base64 XDR.
//!   2. Client signs the same envelope with their account key, posts back.
//!   3. `POST /stellar/auth { transaction: <xdr> }` → server verifies both signatures
//!      and timebounds, mints an entity JWT.
//!
//! Implementation is intentionally DIY against stellar-xdr + ed25519-dalek + stellar-strkey
//! per PLAN.md (no Rust SEP-10 crate exists).

use crate::error::CoreError;
use chrono::Utc;
use rand::RngCore;

/// Build a SEP-10 challenge transaction envelope.
///
/// Returns the base64 XDR of a `TransactionEnvelope` containing one `MANAGE_DATA`
/// op signed by the server account.
///
/// **Status**: scaffold — full XDR construction lands when integrated with stellar-xdr v27 builders.
pub fn build_challenge(
    _server_account_strkey: &str,
    _client_account_strkey: &str,
    _network_passphrase: &str,
    _ttl_secs: i64,
) -> Result<ChallengeTx, CoreError> {
    let mut nonce = [0u8; 48];
    rand::thread_rng().fill_bytes(&mut nonce);
    let now = Utc::now().timestamp();
    Ok(ChallengeTx {
        nonce: nonce.to_vec(),
        min_time: now,
        max_time: now + 900,
        envelope_xdr: String::new(), // TODO: stellar-xdr envelope construction
    })
}

/// Verify a signed SEP-10 challenge envelope.
///
/// Returns the entity's account strkey if both signatures and timebounds check out.
///
/// **Status**: scaffold — full envelope parse + dual-signature verify lands when integrated.
pub fn verify_signed_challenge(
    _envelope_xdr_b64: &str,
    _server_account_strkey: &str,
    _network_passphrase: &str,
) -> Result<String, CoreError> {
    Err(CoreError::InvalidSignature)
}

#[derive(Debug, Clone)]
pub struct ChallengeTx {
    pub nonce: Vec<u8>,
    pub min_time: i64,
    pub max_time: i64,
    pub envelope_xdr: String,
}
