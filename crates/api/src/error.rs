use actix_web::{http::StatusCode, HttpResponse, ResponseError};
use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("bad request: {0}")]
    BadRequest(String),

    #[error("unauthorized")]
    Unauthorized,

    #[error("forbidden")]
    Forbidden,

    #[error("not found")]
    NotFound,

    #[error("not implemented")]
    NotImplemented,

    #[error("internal: {0}")]
    Internal(String),
}

#[derive(Serialize)]
struct ErrorBody {
    error: String,
    message: String,
}

impl ResponseError for ApiError {
    fn status_code(&self) -> StatusCode {
        match self {
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::NotImplemented => StatusCode::NOT_IMPLEMENTED,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn error_response(&self) -> HttpResponse {
        let kind = match self {
            Self::BadRequest(_) => "bad_request",
            Self::Unauthorized => "unauthorized",
            Self::Forbidden => "forbidden",
            Self::NotFound => "not_found",
            Self::NotImplemented => "not_implemented",
            Self::Internal(_) => "internal",
        };
        HttpResponse::build(self.status_code()).json(ErrorBody {
            error: kind.into(),
            message: self.to_string(),
        })
    }
}

impl From<provider_stack_core::error::CoreError> for ApiError {
    fn from(e: provider_stack_core::error::CoreError) -> Self {
        use provider_stack_core::error::CoreError;
        match e {
            CoreError::InvalidSignature | CoreError::InvalidChallenge => Self::Unauthorized,
            CoreError::OperatorNotAuthorized => Self::Forbidden,
            CoreError::Db(e) => Self::Internal(e.to_string()),
            other => Self::Internal(other.to_string()),
        }
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        Self::Internal(e.to_string())
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        Self::Internal(e.to_string())
    }
}
