//! The error type shared by all RustCloud crates.
//!
//! An [`Error`] carries three things:
//! - a [`ErrorKind`] that decides the HTTP status;
//! - a stable machine-readable `code` (`"FILE_NOT_FOUND"`) for clients;
//! - a human-readable `message`.
//!
//! In HTTP responses it is rendered as
//! `{"error": {"code": "FILE_NOT_FOUND", "message": "File not found"}}`.
//! Internal errors never leak their details to the client: the real cause is
//! logged, and the client only sees `INTERNAL`.

use std::fmt;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use sea_orm::{DbErr, SqlErr};
use serde_json::json;

/// Shorthand for results that fail with [`Error`].
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Error category; decides the HTTP status code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// The request is malformed or fails validation (400).
    BadRequest,
    /// No valid session (401).
    Unauthorized,
    /// Authenticated, but not allowed to do this (403).
    Forbidden,
    /// The thing does not exist, or the caller may not know it exists (404).
    NotFound,
    /// The request conflicts with the current state, e.g. a duplicate name (409).
    Conflict,
    /// What the request refers to is gone for good, e.g. an expired cursor (410).
    Gone,
    /// The upload does not fit into the quota or size limit (413).
    PayloadTooLarge,
    /// The requested byte range is outside of the file (416).
    RangeNotSatisfiable,
    /// Too many attempts, try again later (429).
    TooManyRequests,
    /// A bug or an infrastructure failure (500).
    Internal,
    /// A service the server passes requests to failed (502).
    BadGateway,
    /// A service the server passes requests to took too long (504).
    GatewayTimeout,
}

impl ErrorKind {
    /// HTTP status code for this kind of error.
    pub fn status(self) -> StatusCode {
        match self {
            Self::BadRequest => StatusCode::BAD_REQUEST,
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::Conflict => StatusCode::CONFLICT,
            Self::Gone => StatusCode::GONE,
            Self::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::RangeNotSatisfiable => StatusCode::RANGE_NOT_SATISFIABLE,
            Self::TooManyRequests => StatusCode::TOO_MANY_REQUESTS,
            Self::Internal => StatusCode::INTERNAL_SERVER_ERROR,
            Self::BadGateway => StatusCode::BAD_GATEWAY,
            Self::GatewayTimeout => StatusCode::GATEWAY_TIMEOUT,
        }
    }
}

/// An error with a kind, a stable code and a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub kind: ErrorKind,
    /// Stable code clients can match on, e.g. `"FILE_NOT_FOUND"`.
    pub code: &'static str,
    pub message: String,
}

impl Error {
    pub fn new(kind: ErrorKind, code: &'static str, message: impl Into<String>) -> Self {
        Self { kind, code, message: message.into() }
    }

    pub fn bad_request(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(ErrorKind::BadRequest, code, message)
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Unauthorized, "UNAUTHORIZED", message)
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Forbidden, "FORBIDDEN", message)
    }

    pub fn not_found(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(ErrorKind::NotFound, code, message)
    }

    pub fn conflict(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Conflict, code, message)
    }

    /// Replaces the code, keeping kind and message:
    /// `Error::forbidden("Registration is closed").with_code("REGISTRATION_CLOSED")`.
    pub fn with_code(mut self, code: &'static str) -> Self {
        self.code = code;
        self
    }

    /// An unexpected failure. The cause is logged here; the client only gets a
    /// generic message.
    pub fn internal(cause: impl fmt::Display) -> Self {
        tracing::error!("internal error: {cause}");
        Self::new(ErrorKind::Internal, "INTERNAL", "Internal server error")
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for Error {}

impl From<DbErr> for Error {
    /// Unique-constraint violations become `409 CONFLICT`; anything else is internal.
    fn from(err: DbErr) -> Self {
        if let Some(SqlErr::UniqueConstraintViolation(_)) = err.sql_err() {
            return Self::conflict("ALREADY_EXISTS", "An item with this name already exists");
        }
        Self::internal(err)
    }
}

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Self::internal(err)
    }
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let body = json!({ "error": { "code": self.code, "message": self.message } });
        (self.kind.status(), Json(body)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;

    #[test]
    fn kinds_map_to_http_statuses() {
        assert_eq!(ErrorKind::NotFound.status(), StatusCode::NOT_FOUND);
        assert_eq!(ErrorKind::Conflict.status(), StatusCode::CONFLICT);
        assert_eq!(ErrorKind::PayloadTooLarge.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(ErrorKind::Internal.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn renders_as_json_envelope() {
        let response = Error::not_found("FILE_NOT_FOUND", "File not found").into_response();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"]["code"], "FILE_NOT_FOUND");
        assert_eq!(json["error"]["message"], "File not found");
    }

    #[test]
    fn internal_errors_hide_the_cause() {
        let err = Error::internal("database password is hunter2");
        assert_eq!(err.code, "INTERNAL");
        assert!(!err.message.contains("hunter2"));
    }
}
