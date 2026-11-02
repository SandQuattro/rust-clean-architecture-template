use std::future::ready;

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use domain::{
    command::{CreateUser, DeleteUser, FindAllUsers, FindUserById, TransferMoney, UpdateUser},
    entity::{Order, Transfer, User, UserOrders},
    error::Error,
};
use serde_json::{Value, json};
use tower::ServiceExt;

use super::locations;
use crate::{routes::build, usecase::MockUserUseCase};

fn app(mock: MockUserUseCase) -> Router {
    build(mock).0
}

async fn call(app: Router, req: Request<Body>) -> (StatusCode, Value) {
    let resp = app.oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, body)
}

fn json_req(method: &str, uri: &str, body: String) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .unwrap()
}

fn get(uri: &str) -> Request<Body> {
    Request::get(uri).body(Body::empty()).unwrap()
}

fn user(id: i64, name: &str) -> User {
    User {
        id,
        name: name.to_owned(),
    }
}

fn transfer_body(amount: i64) -> String {
    json!({"from_account_id": 1, "to_account_id": 2, "amount": amount}).to_string()
}

#[tokio::test]
async fn list_users_returns_page() {
    let mut mock = MockUserUseCase::new();
    mock.expect_find_all_users()
        .withf(|cmd| *cmd == FindAllUsers { page: 1, size: 10 })
        .times(1)
        .returning(|_| Box::pin(ready(Ok(vec![user(1, "a"), user(2, "b")]))));
    let (status, body) = call(app(mock), get("/users/1/10")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({"users": [{"id": 1, "name": "a"}, {"id": 2, "name": "b"}]})
    );
}

