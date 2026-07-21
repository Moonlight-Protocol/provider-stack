//! SEP-10 entity auth — real implementation against `stellar-xdr` v27 + `ed25519-dalek` +
//! `stellar-strkey`. No external SEP-10 crate exists; this is the DIY path PLAN.md authorises.
//!
//! Two surfaces:
//! - `build_challenge`: server constructs a `TransactionEnvelope` containing one `ManageData`
//!   operation with a random nonce, signs it with the server (PP) key, returns base64 XDR.
//! - `verify_signed_envelope`: server validates a returned envelope — same structure, both
//!   server + client signatures present, timebounds current — and returns the client account.
//!
//! Sequence number is `0`, source account = server (PP) account. ManageData op carries
//! `source_account = Some(client_account)` per SEP-10 §4.2. Replay protection: bounded by
//! `max_time` window (client gets at most `ttl_secs` to return the signed envelope).

use crate::error::CoreError;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use chrono::Utc;
use ed25519_dalek::{Signature as Ed25519Sig, Signer, SigningKey, Verifier, VerifyingKey};
use rand::RngCore;
use sha2::{Digest, Sha256};
use stellar_xdr::{
    BytesM, DataValue, DecoratedSignature, Limits, ManageDataOp, Memo, MuxedAccount, Operation,
    OperationBody, Preconditions, ReadXdr, SequenceNumber, Signature, SignatureHint, String64,
    TimeBounds, TimePoint, Transaction, TransactionEnvelope, TransactionExt, TransactionV1Envelope,
    Uint256, VecM, WriteXdr,
};

/// Format a Stellar strkey value as a `std::string::String`.
///
/// `stellar_strkey::ed25519::PublicKey::to_string` returns `heapless::String<56>`; routing
/// through `Display` gives us a plain `String` without dragging in a `heapless` dep here.
fn strkey_to_string(pk: stellar_strkey::ed25519::PublicKey) -> String {
    format!("{pk}")
}

const NONCE_BYTES: usize = 48;
// SEP-10 mandates no fee — the challenge is never submitted, so the envelope
// carries 0. A nonzero value only makes wallets display a phantom fee in the
// signing prompt (and 100 was pure convention copied from the SDF reference).
const SEP10_FEE: u32 = 0;

/// Result of building a SEP-10 challenge envelope.
#[derive(Debug, Clone)]
pub struct ChallengeBuilt {
    /// Base64-encoded TransactionEnvelope XDR, ready to send to the client.
    pub envelope_xdr_b64: String,
    /// The network passphrase the client must use for signature payload hashing.
    pub network_passphrase: String,
}

/// Compute the network ID a Stellar wallet uses when signing transactions —
/// `sha256(network_passphrase)`.
pub fn network_id(passphrase: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(passphrase.as_bytes());
    h.finalize().into()
}

/// Parse a Stellar `G...` public key string into a 32-byte ed25519 pubkey.
fn pubkey_bytes_from_strkey(strkey: &str) -> Result<[u8; 32], CoreError> {
    Ok(stellar_strkey::ed25519::PublicKey::from_string(strkey)
        .map_err(|e| CoreError::Strkey(e.to_string()))?
        .0)
}

/// Parse a Stellar `S...` secret seed into an ed25519 signing key.
pub fn signing_key_from_seed(strkey: &str) -> Result<SigningKey, CoreError> {
    let seed = stellar_strkey::ed25519::PrivateKey::from_string(strkey)
        .map_err(|e| CoreError::Strkey(e.to_string()))?
        .0;
    Ok(SigningKey::from_bytes(&seed))
}

