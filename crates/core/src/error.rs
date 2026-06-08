use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("invalid signature")]
    InvalidSignature,

    #[error("nonce expired or unknown")]
    InvalidChallenge,

    #[error("operator not authorized: sub does not match OPERATOR_PUBLIC_KEY")]
    OperatorNotAuthorized,

    #[error("xdr parse failure: {0}")]
    XdrParse(String),

    #[error("stellar strkey: {0}")]
    Strkey(String),

    #[error("jwt: {0}")]
    Jwt(String),

    #[error("config: {0}")]
    Config(String),

    #[error(transparent)]
    Db(#[from] sqlx::Error),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
