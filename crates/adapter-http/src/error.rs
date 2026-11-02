//! Маппинг доменных ошибок в HTTP (таблица 5.3 спеки).

use std::error::Error as StdError;

use aide::OperationOutput;
use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};
use domain::error::Error;

use crate::problem::Problem;

#[derive(Debug)]
pub struct ApiError(pub Error);

impl From<Error> for ApiError {
    fn from(err: Error) -> Self {
        Self(err)
    }
}

// Ошибки в OpenAPI объявляются явно в `routes` (точный набор кодов на операцию),
// поэтому сам тип ничего не выводит.
impl OperationOutput for ApiError {
    type Inner = Self;
}

/// Текст ошибки вместе с цепочкой источников.
fn chain(err: &(dyn StdError + 'static)) -> String {
    let mut out = err.to_string();
    let mut cur = err.source();
    while let Some(e) = cur {
        out.push_str(": ");
        out.push_str(&e.to_string());
        cur = e.source();
    }
    out
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        // Без ветки `_`: новый вариант доменной ошибки без маппинга не компилируется.
        let problem = match &self.0 {
            Error::UserNotFound | Error::SourceAccountNotFound | Error::DestAccountNotFound => {
                Problem::new(StatusCode::NOT_FOUND, self.0.to_string())
            }
            Error::InvalidUserName
            | Error::InvalidPagination
            | Error::NonPositiveAmount
            | Error::SameAccount => Problem::new(StatusCode::BAD_REQUEST, self.0.to_string()),
            Error::InsufficientFunds => Problem::new(StatusCode::CONFLICT, self.0.to_string()),
            Error::Unavailable { context, source } => {
                tracing::warn!(context, error = %chain(source.as_ref()), "service unavailable");
                Problem::unavailable()
            }
            Error::Internal { context, source } => {
                tracing::error!(context, error = %chain(source.as_ref()), "internal error");
                Problem::internal()
            }
        };
        problem.into_response()
    }
}
