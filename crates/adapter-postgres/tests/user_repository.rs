//! Тесты репозитория на настоящем PostgreSQL: `#[sqlx::test]` создаёт отдельную БД на тест
//! и применяет миграции. Нужен `DATABASE_URL` (см. `make test-db`).

use std::time::Duration;

use adapter_postgres::PgUserRepository;
use domain::entity::Transfer;
use domain::error::Error;
use domain::port::UserRepository;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use tokio::task::JoinSet;
use tokio::time::timeout;

async fn seed_user(pool: &PgPool, name: &str, balance: i64) -> sqlx::Result<i64> {
    sqlx::query_scalar("INSERT INTO users(name, balance) VALUES($1, $2) RETURNING id")
        .bind(name)
        .bind(balance)
        .fetch_one(pool)
        .await
}

async fn seed_order(pool: &PgPool, id: i64, user_id: i64, amount: i64) -> sqlx::Result<()> {
    sqlx::query("INSERT INTO orders(id, user_id, amount) VALUES($1, $2, $3)")
        .bind(id)
        .bind(user_id)
        .bind(amount)
        .execute(pool)
        .await?;
    Ok(())
}

async fn balance_of(pool: &PgPool, id: i64) -> sqlx::Result<i64> {
    sqlx::query_scalar("SELECT balance FROM users WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
}

async fn transactions_count(pool: &PgPool) -> sqlx::Result<i64> {
    sqlx::query_scalar("SELECT count(*) FROM transactions")
        .fetch_one(pool)
        .await
}

fn transfer(from: i64, to: i64, amount: i64) -> Transfer {
    Transfer {
        from_account_id: from,
        to_account_id: to,
        amount,
    }
}

#[sqlx::test(migrations = "../../migrations")]
async fn insert_then_get_returns_user(pool: PgPool) {
    let repo = PgUserRepository::new(pool);

    let created = repo.insert_user("alice").await.unwrap();
    let fetched = repo.get_user_by_id(created.id).await.unwrap();

    assert_eq!(created.name, "alice");
    assert_eq!(fetched, created);
}

#[sqlx::test(migrations = "../../migrations")]
async fn get_missing_user_returns_not_found(pool: PgPool) {
    let repo = PgUserRepository::new(pool);

    let err = repo.get_user_by_id(999_999).await.unwrap_err();

    assert!(matches!(err, Error::UserNotFound), "{err:?}");
}

#[sqlx::test(migrations = "../../migrations")]
async fn update_returns_updated_user(pool: PgPool) {
    let repo = PgUserRepository::new(pool);
    let created = repo.insert_user("alice").await.unwrap();

    let updated = repo.update_user(created.id, "bob").await.unwrap();

    assert_eq!(updated.id, created.id);
    assert_eq!(updated.name, "bob");
    assert_eq!(repo.get_user_by_id(created.id).await.unwrap(), updated);
}

#[sqlx::test(migrations = "../../migrations")]
async fn update_missing_returns_not_found(pool: PgPool) {
    let repo = PgUserRepository::new(pool);

    let err = repo.update_user(999_999, "bob").await.unwrap_err();

    assert!(matches!(err, Error::UserNotFound), "{err:?}");
}

#[sqlx::test(migrations = "../../migrations")]
async fn delete_removes_user(pool: PgPool) {
    let repo = PgUserRepository::new(pool);
    let created = repo.insert_user("alice").await.unwrap();

    repo.delete_user(created.id).await.unwrap();

    let err = repo.get_user_by_id(created.id).await.unwrap_err();
    assert!(matches!(err, Error::UserNotFound), "{err:?}");
}

#[sqlx::test(migrations = "../../migrations")]
async fn delete_missing_returns_not_found(pool: PgPool) {
    let repo = PgUserRepository::new(pool);

    let err = repo.delete_user(999_999).await.unwrap_err();

    assert!(matches!(err, Error::UserNotFound), "{err:?}");
}

#[sqlx::test(migrations = "../../migrations")]
async fn get_all_users_paginates_by_id(pool: PgPool) {
    let repo = PgUserRepository::new(pool);
    repo.insert_user("first").await.unwrap();
    let second = repo.insert_user("second").await.unwrap();
    repo.insert_user("third").await.unwrap();

    let users = repo.get_all_users(1, 1).await.unwrap();

    assert_eq!(users, vec![second]);
}

#[sqlx::test(migrations = "../../migrations")]
async fn get_all_users_huge_offset_returns_empty(pool: PgPool) {
    let repo = PgUserRepository::new(pool);
    repo.insert_user("alice").await.unwrap();

    let users = repo.get_all_users(i64::MAX, 1000).await.unwrap();

    assert!(users.is_empty());
}

#[sqlx::test(migrations = "../../migrations")]
async fn stores_255_multibyte_chars(pool: PgPool) {
    let repo = PgUserRepository::new(pool);
    let name = "я".repeat(255);

    let created = repo.insert_user(&name).await.unwrap();
    let fetched = repo.get_user_by_id(created.id).await.unwrap();

    assert_eq!(fetched.name, name);
}

#[sqlx::test(migrations = "../../migrations")]
async fn users_with_orders_aligns_ids_and_amounts(pool: PgPool) {
    let user = seed_user(&pool, "alice", 0).await.unwrap();
    let lonely = seed_user(&pool, "bob", 0).await.unwrap();
    // Заказы вставлены не по порядку id: i-й id обязан соответствовать i-й сумме.
    seed_order(&pool, 3, user, 30).await.unwrap();
    seed_order(&pool, 1, user, 10).await.unwrap();
    seed_order(&pool, 2, user, 20).await.unwrap();
    let repo = PgUserRepository::new(pool);

    let result = repo.get_all_users_with_orders(0, 10).await.unwrap();

    assert_eq!(result.len(), 2);
    let pairs: Vec<(i64, i64)> = result[0].orders.iter().map(|o| (o.id, o.amount)).collect();
    assert_eq!(pairs, vec![(1, 10), (2, 20), (3, 30)]);
    assert!(result[0].orders.iter().all(|o| o.user_id == user));
    assert_eq!(result[1].id, lonely);
    assert!(result[1].orders.is_empty());
}

#[sqlx::test(migrations = "../../migrations")]
async fn users_with_orders_paginates_before_join(pool: PgPool) {
    let user1 = seed_user(&pool, "alice", 0).await.unwrap();
    let user2 = seed_user(&pool, "bob", 0).await.unwrap();
    seed_order(&pool, 1, user1, 10).await.unwrap();
    seed_order(&pool, 2, user1, 20).await.unwrap();
    seed_order(&pool, 3, user1, 30).await.unwrap();
    seed_order(&pool, 4, user2, 40).await.unwrap();
    let repo = PgUserRepository::new(pool);

    let result = repo.get_all_users_with_orders(0, 1).await.unwrap();

    assert_eq!(result.len(), 1);
    assert_eq!(result[0].id, user1);
    assert_eq!(result[0].orders.len(), 3);
}

#[sqlx::test(migrations = "../../migrations")]
async fn transfer_moves_balance_and_records_transaction(pool: PgPool) {
    let from = seed_user(&pool, "alice", 500).await.unwrap();
    let to = seed_user(&pool, "bob", 0).await.unwrap();
    let repo = PgUserRepository::new(pool.clone());

    repo.transfer_money(transfer(from, to, 200)).await.unwrap();

    assert_eq!(balance_of(&pool, from).await.unwrap(), 300);
    assert_eq!(balance_of(&pool, to).await.unwrap(), 200);
    let rows: Vec<(i64, i64, i64)> =
        sqlx::query_as("SELECT from_user_id, to_user_id, amount FROM transactions")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(rows, vec![(from, to, 200)]);
}

#[sqlx::test(migrations = "../../migrations")]
async fn transfer_insufficient_funds_changes_nothing(pool: PgPool) {
    let from = seed_user(&pool, "alice", 100).await.unwrap();
    let to = seed_user(&pool, "bob", 0).await.unwrap();
    let repo = PgUserRepository::new(pool.clone());

    let err = repo
        .transfer_money(transfer(from, to, 200))
        .await
        .unwrap_err();

    assert!(matches!(err, Error::InsufficientFunds), "{err:?}");
    assert_eq!(balance_of(&pool, from).await.unwrap(), 100);
    assert_eq!(balance_of(&pool, to).await.unwrap(), 0);
    assert_eq!(transactions_count(&pool).await.unwrap(), 0);
}

#[sqlx::test(migrations = "../../migrations")]
async fn transfer_missing_source(pool: PgPool) {
    let to = seed_user(&pool, "bob", 0).await.unwrap();
    let repo = PgUserRepository::new(pool);

    let err = repo
        .transfer_money(transfer(999_999, to, 1))
        .await
        .unwrap_err();

    assert!(matches!(err, Error::SourceAccountNotFound), "{err:?}");
}

#[sqlx::test(migrations = "../../migrations")]
async fn transfer_missing_destination(pool: PgPool) {
    let from = seed_user(&pool, "alice", 100).await.unwrap();
    let repo = PgUserRepository::new(pool);

    let err = repo
        .transfer_money(transfer(from, 999_999, 1))
        .await
        .unwrap_err();

    assert!(matches!(err, Error::DestAccountNotFound), "{err:?}");
}

#[sqlx::test(migrations = "../../migrations")]
async fn opposite_concurrent_transfers_do_not_deadlock(pool: PgPool) {
    let a = seed_user(&pool, "alice", 1000).await.unwrap();
    let b = seed_user(&pool, "bob", 1000).await.unwrap();
    let repo = PgUserRepository::new(pool.clone());

    let mut tasks = JoinSet::new();
    for _ in 0..50 {
        let r = repo.clone();
        tasks.spawn(async move { r.transfer_money(transfer(a, b, 1)).await });
        let r = repo.clone();
        tasks.spawn(async move { r.transfer_money(transfer(b, a, 1)).await });
    }

    let results = timeout(Duration::from_secs(10), tasks.join_all())
        .await
        .expect("переводы зависли: возможен deadlock");

    assert_eq!(results.len(), 100);
    for res in results {
        res.unwrap();
    }
    let total = balance_of(&pool, a).await.unwrap() + balance_of(&pool, b).await.unwrap();
    assert_eq!(total, 2000);
}

#[sqlx::test(migrations = "../../migrations")]
async fn cancelled_transfers_keep_balances_consistent(pool: PgPool) {
    let a = seed_user(&pool, "alice", 1000).await.unwrap();
    let b = seed_user(&pool, "bob", 1000).await.unwrap();
    let repo = PgUserRepository::new(pool.clone());

    // Блокируем строку B из отдельного соединения. Перевод A→B берёт блокировки по порядку id:
    // сначала A, затем зависает в ожидании B, то есть отменяется уже с блокировкой на A.
    let mut blocker = pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM users WHERE id = $1 FOR UPDATE")
        .bind(b)
        .fetch_one(&mut *blocker)
        .await
        .unwrap();

    let cancelled = timeout(
        Duration::from_millis(300),
        repo.transfer_money(transfer(a, b, 1)),
    )
    .await;
    assert!(
        cancelled.is_err(),
        "перевод должен быть отменён по таймауту, а не завершиться"
    );

    blocker.rollback().await.unwrap();

    // Отменённый перевод ничего не записал.
    assert_eq!(balance_of(&pool, a).await.unwrap(), 1000);
    assert_eq!(balance_of(&pool, b).await.unwrap(), 1000);
    assert_eq!(transactions_count(&pool).await.unwrap(), 0);

    // Блокировка A снята: новый перевод проходит.
    timeout(
        Duration::from_secs(5),
        repo.transfer_money(transfer(a, b, 1)),
    )
    .await
    .expect("перевод после отмены не завершился: блокировки не сняты")
    .unwrap();
    assert_eq!(balance_of(&pool, a).await.unwrap(), 999);
    assert_eq!(balance_of(&pool, b).await.unwrap(), 1001);
}

#[sqlx::test(migrations = "../../migrations")]
async fn pool_timeout_maps_to_unavailable(pool: PgPool) {
    let tiny = PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_millis(100))
        .connect_with((*pool.connect_options()).clone())
        .await
        .unwrap();
    let repo = PgUserRepository::new(tiny.clone());
    let _held = tiny.acquire().await.unwrap();

    let err = repo.get_user_by_id(1).await.unwrap_err();

    assert!(matches!(err, Error::Unavailable { .. }), "{err:?}");
}