#[tokio::test]
async fn list_users_rejects_page_zero() {
    let (status, body) = call(app(MockUserUseCase::new()), get("/users/0/10")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(locations(&body).contains(&"path.page"), "{body}");
}

#[tokio::test]
async fn list_users_rejects_size_over_1000() {
    let (status, body) = call(app(MockUserUseCase::new()), get("/users/1/1001")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(locations(&body).contains(&"path.size"), "{body}");
}

#[tokio::test]
async fn list_users_with_orders_serializes_empty_orders_as_array() {
    let mut mock = MockUserUseCase::new();
    mock.expect_find_all_users_with_orders()
        .times(1)
        .returning(|_| {
            Box::pin(ready(Ok(vec![
                UserOrders {
                    id: 1,
                    name: "a".into(),
                    orders: vec![],
                },
                UserOrders {
                    id: 2,
                    name: "b".into(),
                    orders: vec![Order {
                        id: 9,
                        user_id: 2,
                        amount: 100,
                    }],
                },
            ])))
        });
    let (status, body) = call(app(mock), get("/users-with-orders/1/10")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["users"][0]["orders"], json!([]));
    assert_eq!(
        body["users"][1]["orders"],
        json!([{"id": 9, "amount": 100}])
    );
}

#[tokio::test]
async fn list_users_with_orders_masks_internal_error() {
    let mut mock = MockUserUseCase::new();
    mock.expect_find_all_users_with_orders()
        .returning(|_| Box::pin(ready(Err(Error::internal("query users", "boom")))));
    let (status, body) = call(app(mock), get("/users-with-orders/1/10")).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body["detail"], "internal server error");
}

#[tokio::test]
async fn get_user_not_found() {
    let mut mock = MockUserUseCase::new();
    mock.expect_find_user_by_id()
        .withf(|cmd| *cmd == FindUserById { id: 5 })
        .returning(|_| Box::pin(ready(Err(Error::UserNotFound))));
    let (status, body) = call(app(mock), get("/user/5")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["detail"], "user not found");
}

#[tokio::test]
async fn create_user_returns_201() {
    let mut mock = MockUserUseCase::new();
    mock.expect_create_user()
        .withf(|cmd| {
            *cmd == CreateUser {
                name: "Mike".into(),
            }
        })
        .returning(|_| Box::pin(ready(Ok(user(1, "Mike")))));
    let (status, body) = call(
        app(mock),
        json_req("POST", "/user", r#"{"name":"Mike"}"#.into()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body, json!({"id": 1, "name": "Mike"}));
}

#[tokio::test]
async fn create_user_rejects_empty_name() {
    let (status, body) = call(
        app(MockUserUseCase::new()),
        json_req("POST", "/user", r#"{"name":""}"#.into()),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(locations(&body).contains(&"body.name"), "{body}");
}

#[tokio::test]
async fn create_user_accepts_255_cyrillic_chars() {
    let name = "я".repeat(255);
    let expected = name.clone();
    let mut mock = MockUserUseCase::new();
    mock.expect_create_user()
        .withf(move |cmd| cmd.name == expected)
        .times(1)
        .returning(|cmd| Box::pin(ready(Ok(user(1, &cmd.name)))));
    let body = json!({ "name": name }).to_string();
    let (status, _) = call(app(mock), json_req("POST", "/user", body)).await;
    assert_eq!(status, StatusCode::CREATED);
}

#[tokio::test]
async fn create_user_rejects_256_chars() {
    let body = json!({ "name": "я".repeat(256) }).to_string();
    let (status, body) = call(app(MockUserUseCase::new()), json_req("POST", "/user", body)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(locations(&body).contains(&"body.name"), "{body}");
}

#[tokio::test]
async fn create_user_rejects_oversized_body() {
    let body = format!(r#"{{"name":"{}"}}"#, "x".repeat(3 * 1024 * 1024));
    let resp = app(MockUserUseCase::new())
        .oneshot(json_req("POST", "/user", body))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn create_user_rejects_empty_body() {
    let resp = app(MockUserUseCase::new())
        .oneshot(json_req("POST", "/user", String::new()))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        resp.headers()[header::CONTENT_TYPE],
        "application/problem+json"
    );
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["status"], 400, "{body}");
    assert_eq!(body["detail"], "invalid JSON body", "{body}");
}

#[tokio::test]
async fn update_user_uses_path_id() {
    let mut mock = MockUserUseCase::new();
    mock.expect_update_user()
        .withf(|cmd| {
            *cmd == UpdateUser {
                id: 7,
                name: "x".into(),
            }
        })
        .times(1)
        .returning(|cmd| Box::pin(ready(Ok(user(cmd.id, &cmd.name)))));
    let (status, body) = call(
        app(mock),
        json_req("PUT", "/user/7", r#"{"name":"x"}"#.into()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"id": 7, "name": "x"}));
}

#[tokio::test]
async fn update_user_rejects_body_id() {
    let (status, body) = call(
        app(MockUserUseCase::new()),
        json_req("PUT", "/user/7", r#"{"id":5,"name":"x"}"#.into()),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(locations(&body).contains(&"body.id"), "{body}");
}

#[tokio::test]
async fn update_user_not_found() {
    let mut mock = MockUserUseCase::new();
    mock.expect_update_user()
        .returning(|_| Box::pin(ready(Err(Error::UserNotFound))));
    let (status, _) = call(
        app(mock),
        json_req("PUT", "/user/7", r#"{"name":"x"}"#.into()),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_user_returns_204() {
    let mut mock = MockUserUseCase::new();
    mock.expect_delete_user()
        .withf(|cmd| *cmd == DeleteUser { id: 3 })
        .times(1)
        .returning(|_| Box::pin(ready(Ok(()))));
    let req = Request::delete("/user/3").body(Body::empty()).unwrap();
    let resp = app(mock).oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    assert!(bytes.is_empty());
}

#[tokio::test]
async fn delete_user_not_found() {
    let mut mock = MockUserUseCase::new();
    mock.expect_delete_user()
        .returning(|_| Box::pin(ready(Err(Error::UserNotFound))));
    let req = Request::delete("/user/3").body(Body::empty()).unwrap();
    let (status, _) = call(app(mock), req).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn transfer_returns_204() {
    let mut mock = MockUserUseCase::new();
    mock.expect_transfer_money()
        .withf(|cmd| {
            *cmd == TransferMoney {
                transfer: Transfer {
                    from_account_id: 1,
                    to_account_id: 2,
                    amount: 100,
                },
            }
        })
        .times(1)
        .returning(|_| Box::pin(ready(Ok(()))));
    let resp = app(mock)
        .oneshot(json_req("POST", "/transfer", transfer_body(100)))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}

async fn transfer_error(err: Error) -> StatusCode {
    let mut mock = MockUserUseCase::new();
    mock.expect_transfer_money()
        .return_once(|_| Box::pin(ready(Err(err))));
    call(app(mock), json_req("POST", "/transfer", transfer_body(100)))
        .await
        .0
}

#[tokio::test]
async fn transfer_same_account() {
    assert_eq!(
        transfer_error(Error::SameAccount).await,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn transfer_destination_not_found() {
    assert_eq!(
        transfer_error(Error::DestAccountNotFound).await,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn transfer_insufficient_funds() {
    assert_eq!(
        transfer_error(Error::InsufficientFunds).await,
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn transfer_rejects_zero_amount() {
    let (status, body) = call(
        app(MockUserUseCase::new()),
        json_req("POST", "/transfer", transfer_body(0)),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(locations(&body).contains(&"body.amount"), "{body}");
}

#[tokio::test]
async fn unavailable_maps_to_503() {
    let mut mock = MockUserUseCase::new();
    mock.expect_find_user_by_id().returning(|_| {
        Box::pin(ready(Err(Error::unavailable(
            "acquire connection",
            "pool timeout",
        ))))
    });
    let (status, body) = call(app(mock), get("/user/1")).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["detail"], "service unavailable");
}
