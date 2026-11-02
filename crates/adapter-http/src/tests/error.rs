use std::{
    io,
    sync::{Arc, Mutex},
};

use axum::{body::to_bytes, http::StatusCode, response::IntoResponse};
use domain::error::Error;
use serde_json::Value;
use tracing_subscriber::fmt::MakeWriter;

use crate::{
    error::ApiError,
    problem::{INTERNAL_DETAIL, UNAVAILABLE_DETAIL},
};

async fn render(err: Error) -> (StatusCode, Value) {
    let resp = ApiError(err).into_response();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn api_error_maps_all_variants() {
    let cases = vec![
        (Error::UserNotFound, StatusCode::NOT_FOUND, "user not found"),
        (
            Error::SourceAccountNotFound,
            StatusCode::NOT_FOUND,
            "source account not found",
        ),
        (
            Error::DestAccountNotFound,
            StatusCode::NOT_FOUND,
            "destination account not found",
        ),
        (
            Error::InvalidUserName,
            StatusCode::BAD_REQUEST,
            "user name must be a non-empty valid UTF-8 string",
        ),
        (
            Error::InvalidPagination,
            StatusCode::BAD_REQUEST,
            "page and size must be greater than zero",
        ),
        (
            Error::NonPositiveAmount,
            StatusCode::BAD_REQUEST,
            "transfer amount must be positive",
        ),
        (
            Error::SameAccount,
            StatusCode::BAD_REQUEST,
            "cannot transfer to the same account",
        ),
        (
            Error::InsufficientFunds,
            StatusCode::CONFLICT,
            "insufficient funds",
        ),
        (
            Error::internal("ctx", io::Error::other("x")),
            StatusCode::INTERNAL_SERVER_ERROR,
            INTERNAL_DETAIL,
        ),
        (
            Error::unavailable("ctx", io::Error::other("x")),
            StatusCode::SERVICE_UNAVAILABLE,
            UNAVAILABLE_DETAIL,
        ),
    ];
    for (err, want_status, want_detail) in cases {
        let label = format!("{err:?}");
        let (status, body) = render(err).await;
        assert_eq!(status, want_status, "{label}");
        assert_eq!(body["detail"], want_detail, "{label}");
        assert_eq!(body["status"], want_status.as_u16(), "{label}");
        assert_eq!(
            body["title"],
            want_status.canonical_reason().unwrap(),
            "{label}"
        );
    }
}

#[derive(Clone, Default)]
struct Buf(Arc<Mutex<Vec<u8>>>);

impl io::Write for Buf {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Buf {
    type Writer = Buf;

    fn make_writer(&'a self) -> Buf {
        self.clone()
    }
}

#[tokio::test]
async fn internal_error_is_logged_and_masked() {
    let buf = Buf::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(buf.clone())
        .with_ansi(false)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let (status, body) = render(Error::internal(
        "update user",
        io::Error::other("secret-db-detail"),
    ))
    .await;

    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(!body.to_string().contains("secret-db-detail"));
    let log = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    assert!(log.contains("secret-db-detail"), "{log}");
    assert!(log.contains("update user"), "{log}");
}
