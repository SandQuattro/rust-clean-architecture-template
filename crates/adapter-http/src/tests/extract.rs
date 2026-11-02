use axum::{
    Router,
    body::{Body, to_bytes},
    extract::FromRequestParts,
    http::{Request, StatusCode, header},
    routing::{get, post},
};
use garde::Validate;
use serde::Deserialize;
use serde_json::Value;
use tower::ServiceExt;

use super::locations;
use crate::extract::{ValidJson, ValidPath};

#[derive(Debug, Deserialize, Validate)]
#[serde(deny_unknown_fields)]
struct Probe {
    #[garde(length(chars, min = 1, max = 3))]
    name: String,
}

#[derive(Debug, Deserialize, Validate)]
struct PathProbe {
    #[garde(range(min = 1))]
    id: i64,
}

fn router() -> Router {
    Router::new()
        .route(
            "/",
            post(|ValidJson(_): ValidJson<Probe>| async { StatusCode::OK }),
        )
        .route(
            "/{id}",
            get(|ValidPath(_): ValidPath<PathProbe>| async { StatusCode::OK }),
        )
}

async fn post_body(
    body: &'static str,
    content_type: Option<&str>,
) -> (StatusCode, Option<String>, Value) {
    let mut req = Request::post("/");
    if let Some(ct) = content_type {
        req = req.header(header::CONTENT_TYPE, ct);
    }
    send(req.body(Body::from(body)).unwrap()).await
}

async fn send(req: Request<Body>) -> (StatusCode, Option<String>, Value) {
    let resp = router().oneshot(req).await.unwrap();
    let status = resp.status();
    let ct = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .map(|v| v.to_str().unwrap().to_owned());
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, ct, json)
}

async fn get_path(path: &str) -> (StatusCode, Option<String>, Value) {
    send(Request::get(path).body(Body::empty()).unwrap()).await
}

const JSON: Option<&str> = Some("application/json");

#[tokio::test]
async fn valid_json_accepts_valid_body() {
    let (status, _, _) = post_body(r#"{"name":"ab"}"#, JSON).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn valid_json_reports_garde_violation() {
    let (status, ct, body) = post_body(r#"{"name":""}"#, JSON).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(ct.as_deref(), Some("application/problem+json"));
    assert_eq!(body["detail"], "validation failed");
    assert_eq!(body["errors"][0]["location"], "body.name");
}

#[tokio::test]
async fn valid_json_counts_chars_not_bytes() {
    let (status, _, _) = post_body(r#"{"name":"яяя"}"#, JSON).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = post_body(r#"{"name":"яяяя"}"#, JSON).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn valid_json_rejects_unknown_field() {
    let (status, _, body) = post_body(r#"{"name":"a","id":5}"#, JSON).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(locations(&body).contains(&"body.id"), "{body}");
}

#[tokio::test]
async fn valid_json_rejects_wrong_type() {
    let (status, _, body) = post_body(r#"{"name":5}"#, JSON).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(locations(&body).contains(&"body.name"), "{body}");
}

#[tokio::test]
async fn valid_json_rejects_malformed_json() {
    let (status, ct, _) = post_body(r#"{"name":"#, JSON).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(ct.as_deref(), Some("application/problem+json"));
}

#[tokio::test]
async fn valid_json_accepts_missing_content_type() {
    let (status, _, _) = post_body(r#"{"name":"ab"}"#, None).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn valid_path_reports_range_violation() {
    let (status, ct, body) = get_path("/0").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(ct.as_deref(), Some("application/problem+json"));
    assert!(locations(&body).contains(&"path.id"), "{body}");
}

#[tokio::test]
async fn valid_path_reports_overflow() {
    for path in ["/99999999999999999999", "/abc"] {
        let (status, _, body) = get_path(path).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{path}");
        assert!(locations(&body).contains(&"path.id"), "{path}: {body}");
    }
}

#[tokio::test]
async fn valid_json_rejects_trailing_data() {
    let (status, _, body) = post_body(r#"{"name":"ab"} junk"#, JSON).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

#[tokio::test]
async fn valid_path_masks_internal_rejection() {
    // Запрос без внутренних параметров маршрутизатора: axum отвечает MissingPathParams (500),
    // текст axum наружу не уходит.
    let (mut parts, ()) = Request::new(()).into_parts();
    let rej = match ValidPath::<PathProbe>::from_request_parts(&mut parts, &()).await {
        Ok(_) => panic!("expected rejection"),
        Err(rej) => rej,
    };
    assert_eq!(rej.status, 500);
    assert_eq!(rej.detail, "internal server error");
}
