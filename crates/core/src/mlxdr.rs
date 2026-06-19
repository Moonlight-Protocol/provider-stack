//! Moonlight custom XDR (MLXDR) decoder for bundle operations.
//!
//! Wire format per `moonlight-sdk/src/custom-xdr/`:
//! ```text
//!   base64( [0x30, 0xb0, type_byte] || raw_xdr(ScVec([operation_scval, signature_scval])) )
//! ```
//! - First two bytes are the `ML` prefix.
//! - Third byte is the operation type tag (`0x04 .. 0x07` for Create / Spend / Deposit / Withdraw).
//! - Remaining bytes are a stellar-xdr ScVal containing a vector of two ScVals: the
//!   operation payload + its signature (which is empty for Create and may be empty for
//!   Spend / Deposit if not yet signed).
//!
//! Each operation's payload ScVec layout (per `moonlight-sdk/src/operation/index.ts:736+`):
//! ```text
//!   Create   = [ScBytes(utxo: 65), ScI128(amount)]
//!   Spend    = [ScBytes(utxo: 65), ScVec(conditions)]
//!   Deposit  = [ScAddress(pubkey), ScI128(amount), ScVec(conditions)]
//!   Withdraw = [ScAddress(pubkey), ScI128(amount), ScVec(conditions)]
//! ```
//! Spend operations carry no amount — the value being spent is looked up on-chain.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use soroban_client::xdr::{
    AccountId, Limits, PublicKey, ReadXdr, ScAddress, ScVal, SorobanAuthorizationEntry, Uint256,
};
use thiserror::Error;

const ML_PREFIX: [u8; 2] = [0x30, 0xb0];

/// Operation tag byte at index 2 of the decoded MLXDR.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationKind {
    Create,
    Spend,
    Deposit,
    Withdraw,
}

impl OperationKind {
    pub fn from_type_byte(b: u8) -> Result<Self, MlxdrError> {
        match b {
            0x04 => Ok(Self::Create),
            0x05 => Ok(Self::Spend),
            0x06 => Ok(Self::Deposit),
            0x07 => Ok(Self::Withdraw),
            other => Err(MlxdrError::UnknownTypeByte(other)),
        }
    }
}

#[derive(Debug, Clone)]
pub struct DecodedOperation {
    pub kind: OperationKind,
    /// 65-byte UTXO pubkey for Create / Spend; empty for Deposit / Withdraw.
    pub utxo: Vec<u8>,
    /// Amount (i128 from ScI128) for Create / Deposit / Withdraw; 0 for Spend
    /// (the real value lives on-chain).
    pub amount: i128,
}

#[derive(Debug, Error)]
pub enum MlxdrError {
    #[error("base64 decode: {0}")]
    Base64(String),

    #[error("MLXDR too short (need ≥3 bytes for prefix + type)")]
    TooShort,

    #[error("missing ML prefix (expected 0x30 0xb0)")]
    MissingPrefix,

    #[error("unknown type byte: 0x{0:02x}")]
    UnknownTypeByte(u8),

    #[error("xdr parse: {0}")]
    Xdr(String),

