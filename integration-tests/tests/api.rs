//! Интеграционные тесты против запущенного приложения (`HOST`/`PORT`).
//! `SKIP_INTEGRATION_TESTS` с непустым значением пропускает все тесты.

// Хелперы вне #[test]-функций падают по expect/panic осознанно: это сбой окружения теста.
#![allow(clippy::expect_used, clippy::panic)]

use std::time::Duration;

use reqwest::{Client, Method, Response, StatusCode};
use serde_json::{Value, json};

const READY_ATTEMPTS: u32 = 20;

/// `None` — тесты пропущены. Без `HOST`/`PORT` тест падает с понятным сообщением.
fn base_url() -> Option<String> {
    if std::env::var("SKIP_INTEGRATION_TESTS").is_ok_and(|v| !v.is_empty()) {
        return None;
    }
    let host = std::env::var("HOST").unwrap_or_default();
    let port = std::env::var("PORT").unwrap_or_default();
    assert!(
        !host.is_empty() && !port.is_empty(),
        "integration tests need env HOST and PORT of the running app (or SKIP_INTEGRATION_TESTS=1)"
    );
    Some(format!("http://{host}:{port}"))
}

fn client() -> Client {
    Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("build http client")
}

/// До 20 попыток `GET /readyz` с паузой 1 s.
async fn wait_ready(client: &Client, base: &str) {
    let url = format!("{base}/readyz");
    let mut last = String::new();
    for attempt in 1..=READY_ATTEMPTS {
        match client.get(&url).send().await {
            Ok(resp) if resp.status() == StatusCode::OK => return,
            Ok(resp) => last = format!("status {}", resp.status()),
            Err(err) => last = err.to_string(),
        }
        eprintln!("{url} is not ready (attempt {attempt}/{READY_ATTEMPTS}): {last}");
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    panic!("{url} is not available after {READY_ATTEMPTS} attempts: {last}");
}

/// Возвращает клиента и базовый URL после ожидания готовности; `None` — пропуск.
async fn setup() -> Option<(Client, String)> {
    let base = base_url()?;
    let client = client();
    wait_ready(&client, &base).await;
    Some((client, base))
}

async fn send(client: &Client, method: Method, url: &str, body: Option<Value>) -> Response {
    let mut req = client.request(method.clone(), url);
    if let Some(body) = body {
        req = req.json(&body);
    }
    req.send()
        .await
        .unwrap_or_else(|err| panic!("{method} {url}: {err}"))
}

/// Создаёт пользователя и возвращает его id.
async fn create_user(client: &Client, base: &str, name: &str) -> i64 {
    let resp = send(
        client,
        Method::POST,
        &format!("{base}/user"),
        Some(json!({ "name": name })),
    )
    .await;
    let status = resp.status();
    let body: Value = resp.json().await.expect("decode create response");
    assert_eq!(status, StatusCode::CREATED, "create user: {body}");
    assert_eq!(body["name"], name, "create user: {body}");
    body["id"]
        .as_i64()
        .filter(|id| *id != 0)
        .expect("create user: id")
}

async fn status_of(client: &Client, method: Method, url: &str, body: Option<Value>) -> StatusCode {
    send(client, method, url, body).await.status()
}

#[tokio::test]
async fn readyz_ok() {
    let Some((client, base)) = setup().await else {
        return;
    };
    assert_eq!(
        status_of(&client, Method::GET, &format!("{base}/readyz"), None).await,
        StatusCode::OK
    );
}

#[tokio::test]
async fn livez_ok() {
    let Some((client, base)) = setup().await else {
        return;
    };
    assert_eq!(
        status_of(&client, Method::GET, &format!("{base}/livez"), None).await,
        StatusCode::OK
    );
}

#[tokio::test]
async fn user_crud_flow() {
    let Some((client, base)) = setup().await else {
        return;
    };
    let id = create_user(&client, &base, "integration-user").await;
    let user_url = format!("{base}/user/{id}");

    let status = status_of(&client, Method::GET, &user_url, None).await;
    assert_eq!(status, StatusCode::OK, "get user");

    let resp = send(
        &client,
        Method::PUT,
        &user_url,
        Some(json!({ "name": "integration-user-renamed" })),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK, "update user");
    let updated: Value = resp.json().await.expect("decode update response");
    assert_eq!(updated["id"], id, "update user: {updated}");
    assert_eq!(
        updated["name"], "integration-user-renamed",
        "update user: {updated}"
    );

    let resp = send(&client, Method::GET, &format!("{base}/users/1/100"), None).await;
    assert_eq!(resp.status(), StatusCode::OK, "list users");
    let list: Value = resp.json().await.expect("decode list response");
    let users = list["users"].as_array().expect("users must be an array");
    assert!(!users.is_empty(), "list users: expected at least one user");

    let status = status_of(&client, Method::DELETE, &user_url, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "delete user");

    let status = status_of(&client, Method::GET, &user_url, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "get deleted user");
}

#[tokio::test]
async fn user_not_found() {
    let Some((client, base)) = setup().await else {
        return;
    };
    let status = status_of(&client, Method::GET, &format!("{base}/user/999999"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn create_user_empty_name() {
    let Some((client, base)) = setup().await else {
        return;
    };
    let status = status_of(
        &client,
        Method::POST,
        &format!("{base}/user"),
        Some(json!({ "name": "" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn list_users_invalid_page() {
    let Some((client, base)) = setup().await else {
        return;
    };
    let status = status_of(&client, Method::GET, &format!("{base}/users/0/10"), None).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

/// Заказы через публичное API не создаются, поэтому проверяется форма ответа:
/// у каждого пользователя `orders` — массив (пустой, а не null).
#[tokio::test]
async fn users_with_orders_orders_is_array() {
    let Some((client, base)) = setup().await else {
        return;
    };
    create_user(&client, &base, "integration-user-with-orders").await;

    let resp = send(
        &client,
        Method::GET,
        &format!("{base}/users-with-orders/1/100"),
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK, "list users with orders");
    let list: Value = resp.json().await.expect("decode list response");
    let users = list["users"].as_array().expect("users must be an array");
    assert!(!users.is_empty(), "expected at least one user");
    for user in users {
        assert!(
            user["orders"].is_array(),
            "user {}: orders must be an array, got {}",
            user["id"],
            user["orders"]
        );
    }
}

async fn transfer(client: &Client, base: &str, from: i64, to: i64) -> StatusCode {
    let body = json!({ "from_account_id": from, "to_account_id": to, "amount": 100 });
    status_of(
        client,
        Method::POST,
        &format!("{base}/transfer"),
        Some(body),
    )
    .await
}

/// Новые пользователи создаются с нулевым балансом, поэтому позитивный сценарий
/// перевода недостижим через публичное API — проверяются доменные ошибки.
#[tokio::test]
async fn transfer_insufficient_funds() {
    let Some((client, base)) = setup().await else {
        return;
    };
    let from = create_user(&client, &base, "transfer-source").await;
    let to = create_user(&client, &base, "transfer-destination").await;
    assert_eq!(
        transfer(&client, &base, from, to).await,
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn transfer_same_account() {
    let Some((client, base)) = setup().await else {
        return;
    };
    let user = create_user(&client, &base, "transfer-same-account").await;
    assert_eq!(
        transfer(&client, &base, user, user).await,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn transfer_source_not_found() {
    let Some((client, base)) = setup().await else {
        return;
    };
    let to = create_user(&client, &base, "transfer-orphan-destination").await;
    assert_eq!(
        transfer(&client, &base, 999_999_999, to).await,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn errors_are_problem_json() {
    let Some((client, base)) = setup().await else {
        return;
    };
    let resp = send(&client, Method::GET, &format!("{base}/user/999999"), None).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    assert!(
        content_type.starts_with("application/problem+json"),
        "content-type: {content_type}"
    );
    let body: Value = resp.json().await.expect("decode problem body");
    assert!(body["title"].is_string(), "title: {body}");
    assert_eq!(body["status"], 404, "{body}");
    assert_eq!(body["detail"], "user not found", "{body}");
}

#[tokio::test]
async fn validation_error_has_location() {
    let Some((client, base)) = setup().await else {
        return;
    };
    let resp = send(
        &client,
        Method::POST,
        &format!("{base}/user"),
        Some(json!({ "name": "" })),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body: Value = resp.json().await.expect("decode problem body");
    assert_eq!(body["errors"][0]["location"], "body.name", "{body}");
}