/// Build a SEP-10 challenge transaction envelope, signed with the server key.
pub fn build_challenge(
    server_signing_key: &SigningKey,
    server_account_strkey: &str,
    client_account_strkey: &str,
    network_passphrase: &str,
    domain: &str,
    ttl_secs: u64,
) -> Result<ChallengeBuilt, CoreError> {
    let server_pubkey = pubkey_bytes_from_strkey(server_account_strkey)?;
    let client_pubkey = pubkey_bytes_from_strkey(client_account_strkey)?;

    // Random nonce as base64 — SEP-10 §4.2 specifies the value is 64 base64-encoded chars
    // (== 48 raw bytes).
    let mut nonce_raw = [0u8; NONCE_BYTES];
    rand::thread_rng().fill_bytes(&mut nonce_raw);
    let nonce_b64 = B64.encode(nonce_raw);

    let data_name: String64 = format!("{domain} auth")
        .into_bytes()
        .try_into()
        .map_err(|e: stellar_xdr::Error| CoreError::XdrParse(e.to_string()))
        .map(String64)?;
    let data_value: DataValue = nonce_b64
        .into_bytes()
        .try_into()
        .map_err(|e: stellar_xdr::Error| CoreError::XdrParse(e.to_string()))
        .map(DataValue)?;

    let manage_data_op = Operation {
        source_account: Some(MuxedAccount::Ed25519(Uint256(client_pubkey))),
        body: OperationBody::ManageData(ManageDataOp {
            data_name,
            data_value: Some(data_value),
        }),
    };

    let now = Utc::now().timestamp() as u64;
    let cond = Preconditions::Time(TimeBounds {
        min_time: TimePoint(now),
        max_time: TimePoint(now + ttl_secs),
    });

    let tx = Transaction {
        source_account: MuxedAccount::Ed25519(Uint256(server_pubkey)),
        fee: SEP10_FEE,
        seq_num: SequenceNumber(0),
        cond,
        memo: Memo::None,
        operations: VecM::try_from(vec![manage_data_op])
            .map_err(|e| CoreError::XdrParse(e.to_string()))?,
        ext: TransactionExt::V0,
    };

    // Empty signatures initially; we attach the server sig below.
    let mut envelope = TransactionEnvelope::Tx(TransactionV1Envelope {
        tx,
        signatures: VecM::try_from(Vec::<DecoratedSignature>::new())
            .map_err(|e| CoreError::XdrParse(e.to_string()))?,
    });

    let net_id = network_id(network_passphrase);
    let server_sig = sign_envelope(server_signing_key, &envelope, &net_id)?;

    attach_signature(&mut envelope, server_sig)?;

    let envelope_xdr_b64 = envelope
        .to_xdr_base64(Limits::none())
        .map_err(|e| CoreError::XdrParse(e.to_string()))?;

    Ok(ChallengeBuilt {
        envelope_xdr_b64,
        network_passphrase: network_passphrase.to_string(),
    })
}

/// Result of verifying a signed SEP-10 envelope.
#[derive(Debug, Clone)]
pub struct VerifiedChallenge {
    /// Strkey (`G...`) of the entity whose signature we just verified.
    pub client_account_strkey: String,
}

/// Verify a returned SEP-10 envelope: structure, timebounds, both signatures present + valid.
pub fn verify_signed_envelope(
    envelope_xdr_b64: &str,
    server_account_strkey: &str,
    network_passphrase: &str,
    expected_domain: &str,
) -> Result<VerifiedChallenge, CoreError> {
    let envelope = TransactionEnvelope::from_xdr_base64(envelope_xdr_b64, Limits::none())
        .map_err(|e| CoreError::XdrParse(e.to_string()))?;

    let v1 = match &envelope {
        TransactionEnvelope::Tx(v1) => v1,
        _ => return Err(CoreError::InvalidChallenge),
    };
    let tx = &v1.tx;

    // Source must be server account; sequence 0 (per SEP-10 — challenge is never submitted)
    let server_pubkey = pubkey_bytes_from_strkey(server_account_strkey)?;
    if !muxed_eq(&tx.source_account, &server_pubkey) {
        return Err(CoreError::InvalidChallenge);
    }
    if tx.seq_num.0 != 0 {
        return Err(CoreError::InvalidChallenge);
    }

    // Exactly one ManageData op, sourced from the client.
    if tx.operations.len() != 1 {
        return Err(CoreError::InvalidChallenge);
    }
    let op = &tx.operations[0];
    let client_muxed = op
        .source_account
        .as_ref()
        .ok_or(CoreError::InvalidChallenge)?;
    let client_pubkey = muxed_to_ed25519(client_muxed)?;

    let manage_data = match &op.body {
        OperationBody::ManageData(md) => md,
        _ => return Err(CoreError::InvalidChallenge),
    };

    // data_name = "<expected_domain> auth"; data_value = 64-char base64 (48 random bytes)
    let expected_name = format!("{expected_domain} auth");
    if manage_data.data_name.0.as_slice() != expected_name.as_bytes() {
        return Err(CoreError::InvalidChallenge);
    }
    let dv = manage_data
        .data_value
        .as_ref()
        .ok_or(CoreError::InvalidChallenge)?;
    if dv.0.len() != 64 {
        return Err(CoreError::InvalidChallenge);
    }
    let decoded = B64
        .decode(dv.0.as_slice())
        .map_err(|_| CoreError::InvalidChallenge)?;
    if decoded.len() != NONCE_BYTES {
        return Err(CoreError::InvalidChallenge);
    }

    // Timebounds — current time must fall inside [min_time, max_time].
    let now = Utc::now().timestamp() as u64;
    match &tx.cond {
        Preconditions::Time(tb) => {
            if now < tb.min_time.0 || now > tb.max_time.0 {
                return Err(CoreError::InvalidChallenge);
            }
        }
        _ => return Err(CoreError::InvalidChallenge),
    };

    // Verify both signatures.
    let net_id = network_id(network_passphrase);
    let envelope_hash = envelope
        .hash(net_id)
        .map_err(|e| CoreError::XdrParse(e.to_string()))?;

    let server_verified = signatures_include(&v1.signatures, &envelope_hash, &server_pubkey)?;
    let client_verified = signatures_include(&v1.signatures, &envelope_hash, &client_pubkey)?;
    if !server_verified || !client_verified {
        return Err(CoreError::InvalidSignature);
    }

    let client_strkey = strkey_to_string(stellar_strkey::ed25519::PublicKey(client_pubkey));
    Ok(VerifiedChallenge {
        client_account_strkey: client_strkey,
    })
}

