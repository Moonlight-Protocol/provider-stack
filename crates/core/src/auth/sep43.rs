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
        match guard.remove(nonce) {
            Some(entry) if entry.public_key == expected_public_key
                && entry.created_at.elapsed() < self.ttl => true,
            _ => false,
        }
    }

    fn gc(&self, guard: &mut std::sync::MutexGuard<HashMap<String, NonceEntry>>) {
        let ttl = self.ttl;
        guard.retain(|_, e| e.created_at.elapsed() < ttl);
    }
}

/// Verify an SEP-43 signature over the nonce bytes by the given public key.
///
/// `public_key_strkey` is the G... Stellar address. `signature_b64` is base64'd 64-byte sig.
pub fn verify_signature(
    public_key_strkey: &str,
    nonce_b64: &str,
    signature_b64: &str,
) -> Result<(), CoreError> {
    let pk_bytes = stellar_strkey::ed25519::PublicKey::from_string(public_key_strkey)
        .map_err(|e| CoreError::Strkey(e.to_string()))?
        .0;
    let verifying_key = VerifyingKey::from_bytes(&pk_bytes)
        .map_err(|e| CoreError::Strkey(e.to_string()))?;

    let nonce_bytes = B64
        .decode(nonce_b64)
        .map_err(|_| CoreError::InvalidSignature)?;
    let sig_bytes = B64
        .decode(signature_b64)
        .map_err(|_| CoreError::InvalidSignature)?;
    let sig = Signature::from_slice(&sig_bytes).map_err(|_| CoreError::InvalidSignature)?;

    verifying_key
        .verify(&nonce_bytes, &sig)
        .map_err(|_| CoreError::InvalidSignature)
}
