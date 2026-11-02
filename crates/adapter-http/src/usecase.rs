//! Порт use case, объявленный потребителем (HTTP-адаптером) и реализованный `domain::UserService`.

use std::future::Future;

use domain::{
    command::{CreateUser, DeleteUser, FindAllUsers, FindUserById, TransferMoney, UpdateUser},
    entity::{User, UserOrders},
    error::Result,
    port::UserRepository,
    service::UserService,
};

#[cfg_attr(test, mockall::automock)]
pub trait UserUseCase: Send + Sync + 'static {
    fn find_all_users(&self, cmd: FindAllUsers) -> impl Future<Output = Result<Vec<User>>> + Send;
    fn find_all_users_with_orders(
        &self,
        cmd: FindAllUsers,
    ) -> impl Future<Output = Result<Vec<UserOrders>>> + Send;
    fn find_user_by_id(&self, cmd: FindUserById) -> impl Future<Output = Result<User>> + Send;
    fn create_user(&self, cmd: CreateUser) -> impl Future<Output = Result<User>> + Send;
    fn update_user(&self, cmd: UpdateUser) -> impl Future<Output = Result<User>> + Send;
    fn delete_user(&self, cmd: DeleteUser) -> impl Future<Output = Result<()>> + Send;
    fn transfer_money(&self, cmd: TransferMoney) -> impl Future<Output = Result<()>> + Send;
}

// Делегирование одновременно проверяет на этапе компиляции, что futures домена `Send`.
impl<R: UserRepository + 'static> UserUseCase for UserService<R> {
    fn find_all_users(&self, cmd: FindAllUsers) -> impl Future<Output = Result<Vec<User>>> + Send {
        UserService::find_all_users(self, cmd)
    }

    fn find_all_users_with_orders(
        &self,
        cmd: FindAllUsers,
    ) -> impl Future<Output = Result<Vec<UserOrders>>> + Send {
        UserService::find_all_users_with_orders(self, cmd)
    }

    fn find_user_by_id(&self, cmd: FindUserById) -> impl Future<Output = Result<User>> + Send {
        UserService::find_user_by_id(self, cmd)
    }

    fn create_user(&self, cmd: CreateUser) -> impl Future<Output = Result<User>> + Send {
        UserService::create_user(self, cmd)
    }

    fn update_user(&self, cmd: UpdateUser) -> impl Future<Output = Result<User>> + Send {
        UserService::update_user(self, cmd)
    }

    fn delete_user(&self, cmd: DeleteUser) -> impl Future<Output = Result<()>> + Send {
        UserService::delete_user(self, cmd)
    }

    fn transfer_money(&self, cmd: TransferMoney) -> impl Future<Output = Result<()>> + Send {
        UserService::transfer_money(self, cmd)
    }
}
