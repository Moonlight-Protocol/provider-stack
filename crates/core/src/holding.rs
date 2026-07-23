//! Provider-held ("holding") UTXO keys for the send-via-email surface (#/pay-name).
//!
//! A payer sends funds to an email before the recipient necessarily exists as
//! an entity: the provider derives P-256 keys of its own to hold those UTXOs,
//! deterministically from one master secret plus the email, indexed from 0:
//!
//! ```text
//!   seed_i   = SHA-256("moonlight-holding-v1" ‖ 0x00 ‖ master ‖ 0x00 ‖ email ‖ 0x00 ‖ LE32(i))
//!   scalar_i = SHA-256("moonlight-holding-p256" ‖ 0x00 ‖ seed_i ‖ 0x00 ‖ LE32(attempt))
//! ```
//!
//! (`attempt` starts at 0 and bumps on the ~2⁻³² chance the candidate is not a
//! valid P-256 scalar.) Determinism replaces storage: unused keys are found by
//! scanning balances from index 0 — the contract's `-1` = never-existed
//! convention, same gap logic the frontend sweep uses — and attribution IS the
//! derivation: a key belongs to whichever email derives it. The sequence is
//! append-only ([funded/spent]* then [never-used]*) because keys are always
//! handed out first-unused-first.
//!
//! Spends of held UTXOs are signed here over the channel-auth `AuthPayload`
//! preimage — byte-for-byte the moonlight-sdk client construction
//! (`moonlight-sdk/src/utils/auth/build-auth-payload.ts`): contract-id strkey
//! bytes ‖ XDR(ScVal::Vec(conditions)) ‖ LE32(live_until_ledger), ECDSA
//! P-256/SHA-256, 64-byte r‖s.

use anyhow::{anyhow, Result};
use p256::ecdsa::signature::Signer as _;
use p256::ecdsa::{Signature, SigningKey};
use sha2::{Digest, Sha256};

const SEED_DOMAIN: &[u8] = b"moonlight-holding-v1";
const SCALAR_DOMAIN: &[u8] = b"moonlight-holding-p256";

/// One derived holding key: the signing half plus its 65-byte uncompressed
/// SEC1 public point (the on-chain UTXO id form).
pub struct HoldingKey {
    pub index: u32,
    pub signing: SigningKey,
    pub pubkey65: [u8; 65],
}

/// Derive the holding key at `index` for `email`. Deterministic in
/// (master, email, index); `email` is matched as the exact string the entity
/// registered (KYC `name` column — an email only by UI convention).
pub fn derive_holding_key(master: &str, email: &str, index: u32) -> Result<HoldingKey> {
    let mut h = Sha256::new();
    h.update(SEED_DOMAIN);
    h.update([0u8]);
    h.update(master.as_bytes());
    h.update([0u8]);
    h.update(email.as_bytes());
    h.update([0u8]);
    h.update(index.to_le_bytes());
    let seed = h.finalize();

    for attempt in 0u32..8 {
        let mut h = Sha256::new();
        h.update(SCALAR_DOMAIN);
        h.update([0u8]);
        h.update(seed);
        h.update([0u8]);
        h.update(attempt.to_le_bytes());
        let candidate = h.finalize();
        if let Ok(signing) = SigningKey::from_bytes(&candidate) {
            let point = signing.verifying_key().to_encoded_point(false);
            let pubkey65: [u8; 65] = point
                .as_bytes()
                .try_into()
                .map_err(|_| anyhow!("unexpected SEC1 point length"))?;
            return Ok(HoldingKey {
                index,
                signing,
                pubkey65,
            });
        }
    }
    Err(anyhow!(
        "no valid P-256 scalar after 8 attempts (index {index})"
    ))
}

/// The signed-payload preimage the channel-auth contract's `hash_payload`
/// SHA-256s: contract strkey bytes ‖ conditions ScVal XDR ‖ LE32(exp).
/// `conditions_scval_xdr` must be the XDR of the spend slot's `ScVal::Vec`
/// of conditions, passed through verbatim from the client's MLXDR.
pub fn auth_payload_preimage(
    channel_contract_strkey: &str,
    conditions_scval_xdr: &[u8],
    live_until_ledger: u32,
) -> Vec<u8> {
    let mut preimage =
        Vec::with_capacity(channel_contract_strkey.len() + conditions_scval_xdr.len() + 4);
    preimage.extend_from_slice(channel_contract_strkey.as_bytes());
    preimage.extend_from_slice(conditions_scval_xdr);
    preimage.extend_from_slice(&live_until_ledger.to_le_bytes());
    preimage
}

/// ECDSA P-256/SHA-256 over the preimage (hashing is internal, mirroring the
/// client's `crypto.subtle.sign`), low-S normalized, raw 64-byte r‖s.
pub fn sign_auth_payload(key: &SigningKey, preimage: &[u8]) -> [u8; 64] {
    let sig: Signature = key.sign(preimage);
    let sig = sig.normalize_s().unwrap_or(sig);
    sig.to_bytes().into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::ecdsa::signature::Verifier as _;

    #[test]
    fn derivation_is_deterministic_and_email_scoped() {
        let a1 = derive_holding_key("master", "a@x.com", 0).unwrap();
        let a2 = derive_holding_key("master", "a@x.com", 0).unwrap();
        let b = derive_holding_key("master", "b@x.com", 0).unwrap();
        let a_next = derive_holding_key("master", "a@x.com", 1).unwrap();
        assert_eq!(a1.pubkey65, a2.pubkey65);
        assert_ne!(a1.pubkey65, b.pubkey65);
        assert_ne!(a1.pubkey65, a_next.pubkey65);
        assert_eq!(a1.pubkey65[0], 0x04, "uncompressed SEC1 point");
    }

    #[test]
    fn signature_verifies_over_preimage() {
        let key = derive_holding_key("master", "a@x.com", 3).unwrap();
        let preimage = auth_payload_preimage("CDMOBPZ", b"\x00\x01\x02", 12345);
        let raw = sign_auth_payload(&key.signing, &preimage);
        let sig = Signature::from_slice(&raw).unwrap();
        key.signing.verifying_key().verify(&preimage, &sig).unwrap();
    }
}
