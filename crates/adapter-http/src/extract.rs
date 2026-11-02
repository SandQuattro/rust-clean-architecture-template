//! Экстракторы с валидацией: десериализация, затем `garde::Validate`.
//! Нарушения дают 422 с `errors[]` в стиле Huma, неразбираемый JSON — 400.

use aide::{OperationInput, generate::GenContext, openapi::Operation};
use axum::{
    body::Bytes,
    extract::{
        FromRequest, FromRequestParts, Path, Request, path::ErrorKind, rejection::PathRejection,
    },
    http::{StatusCode, request::Parts},
};
use garde::Validate;
use schemars::JsonSchema;
use serde::de::DeserializeOwned;

use crate::problem::{ErrorDetail, Problem};

pub struct ValidJson<T>(pub T);
pub struct ValidPath<T>(pub T);

fn location(prefix: &str, path: &str) -> String {
    if path.is_empty() || path == "." {
        prefix.to_owned()
    } else {
        format!("{prefix}.{path}")
    }
}

fn garde_problem(prefix: &str, report: &garde::Report) -> Problem {
    let errors = report
        .iter()
        .map(|(path, err)| ErrorDetail {
            message: err.message().to_string(),
            location: location(prefix, &path.to_string()),
            value: None,
        })
        .collect();
    Problem::validation(errors)
}

impl<S, T> FromRequest<S> for ValidJson<T>
where
    S: Send + Sync,
    T: DeserializeOwned + Validate<Context = ()>,
{
    type Rejection = Problem;

    async fn from_request(req: Request, state: &S) -> Result<Self, Problem> {
        // Чтение через `Bytes` подчиняется `DefaultBodyLimit`; статус отказа (413) сохраняется.
        let bytes = Bytes::from_request(req, state)
            .await
            .map_err(|r| Problem::new(r.status(), r.body_text()))?;

        let mut de = serde_json::Deserializer::from_slice(&bytes);
        let value: T = serde_path_to_error::deserialize(&mut de).map_err(|e| {
            let inner = e.inner();
            if inner.is_syntax() || inner.is_eof() {
                return Problem::new(StatusCode::BAD_REQUEST, "invalid JSON body");
            }
            // Для неизвестного поля serde_path_to_error кладёт имя самого поля в путь.
            let loc = location("body", &e.path().to_string());
            let message = inner.to_string();
            Problem::validation(vec![ErrorDetail {
                message,
                location: loc,
                value: None,
            }])
        })?;
        // Данные после значения (`{"name":"ab"} junk`) — неразбираемое тело, как в Huma.
        de.end()
            .map_err(|_| Problem::new(StatusCode::BAD_REQUEST, "invalid JSON body"))?;

        value.validate().map_err(|r| garde_problem("body", &r))?;
        Ok(Self(value))
    }
}

impl<S, T> FromRequestParts<S> for ValidPath<T>
where
    S: Send + Sync,
    T: DeserializeOwned + Validate<Context = ()> + Send,
{
    type Rejection = Problem;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Problem> {
        let Path(value) = Path::<T>::from_request_parts(parts, state)
            .await
            .map_err(path_problem)?;
        value.validate().map_err(|r| garde_problem("path", &r))?;
        Ok(Self(value))
    }
}

fn path_problem(rej: PathRejection) -> Problem {
    let PathRejection::FailedToDeserializePathParams(err) = &rej else {
        return rejection_problem(&rej);
    };
    let key = match err.kind() {
        ErrorKind::ParseErrorAtKey { key, .. }
        | ErrorKind::InvalidUtf8InPathParam { key }
        | ErrorKind::DeserializeError { key, .. } => Some(key.as_str()),
        _ => None,
    };
    match key {
        Some(key) => Problem::validation(vec![ErrorDetail {
            message: err.body_text(),
            location: format!("path.{key}"),
            value: None,
        }]),
        None => rejection_problem(&rej),
    }
}

/// Отказ без привязки к параметру. 500 (например, `MissingPathParams`) — ошибка сервера:
/// текст axum клиенту не отдаётся, он уходит в лог.
fn rejection_problem(rej: &PathRejection) -> Problem {
    if rej.status() == StatusCode::INTERNAL_SERVER_ERROR {
        tracing::error!(error = %rej.body_text(), "path extraction failed");
        return Problem::internal();
    }
    Problem::new(rej.status(), rej.body_text())
}

impl<T: JsonSchema> OperationInput for ValidJson<T> {
    fn operation_input(ctx: &mut GenContext, operation: &mut Operation) {
        <axum::Json<T> as OperationInput>::operation_input(ctx, operation);
    }
}

impl<T: JsonSchema> OperationInput for ValidPath<T> {
    fn operation_input(ctx: &mut GenContext, operation: &mut Operation) {
        <Path<T> as OperationInput>::operation_input(ctx, operation);
    }
}
