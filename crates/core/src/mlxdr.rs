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
use soroban_client::xdr::{Limits, ReadXdr, ScVal};
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
    let bytes = B64.decode(mlxdr_b64).map_err(|e| MlxdrError::Base64(e.to_string()))?;
    if bytes.len() < 3 {
        return Err(MlxdrError::TooShort);
    }
    if bytes[0..2] != ML_PREFIX {
        return Err(MlxdrError::MissingPrefix);
    }
    let kind = OperationKind::from_type_byte(bytes[2])?;

    // Remainder is stellar-xdr of ScVal::Vec([operation_scval, signature_scval]).
    let outer = ScVal::from_xdr(&bytes[3..], Limits::none())
        .map_err(|e| MlxdrError::Xdr(e.to_string()))?;
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
            let utxo = scbytes(&payload[0])
                .ok_or(MlxdrError::BadShape("Create.utxo must be ScBytes"))?;
            let amount = sci128(&payload[1])
                .ok_or(MlxdrError::BadShape("Create.amount must be ScI128"))?;
            (utxo, amount)
        }
        OperationKind::Spend => {
            if payload.len() < 1 {
                return Err(MlxdrError::BadShape("Spend payload must have ≥1 field"));
            }
            let utxo = scbytes(&payload[0])
                .ok_or(MlxdrError::BadShape("Spend.utxo must be ScBytes"))?;
            (utxo, 0i128)
        }
        OperationKind::Deposit | OperationKind::Withdraw => {
            // [address, amount, conditions]
            if payload.len() < 2 {
                return Err(MlxdrError::BadShape(
                    "Deposit/Withdraw payload must have ≥2 fields",
                ));
            }
            let amount = sci128(&payload[1])
                .ok_or(MlxdrError::BadShape("amount must be ScI128"))?;
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
    use soroban_client::xdr::{
        Int128Parts, ScAddress, ScBytes, ScVal, ScVec, VecM, WriteXdr,
    };

    fn i128_to_parts(v: i128) -> Int128Parts {
        Int128Parts {
            hi: ((v as i128) >> 64) as i64,
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
