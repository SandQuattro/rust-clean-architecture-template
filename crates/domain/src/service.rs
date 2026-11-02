//! Use case пользователей: бизнес-валидация и делегирование репозиторию.
//! Статическая диспетчеризация; use case не логирует.

use crate::command::{
    CreateUser, DeleteUser, FindAllUsers, FindUserById, TransferMoney, UpdateUser,
};
use crate::entity::{User, UserOrders};
use crate::error::{Error, Result};
use crate::port::UserRepository;

#[cfg(test)]
mod tests;

pub struct UserService<R> {
    repo: R,
}

impl<R: UserRepository> UserService<R> {
    pub fn new(repo: R) -> Self {
        Self { repo }
    }

    pub async fn find_all_users(&self, cmd: FindAllUsers) -> Result<Vec<User>> {
        let offset = page_offset(&cmd)?;
        self.repo.get_all_users(offset, cmd.size).await
    }

    pub async fn find_all_users_with_orders(&self, cmd: FindAllUsers) -> Result<Vec<UserOrders>> {
        let offset = page_offset(&cmd)?;
        self.repo.get_all_users_with_orders(offset, cmd.size).await
    }

    pub async fn find_user_by_id(&self, cmd: FindUserById) -> Result<User> {
        self.repo.get_user_by_id(cmd.id).await
    }

    pub async fn create_user(&self, cmd: CreateUser) -> Result<User> {
        validate_user_name(&cmd.name)?;
        self.repo.insert_user(&cmd.name).await
    }

    pub async fn update_user(&self, cmd: UpdateUser) -> Result<User> {
        validate_user_name(&cmd.name)?;
        self.repo.update_user(cmd.id, &cmd.name).await
    }

    pub async fn delete_user(&self, cmd: DeleteUser) -> Result<()> {
        self.repo.delete_user(cmd.id).await
    }

    pub async fn transfer_money(&self, cmd: TransferMoney) -> Result<()> {
        let t = cmd.transfer;
        if t.amount <= 0 {
            return Err(Error::NonPositiveAmount);
        }
        if t.from_account_id == t.to_account_id {
            return Err(Error::SameAccount);
        }
        self.repo.transfer_money(t).await
    }
}

/// Проверяет пагинацию и переводит номер страницы в смещение.
/// Страницы нумеруются с 1; умножение насыщается, чтобы огромная страница не переполняла `i64`.
fn page_offset(cmd: &FindAllUsers) -> Result<i64> {
    if cmd.page < 1 || cmd.size < 1 {
        return Err(Error::InvalidPagination);
    }
    Ok((cmd.page - 1).saturating_mul(cmd.size))
}

/// `String` уже гарантирует UTF-8, остаётся проверить непустоту.
fn validate_user_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(Error::InvalidUserName);
    }
    Ok(())
}
