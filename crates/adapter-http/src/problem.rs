//! RFC 9457 `application/problem+json`; структура повторяет `ErrorModel` из Huma v2.

use aide::{
    OperationOutput,
    generate::GenContext,
    openapi::{MediaType, Operation, Response as ApiResponse, SchemaObject},
};
use axum::{
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use schemars::JsonSchema;
use serde::Serialize;

/// Общий текст для 500: внутренние детали клиенту не уходят.
pub const INTERNAL_DETAIL: &str = "internal server error";
/// Общий текст для 503.
pub const UNAVAILABLE_DETAIL: &str = "service unavailable";

#[derive(Debug, Serialize, JsonSchema)]
pub struct Problem {
    pub title: String,
    pub status: u16,
    pub detail: String,
    // `default` нужен схеме: поле необязательно, раз при пустом списке не сериализуется.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<ErrorDetail>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ErrorDetail {
    pub message: String,
    /// Место ошибки в стиле Huma: `body.<поле>` или `path.<параметр>`.
    pub location: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
}

impl Problem {
    pub fn new(status: StatusCode, detail: impl Into<String>) -> Self {
        Self {
            title: status.canonical_reason().unwrap_or_default().to_owned(),
            status: status.as_u16(),
            detail: detail.into(),
            errors: Vec::new(),
        }
    }

    /// 500 с общим текстом: внутренние детали клиенту не уходят.
    pub fn internal() -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, INTERNAL_DETAIL)
    }

    /// 503 с общим текстом.
    pub fn unavailable() -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, UNAVAILABLE_DETAIL)
    }

    /// 422 со списком нарушений.
    pub fn validation(errors: Vec<ErrorDetail>) -> Self {
        Self {
            errors,
            ..Self::new(StatusCode::UNPROCESSABLE_ENTITY, "validation failed")
        }
    }
}

impl IntoResponse for Problem {
    fn into_response(self) -> Response {
        let status = StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        // Сериализация структуры из строк и чисел не падает; пустое тело — безопасный запасной вариант.
        let body = serde_json::to_vec(&self).unwrap_or_default();
        let mut resp = (status, body).into_response();
        resp.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/problem+json"),
        );
        resp
    }
}

// Описание ответа для OpenAPI: схема `Problem` с типом содержимого `application/problem+json`.
impl OperationOutput for Problem {
    type Inner = Self;

    fn operation_response(ctx: &mut GenContext, _operation: &mut Operation) -> Option<ApiResponse> {
        let media = MediaType {
            schema: Some(SchemaObject {
                json_schema: ctx.schema.subschema_for::<Self>(),
                example: None,
                external_docs: None,
            }),
            ..Default::default()
        };
        Some(ApiResponse {
            content: [("application/problem+json".to_owned(), media)]
                .into_iter()
                .collect(),
            ..Default::default()
        })
    }
}
