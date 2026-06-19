//! SEP-43 dashboard auth: single-message signed by the operator wallet.
//!
//! Flow:
//!   1. `POST /dashboard/auth/challenge { public_key }` → `{ nonce }`
//!      Server generates random nonce, stores it with TTL.
//!   2. Client signs the raw bytes of the nonce with their ed25519 wallet key.
//!   3. `POST /dashboard/auth/verify { public_key, nonce, signature }` → `{ token }`
//!      Server verifies signature against `OPERATOR_PUBLIC_KEY` (rejects others),
//!      consumes the nonce, mints an operator JWT.

use crate::error::CoreError;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use rand::RngCore;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const MAX_PENDING_NONCES: usize = 1024;

pub struct NonceStore {
    inner: Mutex<HashMap<String, NonceEntry>>,
    ttl: Duration,
}

struct NonceEntry {
    public_key: String,
    created_at: Instant,
}

impl NonceStore {
    pub fn new(ttl: Duration) -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            ttl,
        }
    }

    pub fn issue(&self, public_key: &str) -> String {
        let mut buf = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut buf);
        let nonce = B64.encode(buf);
        let mut guard = self.inner.lock().unwrap();
        self.gc(&mut guard);
        if guard.len() >= MAX_PENDING_NONCES {
            // evict oldest
            if let Some(oldest) = guard
                .iter()
                .min_by_key(|(_, e)| e.created_at)
                .map(|(k, _)| k.clone())
            {
                guard.remove(&oldest);
            }
        }
        guard.insert(
            nonce.clone(),
            NonceEntry {
                public_key: public_key.to_string(),
                created_at: Instant::now(),
            },
        );
        nonce
    }

    pub fn consume(&self, nonce: &str, expected_public_key: &str) -> bool {
        let mut guard = self.inner.lock().unwrap();
        self.gc(&mut guard);
        matches!(
            guard.remove(nonce),
            Some(entry)
                if entry.public_key == expected_public_key
                    && entry.created_at.elapsed() < self.ttl
        )
    }

    fn gc(&self, guard: &mut std::sync::MutexGuard<HashMap<String, NonceEntry>>) {
        let ttl = self.ttl;
        guard.retain(|_, e| e.created_at.elapsed() < ttl);
    }
}

/// Verify a Stellar wallet challenge signature over `nonce`.
///
/// Accepts the three encodings produced by the wallet ecosystem in current use
/// (mirrors `provider-platform/src/core/service/auth/verify-stellar-signature.ts`):
///   - SEP-43 (Freighter `signMessage`): `sign(SHA256(0x00 0x00 || len(msg) BE32 || msg))`
///   - SEP-53 (Stellar Signed Message):  `sign(SHA256("Stellar Signed Message:\n" + msg))`
///   - Raw bytes (SDK direct-sign):       `sign(base64-decoded nonce)`
///
/// `signature` may be hex (SEP-43 / SEP-53 / signMessage shape) or base64 (raw).
/// In the hashed shapes `msg` is the nonce string's UTF-8 bytes; in the raw shape
/// the nonce is treated as a base64 payload and the decoded bytes are signed
/// directly. The test harness uses the raw shape; real wallets produce the
/// hashed shapes.
pub fn verify_signature(
    public_key_strkey: &str,
    nonce: &str,
    signature: &str,
) -> Result<(), CoreError> {
    use sha2::{Digest, Sha256};

    let pk_bytes = stellar_strkey::ed25519::PublicKey::from_string(public_key_strkey)
        .map_err(|e| CoreError::Strkey(e.to_string()))?
        .0;
    let verifying_key = VerifyingKey::from_bytes(&pk_bytes)
        .map_err(|e| CoreError::Strkey(e.to_string()))?;

    let sig_bytes = if !signature.is_empty()
        && signature.chars().all(|c| c.is_ascii_hexdigit())
    {
        hex::decode(signature).map_err(|_| CoreError::InvalidSignature)?
    } else {
        B64.decode(signature).map_err(|_| CoreError::InvalidSignature)?
    };
    let sig = Signature::from_slice(&sig_bytes).map_err(|_| CoreError::InvalidSignature)?;

    let nonce_utf8 = nonce.as_bytes();

    // SEP-43: SHA256(0x00 0x00 || len(msg) BE32 || msg)
    let mut sep43_payload = Vec::with_capacity(6 + nonce_utf8.len());
    sep43_payload.extend_from_slice(&[0x00, 0x00]);
    sep43_payload.extend_from_slice(&(nonce_utf8.len() as u32).to_be_bytes());
    sep43_payload.extend_from_slice(nonce_utf8);
    let sep43_hash = Sha256::digest(&sep43_payload);
    if verifying_key.verify(&sep43_hash, &sig).is_ok() {
        return Ok(());
    }

    // SEP-53: SHA256("Stellar Signed Message:\n" + msg)
    let sep53_prefix = b"Stellar Signed Message:\n";
    let mut sep53_payload = Vec::with_capacity(sep53_prefix.len() + nonce_utf8.len());
    sep53_payload.extend_from_slice(sep53_prefix);
    sep53_payload.extend_from_slice(nonce_utf8);
    let sep53_hash = Sha256::digest(&sep53_payload);
    if verifying_key.verify(&sep53_hash, &sig).is_ok() {
        return Ok(());
    }

    // Raw: signature over the base64-decoded nonce bytes (test harness shape).
    if let Ok(raw_nonce) = B64.decode(nonce) {
        if verifying_key.verify(&raw_nonce, &sig).is_ok() {
            return Ok(());
        }
    }

    Err(CoreError::InvalidSignature)
}
