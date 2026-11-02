//! Порт репозитория. Объявлен в domain: его потребитель — use case.
//! Future каждого метода `Send`, чтобы хендлеры axum могли ждать их напрямую.

use std::future::Future;

use crate::entity::{Transfer, User, UserOrders};
use crate::error::Result;

#[cfg_attr(test, mockall::automock)]
pub trait UserRepository: Send + Sync {
    fn get_all_users(
        &self,
        offset: i64,
        limit: i64,
    ) -> impl Future<Output = Result<Vec<User>>> + Send;

    fn get_all_users_with_orders(
        &self,
        offset: i64,
        limit: i64,
    ) -> impl Future<Output = Result<Vec<UserOrders>>> + Send;

    fn get_user_by_id(&self, id: i64) -> impl Future<Output = Result<User>> + Send;

    fn insert_user(&self, name: &str) -> impl Future<Output = Result<User>> + Send;

    fn update_user(&self, id: i64, name: &str) -> impl Future<Output = Result<User>> + Send;

    fn delete_user(&self, id: i64) -> impl Future<Output = Result<()>> + Send;

    fn transfer_money(&self, transfer: Transfer) -> impl Future<Output = Result<()>> + Send;
}
