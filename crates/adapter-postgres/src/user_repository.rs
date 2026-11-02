//! Репозиторий пользователей на чистом SQL (проверяется на этапе компиляции через `sqlx::query!`).
//! Тексты запросов совпадают с Go-шаблоном. Репозиторий не логирует.

use domain::entity::{Order, Transfer, User, UserOrders};
use domain::error::{Error, Result};
use domain::port::UserRepository;
use sqlx::PgPool;

#[derive(Clone)]
pub struct PgUserRepository {
    pool: PgPool,
}

impl PgUserRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

/// Единственная точка перевода ошибок sqlx в доменные:
/// таймаут ожидания соединения из пула — `Unavailable`, остальное — `Internal`.
fn map_db_error(context: &'static str, err: sqlx::Error) -> Error {
    match err {
        sqlx::Error::PoolTimedOut => Error::unavailable(context, err),
        _ => Error::internal(context, err),
    }
}

impl UserRepository for PgUserRepository {
    async fn get_all_users(&self, offset: i64, limit: i64) -> Result<Vec<User>> {
        let rows = sqlx::query!(
            "SELECT u.id, u.name FROM users u ORDER BY u.id OFFSET $1 LIMIT $2",
            offset,
            limit
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| map_db_error("query users", e))?;

        Ok(rows
            .into_iter()
            .map(|r| User {
                id: r.id,
                name: r.name,
            })
            .collect())
    }

    async fn get_all_users_with_orders(&self, offset: i64, limit: i64) -> Result<Vec<UserOrders>> {
        // Страница пользователей выбирается в подзапросе до JOIN: агрегируются заказы
        // только этой страницы. Оба агрегата упорядочены по o.id: i-й id обязан
        // соответствовать i-й сумме.
        let rows = sqlx::query!(
            r#"
            SELECT u.id,
                   u.name,
                   COALESCE(array_agg(o.id ORDER BY o.id) FILTER (WHERE o.id IS NOT NULL), '{}') as "order_ids!: Vec<i64>",
                   COALESCE(array_agg(o.amount ORDER BY o.id) FILTER (WHERE o.id IS NOT NULL), '{}') as "order_amounts!: Vec<i64>"
            FROM (
                SELECT id, name
                FROM users
                ORDER BY id
                OFFSET $1 LIMIT $2
            ) u
            LEFT JOIN orders o ON u.id = o.user_id
            GROUP BY u.id, u.name
            ORDER BY u.id
            "#,
            offset,
            limit
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| map_db_error("query users with orders", e))?;

        Ok(rows
            .into_iter()
            .map(|r| {
                let user_id = r.id;
                let orders = r
                    .order_ids
                    .into_iter()
                    .zip(r.order_amounts)
                    .map(|(id, amount)| Order {
                        id,
                        user_id,
                        amount,
                    })
                    .collect();
                UserOrders {
                    id: user_id,
                    name: r.name,
                    orders,
                }
            })
            .collect())
    }

    async fn get_user_by_id(&self, id: i64) -> Result<User> {
        let row = sqlx::query!("SELECT u.id, u.name FROM users u WHERE u.id = $1", id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| map_db_error("query user by id", e))?
            .ok_or(Error::UserNotFound)?;

        Ok(User {
            id: row.id,
            name: row.name,
        })
    }

    async fn insert_user(&self, name: &str) -> Result<User> {
        let row = sqlx::query!(
            "INSERT INTO users(name) VALUES($1) RETURNING id, name",
            name
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|e| map_db_error("insert user", e))?;

        Ok(User {
            id: row.id,
            name: row.name,
        })
    }

    async fn update_user(&self, id: i64, name: &str) -> Result<User> {
        // Одним запросом, без предварительного чтения: RETURNING отличает
        // «обновлено» от «не найдено» атомарно.
        let row = sqlx::query!(
            "UPDATE users SET name = $2, updated_at = CURRENT_TIMESTAMP WHERE id = $1 RETURNING id, name",
            id,
            name
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| map_db_error("update user", e))?
        .ok_or(Error::UserNotFound)?;

        Ok(User {
            id: row.id,
            name: row.name,
        })
    }

    async fn delete_user(&self, id: i64) -> Result<()> {
        let done = sqlx::query!("DELETE FROM users WHERE id = $1", id)
            .execute(&self.pool)
            .await
            .map_err(|e| map_db_error("delete user", e))?;
        if done.rows_affected() == 0 {
            return Err(Error::UserNotFound);
        }
        Ok(())
    }

    async fn transfer_money(&self, transfer: Transfer) -> Result<()> {
        // Выход без commit (ошибка или отмена future) откатывает транзакцию при drop.
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| map_db_error("begin transaction", e))?;

        // Блокируем обе строки одним запросом в детерминированном порядке (ORDER BY id),
        // иначе встречные переводы A→B и B→A взаимно блокируются (deadlock).
        let accounts = sqlx::query!(
            "SELECT id, balance FROM users WHERE id = ANY($1) ORDER BY id FOR UPDATE",
            &[transfer.from_account_id, transfer.to_account_id][..]
        )
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| map_db_error("lock accounts", e))?;

        let source = accounts
            .iter()
            .find(|a| a.id == transfer.from_account_id)
            .ok_or(Error::SourceAccountNotFound)?;
        if !accounts.iter().any(|a| a.id == transfer.to_account_id) {
            return Err(Error::DestAccountNotFound);
        }
        if source.balance < transfer.amount {
            return Err(Error::InsufficientFunds);
        }

        sqlx::query!(
            "UPDATE users SET balance = balance - $1, updated_at = CURRENT_TIMESTAMP WHERE id = $2",
            transfer.amount,
            transfer.from_account_id
        )
        .execute(&mut *tx)
        .await
        .map_err(|e| map_db_error("update source account", e))?;

        sqlx::query!(
            "UPDATE users SET balance = balance + $1, updated_at = CURRENT_TIMESTAMP WHERE id = $2",
            transfer.amount,
            transfer.to_account_id
        )
        .execute(&mut *tx)
        .await
        .map_err(|e| map_db_error("update destination account", e))?;

        sqlx::query!(
            "INSERT INTO transactions(from_user_id, to_user_id, amount) VALUES($1, $2, $3)",
            transfer.from_account_id,
            transfer.to_account_id,
            transfer.amount
        )
        .execute(&mut *tx)
        .await
        .map_err(|e| map_db_error("create transaction", e))?;

        tx.commit()
            .await
            .map_err(|e| map_db_error("commit transaction", e))
    }
}
