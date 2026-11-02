//! Сборка роутера: маршруты, OpenAPI-контракт, `/openapi.json` и `/docs`.

use std::sync::Arc;

use aide::{
    axum::{
        ApiRouter,
        routing::{get_with, post_with},
    },
    openapi::{OpenApi, SecurityScheme},
    scalar::Scalar,
    transform::TransformOpenApi,
};
use axum::{
    Json, Router,
    body::Bytes,
    http::{StatusCode, header},
    routing::get,
};

use crate::{dto::*, handlers, problem::Problem, usecase::UserUseCase};

const TITLE: &str = "Clean Architecture Template";
const TAG: &str = "Users";

/// Объявляет ответы-ошибки: ровно перечисленные коды, схема `Problem`, `application/problem+json`.
macro_rules! problems {
    ($op:expr, $($code:literal),+ $(,)?) => {
        $op$(.response_with::<$code, Problem, _>(|r| r.description(reason($code))))+
    };
}

fn reason(code: u16) -> &'static str {
    StatusCode::from_u16(code)
        .ok()
        .and_then(|s| s.canonical_reason())
        .unwrap_or_default()
}

fn api_info(api: TransformOpenApi<'_>) -> TransformOpenApi<'_> {
    api.title(TITLE)
        .version("1.0.0")
        .security_scheme(
            "bearer",
            SecurityScheme::Http {
                scheme: "bearer".to_owned(),
                bearer_format: Some("JWT".to_owned()),
                description: None,
                extensions: Default::default(),
            },
        )
        .security_requirement("bearer")
}

/// Строит роутер (состояние — `Arc<U>`) и OpenAPI-документ.
/// Документ сгенерирован из типов и обработчиков; схемы вручную не пишутся.
pub fn build<U: UserUseCase>(usecase: U) -> (Router, OpenApi) {
    let mut api = OpenApi::default();
    let router =
        ApiRouter::<Arc<U>>::new()
            .route(
                "/docs",
                Scalar::new("/openapi.json").with_title(TITLE).axum_route(),
            )
            .api_route(
                "/users/{page}/{size}",
                get_with(handlers::list_users::<U>, |op| {
                    problems!(
                        op.id("list-users")
                            .summary("list all users")
                            .description("Get a page of users. Pages are 1-based.")
                            .tag(TAG),
                        422,
                        500,
                        503
                    )
                }),
            )
            .api_route(
                "/users-with-orders/{page}/{size}",
                get_with(handlers::list_users_with_orders::<U>, |op| {
                    problems!(
                        op.id("list-users-with-orders")
                            .summary("list users with orders")
                            .description(
                                "Get a page of users with their orders. Pages are 1-based."
                            )
                            .tag(TAG),
                        422,
                        500,
                        503
                    )
                }),
            )
            .api_route(
                "/user/{id}",
                get_with(handlers::get_user_by_id::<U>, |op| {
                    problems!(
                        op.id("get-user-by-id")
                            .summary("user by id")
                            .description("Get a user by id.")
                            .tag(TAG),
                        404,
                        422,
                        500,
                        503
                    )
                })
                .put_with(handlers::update_user::<U>, |op| {
                    problems!(
                    op.id("update-user")
                        .summary("update user")
                        .description(
                            "Update an existing user by ID. The ID is taken from the path only; \
                             an `id` field in the body is rejected with 422.",
                        )
                        .tag(TAG),
                    400, 404, 422, 500, 503
                )
                })
                .delete_with(handlers::delete_user::<U>, |op| {
                    problems!(
                        op.id("delete-user")
                            .summary("delete user")
                            .description("Delete a user by ID.")
                            .tag(TAG),
                        404,
                        422,
                        500,
                        503
                    )
                }),
            )
            .api_route(
                "/user",
                post_with(handlers::create_user::<U>, |op| {
                    problems!(
                        op.id("create-user")
                            .summary("create new user")
                            .description("Create a new user record.")
                            .tag(TAG)
                            .response_with::<201, Json<UserDto>, _>(|r| r.description("Created")),
                        400,
                        422,
                        500,
                        503
                    )
                }),
            )
            .api_route(
                "/transfer",
                post_with(handlers::transfer_money::<U>, |op| {
                    problems!(
                        op.id("transfer-money")
                            .summary("transfer money")
                            .description("Transfer money between two accounts.")
                            .tag(TAG),
                        400,
                        404,
                        409,
                        422,
                        500,
                        503
                    )
                }),
            )
            .finish_api_with(&mut api, api_info);
    // Документ сериализуется один раз; маршрут добавляется после `finish_api_with` и
    // только он держит копию, поэтому API-запросы не платят за расширения роутера.
    // Сериализация структуры из строк и чисел не падает; пустое тело — запасной вариант.
    let spec = Bytes::from(serde_json::to_vec(&api).unwrap_or_default());
    let router = router
        .route(
            "/openapi.json",
            get(move || {
                let body = spec.clone();
                async move { ([(header::CONTENT_TYPE, "application/json")], body) }
            }),
        )
        .with_state(Arc::new(usecase));
    (router, api)
}