    #[error("operation payload had wrong shape: {0}")]
    BadShape(&'static str),
}

/// Decode an MLXDR string into a typed operation summary used for classification + fee calc.
pub fn decode(mlxdr_b64: &str) -> Result<DecodedOperation, MlxdrError> {
    let bytes = B64
        .decode(mlxdr_b64)
        .map_err(|e| MlxdrError::Base64(e.to_string()))?;
    if bytes.len() < 3 {
        return Err(MlxdrError::TooShort);
    }
    if bytes[0..2] != ML_PREFIX {
        return Err(MlxdrError::MissingPrefix);
    }
    let kind = OperationKind::from_type_byte(bytes[2])?;

    // Remainder is stellar-xdr of ScVal::Vec([operation_scval, signature_scval]).
    let outer =
        ScVal::from_xdr(&bytes[3..], Limits::none()).map_err(|e| MlxdrError::Xdr(e.to_string()))?;
    let scvec = match outer {
        ScVal::Vec(Some(v)) => v.0,
        _ => return Err(MlxdrError::BadShape("outer must be ScVec(Some(..))")),
    };
    if scvec.is_empty() {
        return Err(MlxdrError::BadShape("outer ScVec is empty"));
    }
    let op_scval = &scvec[0];
    let payload = match op_scval {
        ScVal::Vec(Some(v)) => v.0.as_slice(),
        _ => return Err(MlxdrError::BadShape("operation payload must be ScVec")),
    };

    let (utxo, amount) = match kind {
        OperationKind::Create => {
            if payload.len() != 2 {
                return Err(MlxdrError::BadShape("Create payload must be 2 fields"));
            }
            let utxo =
                scbytes(&payload[0]).ok_or(MlxdrError::BadShape("Create.utxo must be ScBytes"))?;
            let amount =
                sci128(&payload[1]).ok_or(MlxdrError::BadShape("Create.amount must be ScI128"))?;
            (utxo, amount)
        }
        OperationKind::Spend => {
            if payload.is_empty() {
                return Err(MlxdrError::BadShape("Spend payload must have ≥1 field"));
            }
            let utxo =
                scbytes(&payload[0]).ok_or(MlxdrError::BadShape("Spend.utxo must be ScBytes"))?;
            (utxo, 0i128)
        }
        OperationKind::Deposit | OperationKind::Withdraw => {
            // [address, amount, conditions]
            if payload.len() < 2 {
                return Err(MlxdrError::BadShape(
                    "Deposit/Withdraw payload must have ≥2 fields",
                ));
            }
            let amount =
                sci128(&payload[1]).ok_or(MlxdrError::BadShape("amount must be ScI128"))?;
            (Vec::new(), amount)
        }
    };

    Ok(DecodedOperation { kind, utxo, amount })
}

fn scbytes(v: &ScVal) -> Option<Vec<u8>> {
    if let ScVal::Bytes(soroban_client::xdr::ScBytes(b)) = v {
        Some(b.as_slice().to_vec())
    } else {
        None
    }
}

fn sci128(v: &ScVal) -> Option<i128> {
    if let ScVal::I128(soroban_client::xdr::Int128Parts { hi, lo }) = v {
        Some(((*hi as i128) << 64) | (*lo as i128))
    } else {
        None
    }
}

/// Bundle-level classification result.
#[derive(Debug, Clone, Default)]
pub struct Classified {
    pub create: Vec<DecodedOperation>,
    pub spend: Vec<DecodedOperation>,
    pub deposit: Vec<DecodedOperation>,
    pub withdraw: Vec<DecodedOperation>,
}

/// Aggregate MLXDR slots into a single `ChannelOperation` ScVal — the argument the
/// privacy-channel contract's `transact(op: ChannelOperation)` entrypoint expects.
///
/// `ChannelOperation` is a Soroban `#[contracttype]` struct with four `Vec<...>` fields:
///   - `create`:   `Vec<(BytesN<65>, i128)>`
///   - `deposit`:  `Vec<(Address, i128, Vec<Condition>)>`
///   - `spend`:    `Vec<(BytesN<65>, Vec<Condition>)>`
///   - `withdraw`: `Vec<(Address, i128, Vec<Condition>)>`
///
/// In ScVal, the struct is `ScMap` keyed by symbol field name (alphabetic order), each
/// value an `ScVec` of tuple-ScVecs. Every MLXDR slot's *operation payload* (the first
/// element of the outer ScVec) is already shaped as the matching tuple — so we extract
/// it verbatim and push it into the bucket for its type byte.
/// One user-pre-signed Soroban auth entry lifted out of an MLXDR slot.
///
/// Each Deposit / Withdraw MLXDR slot carries the depositor's pre-built
/// `SorobanAuthorizationEntry` in `outer_scvec[1]`. The user built it via
/// `MoonlightOperation.signWithEd25519` on the client (see
/// `moonlight-sdk/src/operation/index.ts:518`), committing to a specific
/// nonce + signature_expiration_ledger and signing the matching preimage.
/// The provider must relay it verbatim — recomputing the preimage with a
/// different nonce would invalidate the signature.
#[derive(Debug, Clone)]
pub struct UserSignedSlot {
    /// 32-byte Ed25519 pubkey of the depositor / withdrawer.
    pub account_pk32: [u8; 32],
    /// The user-signed authorization entry, ready to splice into
    /// `InvokeHostFunctionOp.auth`.
    pub auth_entry: SorobanAuthorizationEntry,
}

/// Extract every user-pre-signed `SorobanAuthorizationEntry` from a bundle's
/// MLXDR slots. Mirrors the Deno SDK's `_extSignatures` map at
/// `moonlight-sdk/src/transaction-builder/index.ts:64`.
///
/// Slot signature shape (from `moonlight-sdk/src/custom-xdr/index.ts:155-173`):
/// - Deposit / Withdraw signed:   `ScVal::Vec([ScVal::Bytes(<raw XDR of SorobanAuthorizationEntry>)])`
/// - Anything else (Create, unsigned, UTXO P256 sig for Spend): skipped here.
pub fn extract_user_signed_entries(
    mlxdr_strings: &[&str],
) -> Result<Vec<UserSignedSlot>, MlxdrError> {
    let mut out: Vec<UserSignedSlot> = Vec::new();
    for s in mlxdr_strings {
        let bytes = B64
            .decode(s)
            .map_err(|e| MlxdrError::Base64(e.to_string()))?;
        if bytes.len() < 3 {
            return Err(MlxdrError::TooShort);
        }
        if bytes[0..2] != ML_PREFIX {
            return Err(MlxdrError::MissingPrefix);
        }
        let kind = OperationKind::from_type_byte(bytes[2])?;
        if !matches!(kind, OperationKind::Deposit | OperationKind::Withdraw) {
            continue;
        }
        let outer = ScVal::from_xdr(&bytes[3..], Limits::none())
            .map_err(|e| MlxdrError::Xdr(e.to_string()))?;
        let scvec = match outer {
            ScVal::Vec(Some(v)) => v.0,
            _ => return Err(MlxdrError::BadShape("outer must be ScVec(Some(..))")),
        };
        if scvec.len() < 2 {
            continue;
        }
        // scvec[1] is the signature wrapper. For a signed Ed25519 deposit:
        // ScVal::Vec([ScVal::Bytes(<raw SorobanAuthorizationEntry XDR>)]).
        let sig_vec = match &scvec[1] {
            ScVal::Vec(Some(v)) if !v.0.is_empty() => v.0.clone(),
            _ => continue,
        };
        let entry_bytes = match &sig_vec[0] {
            ScVal::Bytes(b) => b.0.as_slice().to_vec(),
            _ => continue,
        };
        let auth_entry = SorobanAuthorizationEntry::from_xdr(&entry_bytes, Limits::none())
            .map_err(|e| MlxdrError::Xdr(format!("user-signed auth entry XDR: {e}")))?;

        // Depositor pubkey lives in scvec[0] payload[0] as ScAddress::Account(ed25519).
        let payload = match &scvec[0] {
            ScVal::Vec(Some(v)) => v.0.clone(),
            _ => return Err(MlxdrError::BadShape("op payload must be Vec")),
        };
        if payload.is_empty() {
            return Err(MlxdrError::BadShape("op payload empty"));
        }
        let account_pk32 = match &payload[0] {
            ScVal::Address(ScAddress::Account(AccountId(PublicKey::PublicKeyTypeEd25519(
                Uint256(bytes),
            )))) => *bytes,
            _ => continue,
        };
        out.push(UserSignedSlot {
            account_pk32,
            auth_entry,
        });
    }
    Ok(out)
}

/// One UTXO-spend P256 signature lifted out of a Spend MLXDR slot.
///
/// Spend slots carry the UTXO owner's pre-signed P256 signature over the
/// channel-auth contract's `AuthPayload` (conditions + live_until_ledger).
/// The provider must add a matching `{ SignerKey::P256(utxo) → (Signature::P256(sig), exp) }`
/// entry to the channel-auth `Signatures` map alongside the Provider entry —
/// mirrors moonlight-sdk's `buildSignaturesXDR` spend-signers loop at
/// `moonlight-sdk/src/transaction-builder/signatures/signatures-xdr.ts:26-46`.
#[derive(Debug, Clone)]
pub struct UserSpendSignature {
    /// 65-byte UTXO public key (P-256 uncompressed point form).
    pub utxo_pk65: [u8; 65],
    /// 64-byte P-256 signature over the per-spend AuthPayload hash.
    pub sig: [u8; 64],
    /// Ledger expiration the signer committed to (becomes the u32 in `(Signature, u32)`).
    pub exp: u32,
}

/// Extract every UTXO-spend P256 signature from a bundle's Spend MLXDR slots.
///
/// Slot signature shape (from `moonlight-sdk/src/custom-xdr/index.ts:166-169`):
/// `ScVal::Vec([ScVal::I128(exp), ScVal::Bytes(sig)])`.
pub fn extract_user_spend_signatures(
    mlxdr_strings: &[&str],
) -> Result<Vec<UserSpendSignature>, MlxdrError> {
    use soroban_client::xdr::Int128Parts;
    let mut out: Vec<UserSpendSignature> = Vec::new();
    for s in mlxdr_strings {
        let bytes = B64
            .decode(s)
            .map_err(|e| MlxdrError::Base64(e.to_string()))?;
        if bytes.len() < 3 {
            return Err(MlxdrError::TooShort);
        }
        if bytes[0..2] != ML_PREFIX {
            return Err(MlxdrError::MissingPrefix);
        }
        let kind = OperationKind::from_type_byte(bytes[2])?;
        if !matches!(kind, OperationKind::Spend) {
            continue;
        }
        let outer = ScVal::from_xdr(&bytes[3..], Limits::none())
            .map_err(|e| MlxdrError::Xdr(e.to_string()))?;
        let scvec = match outer {
            ScVal::Vec(Some(v)) => v.0,
            _ => return Err(MlxdrError::BadShape("outer must be ScVec(Some(..))")),
        };
        if scvec.len() < 2 {
            continue;
        }
        // scvec[1] = [ScVal::I128(exp), ScVal::Bytes(sig)]
        let sig_vec = match &scvec[1] {
            ScVal::Vec(Some(v)) if v.0.len() >= 2 => v.0.clone(),
            _ => continue,
        };
        let exp_u32 = match &sig_vec[0] {
            ScVal::I128(Int128Parts { lo, .. }) => (*lo & 0xFFFF_FFFF) as u32,
            _ => continue,
        };
        let sig_bytes = match &sig_vec[1] {
            ScVal::Bytes(b) => b.0.as_slice().to_vec(),
            _ => continue,
        };
        if sig_bytes.len() != 64 {
            continue;
        }
        let mut sig = [0u8; 64];
        sig.copy_from_slice(&sig_bytes);

        // scvec[0] = op payload [ScBytes(utxo:65), ScVec(conditions)]
        let payload = match &scvec[0] {
            ScVal::Vec(Some(v)) => v.0.clone(),
            _ => return Err(MlxdrError::BadShape("Spend payload must be Vec")),
        };
        let utxo_bytes = match payload.first() {
            Some(ScVal::Bytes(b)) => b.0.as_slice().to_vec(),
            _ => continue,
        };
        if utxo_bytes.len() != 65 {
            continue;
        }
        let mut utxo_pk65 = [0u8; 65];
        utxo_pk65.copy_from_slice(&utxo_bytes);

        out.push(UserSpendSignature {
            utxo_pk65,
            sig,
            exp: exp_u32,
        });
    }
    Ok(out)
}

/// Build an extra `Create` operation tuple to inject as the PP's fee UTXO.
/// The privacy channel contract enforces `total_deposit + spend = total_create + total_withdraw`;
/// the difference (the fee) has to land somewhere or it's `UnbalancedBundle`. The Deno reference
/// creates a fresh OPEX UTXO under the PP's control for exactly this amount.
///
/// `utxo_pubkey` must be 65 bytes (P-256 uncompressed point shape: `0x04 || X || Y`).
pub fn build_fee_create_op(utxo_pubkey: &[u8; 65], fee: i128) -> Result<ScVal, MlxdrError> {
    use soroban_client::xdr::{Int128Parts, ScBytes, ScVec, VecM};
    let utxo_bytes: soroban_client::xdr::BytesM = utxo_pubkey
        .to_vec()
        .try_into()
        .map_err(|e: soroban_client::xdr::Error| MlxdrError::Xdr(format!("BytesM utxo: {e}")))?;
    let amount = Int128Parts {
        hi: (fee >> 64) as i64,
        lo: ((fee as u128) & 0xFFFF_FFFF_FFFF_FFFF) as u64,
    };
    let tuple = vec![ScVal::Bytes(ScBytes(utxo_bytes)), ScVal::I128(amount)];
    let vecm: VecM<ScVal> =
        VecM::try_from(tuple).map_err(|e| MlxdrError::Xdr(format!("VecM fee tuple: {e}")))?;
    Ok(ScVal::Vec(Some(ScVec(vecm))))
}

/// Like [`aggregate_to_channel_operation`] but also appends an extra Create op
/// (typically the PP's fee UTXO) to the create bucket so the bundle balances on-chain.
pub fn aggregate_to_channel_operation_with_fee_create(
    mlxdr_strings: &[&str],
    extra_create: ScVal,
) -> Result<ScVal, MlxdrError> {
    use soroban_client::xdr::{ScMap, ScMapEntry, ScSymbol, ScVec, StringM, VecM};

    let mut create_ops: Vec<ScVal> = Vec::new();
    let mut deposit_ops: Vec<ScVal> = Vec::new();
    let mut spend_ops: Vec<ScVal> = Vec::new();
    let mut withdraw_ops: Vec<ScVal> = Vec::new();

    for s in mlxdr_strings {
        let bytes = B64
            .decode(s)
            .map_err(|e| MlxdrError::Base64(e.to_string()))?;
        if bytes.len() < 3 {
            return Err(MlxdrError::TooShort);
        }
        if bytes[0..2] != ML_PREFIX {
            return Err(MlxdrError::MissingPrefix);
        }
        let kind = OperationKind::from_type_byte(bytes[2])?;
        let outer = ScVal::from_xdr(&bytes[3..], Limits::none())
            .map_err(|e| MlxdrError::Xdr(e.to_string()))?;
        let scvec = match outer {
            ScVal::Vec(Some(v)) => v.0,
            _ => return Err(MlxdrError::BadShape("outer must be ScVec(Some(..))")),
        };
        if scvec.is_empty() {
            return Err(MlxdrError::BadShape("outer ScVec is empty"));
        }
        let op_scval = scvec[0].clone();
        match kind {
            OperationKind::Create => create_ops.push(op_scval),
            OperationKind::Deposit => deposit_ops.push(op_scval),
            OperationKind::Spend => spend_ops.push(op_scval),
            OperationKind::Withdraw => withdraw_ops.push(op_scval),
        }
    }
    create_ops.push(extra_create);

    fn vec_of(ops: Vec<ScVal>) -> Result<ScVal, MlxdrError> {
        let vecm: VecM<ScVal> =
            VecM::try_from(ops).map_err(|e| MlxdrError::Xdr(format!("VecM build: {e}")))?;
        Ok(ScVal::Vec(Some(ScVec(vecm))))
    }
    fn sym(s: &'static str) -> Result<ScVal, MlxdrError> {
        let strm: StringM<32> = StringM::try_from(s.as_bytes().to_vec())
            .map_err(|e| MlxdrError::Xdr(format!("Symbol build: {e}")))?;
        Ok(ScVal::Symbol(ScSymbol(strm)))
    }

    let entries = vec![
        ScMapEntry {
            key: sym("create")?,
            val: vec_of(create_ops)?,
        },
        ScMapEntry {
            key: sym("deposit")?,
            val: vec_of(deposit_ops)?,
        },
        ScMapEntry {
            key: sym("spend")?,
            val: vec_of(spend_ops)?,
        },
        ScMapEntry {
            key: sym("withdraw")?,
            val: vec_of(withdraw_ops)?,
        },
    ];
    let map: VecM<ScMapEntry> =
        VecM::try_from(entries).map_err(|e| MlxdrError::Xdr(format!("ScMap build: {e}")))?;
    Ok(ScVal::Map(Some(ScMap(map))))
}

pub fn aggregate_to_channel_operation(mlxdr_strings: &[&str]) -> Result<ScVal, MlxdrError> {
    use soroban_client::xdr::{ScMap, ScMapEntry, ScSymbol, ScVec, StringM, VecM};

    let mut create_ops: Vec<ScVal> = Vec::new();
    let mut deposit_ops: Vec<ScVal> = Vec::new();
    let mut spend_ops: Vec<ScVal> = Vec::new();
    let mut withdraw_ops: Vec<ScVal> = Vec::new();

    for s in mlxdr_strings {
        let bytes = B64
            .decode(s)
            .map_err(|e| MlxdrError::Base64(e.to_string()))?;
        if bytes.len() < 3 {
            return Err(MlxdrError::TooShort);
        }
        if bytes[0..2] != ML_PREFIX {
            return Err(MlxdrError::MissingPrefix);
        }
        let kind = OperationKind::from_type_byte(bytes[2])?;
        let outer = ScVal::from_xdr(&bytes[3..], Limits::none())
            .map_err(|e| MlxdrError::Xdr(e.to_string()))?;
        let scvec = match outer {
            ScVal::Vec(Some(v)) => v.0,
            _ => return Err(MlxdrError::BadShape("outer must be ScVec(Some(..))")),
        };
        if scvec.is_empty() {
            return Err(MlxdrError::BadShape("outer ScVec is empty"));
        }
        let op_scval = scvec[0].clone();
        match kind {
            OperationKind::Create => create_ops.push(op_scval),
            OperationKind::Deposit => deposit_ops.push(op_scval),
            OperationKind::Spend => spend_ops.push(op_scval),
            OperationKind::Withdraw => withdraw_ops.push(op_scval),
        }
    }

    fn vec_of(ops: Vec<ScVal>) -> Result<ScVal, MlxdrError> {
        let vecm: VecM<ScVal> =
            VecM::try_from(ops).map_err(|e| MlxdrError::Xdr(format!("VecM build: {e}")))?;
        Ok(ScVal::Vec(Some(ScVec(vecm))))
    }

    fn sym(s: &'static str) -> Result<ScVal, MlxdrError> {
        let strm: StringM<32> = StringM::try_from(s.as_bytes().to_vec())
            .map_err(|e| MlxdrError::Xdr(format!("Symbol build: {e}")))?;
        Ok(ScVal::Symbol(ScSymbol(strm)))
    }

    // Keys must be in lexicographic order: create < deposit < spend < withdraw.
    let entries = vec![
        ScMapEntry {
            key: sym("create")?,
            val: vec_of(create_ops)?,
        },
        ScMapEntry {
            key: sym("deposit")?,
            val: vec_of(deposit_ops)?,
        },
        ScMapEntry {
            key: sym("spend")?,
            val: vec_of(spend_ops)?,
        },
        ScMapEntry {
            key: sym("withdraw")?,
            val: vec_of(withdraw_ops)?,
        },
    ];
    let map: VecM<ScMapEntry> =
        VecM::try_from(entries).map_err(|e| MlxdrError::Xdr(format!("ScMap build: {e}")))?;
    Ok(ScVal::Map(Some(ScMap(map))))
}

pub fn classify(mlxdr_strings: &[&str]) -> Result<Classified, MlxdrError> {
    let mut out = Classified::default();
    for s in mlxdr_strings {
        let op = decode(s)?;
        match op.kind {
            OperationKind::Create => out.create.push(op),
            OperationKind::Spend => out.spend.push(op),
            OperationKind::Deposit => out.deposit.push(op),
            OperationKind::Withdraw => out.withdraw.push(op),
        }
    }
    Ok(out)
}

/// Sum amounts for each operation set (Spend amounts come from on-chain balances; pass them in).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Amounts {
    pub total_create: i128,
    pub total_spend: i128,
    pub total_deposit: i128,
    pub total_withdraw: i128,
}

