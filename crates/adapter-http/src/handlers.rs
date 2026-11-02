//! Хендлеры: тонкая прослойка между HTTP и `UserUseCase`.
//! Имена internal-спанов совпадают с Go-шаблоном; серверный спан создаёт middleware.

use std::sync::Arc;

use axum::{Json, extract::State, http::StatusCode};
use domain::command::{
    CreateUser, DeleteUser, FindAllUsers, FindUserById, TransferMoney, UpdateUser,
};

use crate::{
    dto::{
        IdParam, PageParams, TransferBody, UserBody, UserDto, UsersResponse,
        UsersWithOrdersResponse,
    },
    error::ApiError,
    extract::{ValidJson, ValidPath},
    usecase::UserUseCase,
};

#[tracing::instrument(skip_all, name = "ListUsers")]
pub async fn list_users<U: UserUseCase>(
    State(uc): State<Arc<U>>,
    ValidPath(p): ValidPath<PageParams>,
) -> Result<Json<UsersResponse>, ApiError> {
    let users = uc
        .find_all_users(FindAllUsers {
            page: p.page,
            size: p.size,
        })
        .await?;
    Ok(Json(users.into()))
}

#[tracing::instrument(skip_all, name = "ListUsersWithOrders")]
pub async fn list_users_with_orders<U: UserUseCase>(
    State(uc): State<Arc<U>>,
    ValidPath(p): ValidPath<PageParams>,
) -> Result<Json<UsersWithOrdersResponse>, ApiError> {
    let users = uc
        .find_all_users_with_orders(FindAllUsers {
            page: p.page,
            size: p.size,
        })
        .await?;
    Ok(Json(users.into()))
}

#[tracing::instrument(skip_all, name = "FindUserByID")]
pub async fn get_user_by_id<U: UserUseCase>(
    State(uc): State<Arc<U>>,
    ValidPath(p): ValidPath<IdParam>,
) -> Result<Json<UserDto>, ApiError> {
    let user = uc.find_user_by_id(FindUserById { id: p.id }).await?;
    Ok(Json(user.into()))
}

#[tracing::instrument(skip_all, name = "CreateUser")]
pub async fn create_user<U: UserUseCase>(
    State(uc): State<Arc<U>>,
    ValidJson(body): ValidJson<UserBody>,
) -> Result<(StatusCode, Json<UserDto>), ApiError> {
    let user = uc.create_user(CreateUser { name: body.name }).await?;
    Ok((StatusCode::CREATED, Json(user.into())))
}

#[tracing::instrument(skip_all, name = "UpdateUser")]
pub async fn update_user<U: UserUseCase>(
    State(uc): State<Arc<U>>,
    ValidPath(p): ValidPath<IdParam>,
    ValidJson(body): ValidJson<UserBody>,
) -> Result<Json<UserDto>, ApiError> {
    let user = uc
        .update_user(UpdateUser {
            id: p.id,
            name: body.name,
        })
        .await?;
    Ok(Json(user.into()))
}

#[tracing::instrument(skip_all, name = "DeleteUser")]
pub async fn delete_user<U: UserUseCase>(
    State(uc): State<Arc<U>>,
    ValidPath(p): ValidPath<IdParam>,
) -> Result<axum::response::NoContent, ApiError> {
    uc.delete_user(DeleteUser { id: p.id }).await?;
    Ok(axum::response::NoContent)
}

#[tracing::instrument(skip_all, name = "TransferMoney")]
pub async fn transfer_money<U: UserUseCase>(
    State(uc): State<Arc<U>>,
    ValidJson(body): ValidJson<TransferBody>,
) -> Result<axum::response::NoContent, ApiError> {
    uc.transfer_money(TransferMoney {
        transfer: body.into(),
    })
    .await?;
    Ok(axum::response::NoContent)
}