/// Attach a signed nonce to a signed envelope (client-side helper, used in tests + by the SDK).
pub fn attach_signature(
    envelope: &mut TransactionEnvelope,
    sig: DecoratedSignature,
) -> Result<(), CoreError> {
    match envelope {
        TransactionEnvelope::Tx(v1) => {
            let mut sigs: Vec<DecoratedSignature> = v1.signatures.to_vec();
            sigs.push(sig);
            v1.signatures = VecM::try_from(sigs).map_err(|e| CoreError::XdrParse(e.to_string()))?;
            Ok(())
        }
        _ => Err(CoreError::InvalidChallenge),
    }
}

/// Sign an envelope (its `network_id || signature_payload_tagged_transaction` hash) with the
/// given ed25519 key, returning a `DecoratedSignature` ready to attach.
pub fn sign_envelope(
    key: &SigningKey,
    envelope: &TransactionEnvelope,
    network_id: &[u8; 32],
) -> Result<DecoratedSignature, CoreError> {
    let envelope_hash = envelope
        .hash(*network_id)
        .map_err(|e| CoreError::XdrParse(e.to_string()))?;
    let sig: Ed25519Sig = key.sign(&envelope_hash);
    let pubkey = key.verifying_key().to_bytes();
    let hint: [u8; 4] = pubkey[28..32].try_into().expect("32-byte pubkey");
    let signature_bytes: BytesM<64> = sig
        .to_bytes()
        .to_vec()
        .try_into()
        .map_err(|e: stellar_xdr::Error| CoreError::XdrParse(e.to_string()))?;
    Ok(DecoratedSignature {
        hint: SignatureHint(hint),
        signature: Signature(signature_bytes),
    })
}

fn muxed_eq(muxed: &MuxedAccount, expected: &[u8; 32]) -> bool {
    matches!(muxed, MuxedAccount::Ed25519(Uint256(bytes)) if bytes == expected)
}

fn muxed_to_ed25519(muxed: &MuxedAccount) -> Result<[u8; 32], CoreError> {
    match muxed {
        MuxedAccount::Ed25519(Uint256(bytes)) => Ok(*bytes),
        _ => Err(CoreError::InvalidChallenge),
    }
}

/// Return true if any signature in `sigs` verifies `hash` under `pubkey`.
fn signatures_include(
    sigs: &VecM<DecoratedSignature, 20>,
    hash: &[u8; 32],
    pubkey: &[u8; 32],
) -> Result<bool, CoreError> {
    let verifying = VerifyingKey::from_bytes(pubkey).map_err(|_| CoreError::InvalidSignature)?;
    let hint: [u8; 4] = pubkey[28..32].try_into().expect("32-byte pubkey");
    for ds in sigs.iter() {
        if ds.hint.0 != hint {
            continue;
        }
        let sig_slice: &[u8] = ds.signature.0.as_slice();
        let Ok(sig) = Ed25519Sig::from_slice(sig_slice) else {
            continue;
        };
        if verifying.verify(hash, &sig).is_ok() {
            return Ok(true);
        }
    }
    Ok(false)
}