impl Amounts {
    pub fn from_classified(c: &Classified, spend_balances_in_order: &[i128]) -> Self {
        Self {
            total_create: c.create.iter().map(|o| o.amount).sum(),
            total_spend: spend_balances_in_order.iter().sum(),
            total_deposit: c.deposit.iter().map(|o| o.amount).sum(),
            total_withdraw: c.withdraw.iter().map(|o| o.amount).sum(),
        }
    }
}

/// Bundle fee derivation — matches `provider-platform/src/core/service/bundle/bundle.service.ts:169`:
///
/// ```text
/// totalInflows  = totalDeposit
/// totalOutflows = totalCreate + totalWithdraw
/// if totalInflows <= 0 → fee = totalSpend - totalOutflows
/// else                 → fee = totalInflows - totalOutflows
/// ```
pub fn calculate_fee(a: Amounts) -> i128 {
    let total_inflows = a.total_deposit;
    let total_outflows = a.total_create + a.total_withdraw;
    if total_inflows <= 0 {
        a.total_spend - total_outflows
    } else {
        total_inflows - total_outflows
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_client::xdr::{Int128Parts, ScAddress, ScBytes, ScVal, ScVec, VecM, WriteXdr};

    fn i128_to_parts(v: i128) -> Int128Parts {
        Int128Parts {
            hi: (v >> 64) as i64,
            lo: ((v as u128) & 0xFFFF_FFFF_FFFF_FFFF) as u64,
        }
    }

    /// Encode an MLXDR string given the type byte + already-built operation ScVec.
    fn build_mlxdr(type_byte: u8, op_payload: ScVal) -> String {
        let signature = ScVal::Vec(Some(ScVec(VecM::try_from(Vec::<ScVal>::new()).unwrap())));
        let outer = ScVal::Vec(Some(ScVec(
            VecM::try_from(vec![op_payload, signature]).unwrap(),
        )));
        let xdr_bytes = outer.to_xdr(Limits::none()).unwrap();
        let mut buf = Vec::with_capacity(3 + xdr_bytes.len());
        buf.extend_from_slice(&ML_PREFIX);
        buf.push(type_byte);
        buf.extend_from_slice(&xdr_bytes);
        B64.encode(buf)
    }

    fn create_op(utxo: [u8; 65], amount: i128) -> String {
        let payload = ScVal::Vec(Some(ScVec(
            VecM::try_from(vec![
                ScVal::Bytes(ScBytes(utxo.to_vec().try_into().unwrap())),
                ScVal::I128(i128_to_parts(amount)),
            ])
            .unwrap(),
        )));
        build_mlxdr(0x04, payload)
    }

    fn spend_op(utxo: [u8; 65]) -> String {
        let payload = ScVal::Vec(Some(ScVec(
            VecM::try_from(vec![
                ScVal::Bytes(ScBytes(utxo.to_vec().try_into().unwrap())),
                ScVal::Vec(Some(ScVec(VecM::try_from(Vec::<ScVal>::new()).unwrap()))),
            ])
            .unwrap(),
        )));
        build_mlxdr(0x05, payload)
    }

    fn deposit_op(amount: i128) -> String {
        let payload = ScVal::Vec(Some(ScVec(
            VecM::try_from(vec![
                ScVal::Address(ScAddress::Account(soroban_client::xdr::AccountId(
                    soroban_client::xdr::PublicKey::PublicKeyTypeEd25519(
                        soroban_client::xdr::Uint256([0xABu8; 32]),
                    ),
                ))),
                ScVal::I128(i128_to_parts(amount)),
                ScVal::Vec(Some(ScVec(VecM::try_from(Vec::<ScVal>::new()).unwrap()))),
            ])
            .unwrap(),
        )));
        build_mlxdr(0x06, payload)
    }

    fn withdraw_op(amount: i128) -> String {
        let payload = ScVal::Vec(Some(ScVec(
            VecM::try_from(vec![
                ScVal::Address(ScAddress::Account(soroban_client::xdr::AccountId(
                    soroban_client::xdr::PublicKey::PublicKeyTypeEd25519(
                        soroban_client::xdr::Uint256([0xCDu8; 32]),
                    ),
                ))),
                ScVal::I128(i128_to_parts(amount)),
                ScVal::Vec(Some(ScVec(VecM::try_from(Vec::<ScVal>::new()).unwrap()))),
            ])
            .unwrap(),
        )));
        build_mlxdr(0x07, payload)
    }

    #[test]
    fn decode_create_returns_amount_and_utxo() {
        let s = create_op([0x11u8; 65], 12345);
        let op = decode(&s).expect("decode");
        assert_eq!(op.kind, OperationKind::Create);
        assert_eq!(op.amount, 12345);
        assert_eq!(op.utxo, vec![0x11u8; 65]);
    }

    #[test]
    fn decode_deposit_returns_amount_zero_utxo() {
        let s = deposit_op(7_000);
        let op = decode(&s).unwrap();
        assert_eq!(op.kind, OperationKind::Deposit);
        assert_eq!(op.amount, 7_000);
        assert!(op.utxo.is_empty());
    }

    #[test]
    fn decode_rejects_bad_prefix() {
        // base64 of [0xAA, 0xBB, 0x04] is invalid prefix.
        let bad = B64.encode([0xAAu8, 0xBB, 0x04]);
        let err = decode(&bad).unwrap_err();
        assert!(matches!(err, MlxdrError::MissingPrefix));
    }

    #[test]
    fn classify_partitions_by_kind() {
        let strs: Vec<String> = vec![
            create_op([0x11u8; 65], 100),
            create_op([0x22u8; 65], 200),
            spend_op([0x33u8; 65]),
            deposit_op(1000),
            withdraw_op(50),
        ];
        let refs: Vec<&str> = strs.iter().map(|s| s.as_str()).collect();
        let c = classify(&refs).expect("classify");
        assert_eq!(c.create.len(), 2);
        assert_eq!(c.spend.len(), 1);
        assert_eq!(c.deposit.len(), 1);
        assert_eq!(c.withdraw.len(), 1);
    }

    #[test]
    fn fee_deposit_minus_create_when_inflows_positive() {
        // 2 creates totaling 300, 1 deposit 1000 → fee = 1000 - 300 = 700.
        let strs: Vec<String> = vec![
            create_op([0x11u8; 65], 100),
            create_op([0x22u8; 65], 200),
            deposit_op(1000),
        ];
        let refs: Vec<&str> = strs.iter().map(|s| s.as_str()).collect();
        let c = classify(&refs).unwrap();
        let amounts = Amounts::from_classified(&c, &[]);
        assert_eq!(calculate_fee(amounts), 700);
    }

    #[test]
    fn fee_spend_minus_create_when_no_inflows() {
        // Spend balance = 500 (from chain), creates = 300, no deposit → fee = 500 - 300 = 200.
        let strs: Vec<String> = vec![
            create_op([0x11u8; 65], 100),
            create_op([0x22u8; 65], 200),
            spend_op([0x33u8; 65]),
        ];
        let refs: Vec<&str> = strs.iter().map(|s| s.as_str()).collect();
        let c = classify(&refs).unwrap();
        let amounts = Amounts::from_classified(&c, &[500]);
        assert_eq!(calculate_fee(amounts), 200);
    }

    #[test]
    fn fee_minus_withdraw_when_pure_withdrawal() {
        // Spend balance = 1000, withdraw 800, no create, no deposit → fee = 1000 - 800 = 200.
        let strs: Vec<String> = vec![spend_op([0x33u8; 65]), withdraw_op(800)];
        let refs: Vec<&str> = strs.iter().map(|s| s.as_str()).collect();
        let c = classify(&refs).unwrap();
        let amounts = Amounts::from_classified(&c, &[1000]);
        assert_eq!(calculate_fee(amounts), 200);
    }
}