// Standard Stellar network passphrases.
pub const TESTNET_PASSPHRASE: &str = "Test SDF Network ; September 2015";
pub const MAINNET_PASSPHRASE: &str = "Public Global Stellar Network ; September 2015";
pub const STANDALONE_PASSPHRASE: &str = "Standalone Network ; February 2017";

pub fn passphrase_for(network: &str) -> &'static str {
    match network {
        "mainnet" => MAINNET_PASSPHRASE,
        "testnet" => TESTNET_PASSPHRASE,
        "local" | "standalone" => STANDALONE_PASSPHRASE,
        _ => STANDALONE_PASSPHRASE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Full roundtrip: build → sign as client → verify → assert client account returned.
    #[test]
    fn full_roundtrip_against_deterministic_keys() {
        let server_seed = [0x42u8; 32];
        let client_seed = [0x99u8; 32];

        let server_key = SigningKey::from_bytes(&server_seed);
        let client_key = SigningKey::from_bytes(&client_seed);

        let server_strkey = strkey_to_string(stellar_strkey::ed25519::PublicKey(
            server_key.verifying_key().to_bytes(),
        ));
        let client_strkey = strkey_to_string(stellar_strkey::ed25519::PublicKey(
            client_key.verifying_key().to_bytes(),
        ));

        let network = STANDALONE_PASSPHRASE;
        let domain = "smoke.local";

        let built = build_challenge(
            &server_key,
            &server_strkey,
            &client_strkey,
            network,
            domain,
            900,
        )
        .expect("build_challenge");

        // Client receives the envelope, parses, signs, returns.
        let mut envelope =
            TransactionEnvelope::from_xdr_base64(&built.envelope_xdr_b64, Limits::none())
                .expect("parse envelope");
        let net_id = network_id(network);
        let client_sig = sign_envelope(&client_key, &envelope, &net_id).expect("client sig");
        attach_signature(&mut envelope, client_sig).expect("attach");
        let signed_b64 = envelope.to_xdr_base64(Limits::none()).expect("re-encode");

        let verified =
            verify_signed_envelope(&signed_b64, &server_strkey, network, domain).expect("verify");
        assert_eq!(verified.client_account_strkey, client_strkey);
    }

    #[test]
    fn rejects_envelope_with_only_server_sig() {
        let server_key = SigningKey::from_bytes(&[0x42u8; 32]);
        let client_strkey = strkey_to_string(stellar_strkey::ed25519::PublicKey([0xAAu8; 32]));
        let server_strkey = strkey_to_string(stellar_strkey::ed25519::PublicKey(
            server_key.verifying_key().to_bytes(),
        ));

        let built = build_challenge(
            &server_key,
            &server_strkey,
            &client_strkey,
            STANDALONE_PASSPHRASE,
            "smoke.local",
            900,
        )
        .expect("build_challenge");

        // No client sig attached — verify must refuse.
        let err = verify_signed_envelope(
            &built.envelope_xdr_b64,
            &server_strkey,
            STANDALONE_PASSPHRASE,
            "smoke.local",
        )
        .unwrap_err();
        assert!(matches!(err, CoreError::InvalidSignature));
    }

    #[test]
    fn rejects_wrong_domain() {
        let server_key = SigningKey::from_bytes(&[0x42u8; 32]);
        let client_key = SigningKey::from_bytes(&[0x99u8; 32]);
        let server_strkey = strkey_to_string(stellar_strkey::ed25519::PublicKey(
            server_key.verifying_key().to_bytes(),
        ));
        let client_strkey = strkey_to_string(stellar_strkey::ed25519::PublicKey(
            client_key.verifying_key().to_bytes(),
        ));

        let built = build_challenge(
            &server_key,
            &server_strkey,
            &client_strkey,
            STANDALONE_PASSPHRASE,
            "smoke.local",
            900,
        )
        .expect("build_challenge");

        // Client signs + returns, but server now verifies with a different expected_domain.
        let mut envelope =
            TransactionEnvelope::from_xdr_base64(&built.envelope_xdr_b64, Limits::none()).unwrap();
        let net_id = network_id(STANDALONE_PASSPHRASE);
        let client_sig = sign_envelope(&client_key, &envelope, &net_id).unwrap();
        attach_signature(&mut envelope, client_sig).unwrap();
        let signed_b64 = envelope.to_xdr_base64(Limits::none()).unwrap();

        let err = verify_signed_envelope(
            &signed_b64,
            &server_strkey,
            STANDALONE_PASSPHRASE,
            "different.example",
        )
        .unwrap_err();
        assert!(matches!(err, CoreError::InvalidChallenge));
    }
}
