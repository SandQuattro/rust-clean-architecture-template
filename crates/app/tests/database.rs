//! Тесты пула и миграций. Тесты с `#[sqlx::test]` требуют `DATABASE_URL`.

// `allow-*-in-tests` из clippy.toml покрывает только функции `#[test]`, а не
// вспомогательный код тестового крейта.
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test-only helpers")]

use std::path::Path;
use std::time::{Duration, Instant};

use adapter_postgres::PgUserRepository;
use app::database::{DatabaseError, PoolSettings, connect, connect_options, migrate};
use domain::entity::Transfer;
use domain::error::Error;
use domain::port::UserRepository;
use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgConnection, PgSslMode};
use tokio_util::sync::CancellationToken;

/// Предел ожидания событий: сломанная реализация роняет тест, а не вешает его.
const WAIT: Duration = Duration::from_secs(5);

fn settings(attempts: u32, connect_timeout: Duration) -> PoolSettings {
    PoolSettings {
        max_connections: 2,
        min_connections: 0,
        connect_timeout,
        acquire_timeout: Duration::from_secs(1),
        idle_timeout: Duration::from_secs(30),
        connect_attempts: attempts,
        statement_timeout: Duration::from_secs(30),
    }
}

/// Параметры подключения к закрытому порту.
fn unreachable() -> PgConnectOptions {
    connect_options("postgres://u@127.0.0.1:1/db", false).unwrap()
}

/// Пул API из одного соединения к тестовой БД `db`.
async fn single_connection_pool(db: &PgPool) -> PgPool {
    single_connection_pool_with(db, Duration::from_secs(30)).await
}

/// Пул API из одного соединения к тестовой БД `db` с заданным `statement_timeout`.
async fn single_connection_pool_with(db: &PgPool, statement_timeout: Duration) -> PgPool {
    let settings = PoolSettings {
        max_connections: 1,
        statement_timeout,
        ..settings(1, Duration::from_secs(1))
    };
    connect(&db.connect_options(), &settings, &CancellationToken::new())
        .await
        .unwrap()
}

async fn seed_user(db: &PgPool, name: &str, balance: i64) -> i64 {
    sqlx::query_scalar("INSERT INTO users(name, balance) VALUES($1, $2) RETURNING id")
        .bind(name)
        .bind(balance)
        .fetch_one(db)
        .await
        .unwrap()
}

/// Номер серверного процесса соединения.
async fn backend_pid(conn: &mut PgConnection) -> sqlx::Result<i32> {
    sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(conn)
        .await
}

/// Номер серверного процесса единственного соединения пула; соединение возвращается
/// в пул (sqlx делает это фоновой задачей, поэтому возврат ожидается).
async fn idle_backend_pid(pool: &PgPool) -> i32 {
    let pid = backend_pid(&mut pool.acquire().await.unwrap())
        .await
        .unwrap();
    tokio::time::timeout(WAIT, async {
        while pool.num_idle() == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("connection must return to the pool");
    pid
}

/// Завершает серверный процесс `pid` из соединения `db` и ждёт его выхода.
async fn terminate_backend(db: &PgPool, pid: i32) {
    let terminated: bool = sqlx::query_scalar("SELECT pg_terminate_backend($1, 5000)")
        .bind(pid)
        .fetch_one(db)
        .await
        .unwrap();
    assert!(terminated, "backend {pid} must exit");
}

#[tokio::test]
async fn connect_fails_after_attempts() {
    let started = Instant::now();
    let res = connect(
        &unreachable(),
        &settings(2, Duration::from_millis(50)),
        &CancellationToken::new(),
    )
    .await;

    assert!(matches!(
        res,
        Err(DatabaseError::Unreachable { attempts: 2, .. })
    ));
    assert!(started.elapsed() < Duration::from_secs(2));
}

/// TLS включён в сборку sqlx: тестовый Postgres без TLS отклоняется самим сервером
/// («server does not support TLS»), а не сообщением «built without TLS support».
#[sqlx::test(migrations = false)]
async fn sslmode_require_reaches_tls_negotiation(db: PgPool) {
    let options = db
        .connect_options()
        .as_ref()
        .clone()
        .ssl_mode(PgSslMode::Require);
    let res = connect(
        &options,
        &settings(1, Duration::from_secs(2)),
        &CancellationToken::new(),
    )
    .await;

    let Err(DatabaseError::Unreachable { source, .. }) = res else {
        panic!("sslmode=require against a server without TLS must fail: {res:?}");
    };
    let sqlx::Error::Tls(err) = &source else {
        panic!("expected a TLS error: {source:?}");
    };
    assert!(
        err.to_string().contains("server does not support TLS"),
        "{err}"
    );
}

#[tokio::test]
async fn connect_stops_on_shutdown() {
    let shutdown = CancellationToken::new();
    let canceller = shutdown.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        canceller.cancel();
    });

    let started = Instant::now();
    let res = connect(
        &unreachable(),
        &settings(100, Duration::from_secs(1)),
        &shutdown,
    )
    .await;

    assert!(matches!(res, Err(DatabaseError::Cancelled)));
    assert!(started.elapsed() < Duration::from_secs(1));
}

/// Предел подключения мигратора в тестах.
const MIGRATE_CONNECT: Duration = Duration::from_secs(1);

#[tokio::test]
async fn migrate_fails_on_missing_dir() {
    let res = migrate(&unreachable(), Path::new("/nonexistent"), MIGRATE_CONNECT).await;

    assert!(matches!(res, Err(DatabaseError::Migrations(_))));
}

#[tokio::test]
async fn migrate_fails_on_unreachable_db() {
    let res = migrate(
        &unreachable(),
        Path::new("../../migrations"),
        MIGRATE_CONNECT,
    )
    .await;

    assert!(matches!(res, Err(DatabaseError::Migrations(_))), "{res:?}");
}

#[sqlx::test(migrations = false)]
async fn migrate_fails_on_broken_sql(pool: PgPool) {
    let dir = Path::new("tests/fixtures/broken_migrations");

    let res = migrate(&pool.connect_options(), dir, MIGRATE_CONNECT).await;

    assert!(matches!(res, Err(DatabaseError::Migrations(_))));
}

#[sqlx::test(migrations = false)]
async fn migrate_is_idempotent(pool: PgPool) {
    let dir = Path::new("../../migrations");
    let options = pool.connect_options();

    migrate(&options, dir, MIGRATE_CONNECT)
        .await
        .expect("first run");
    migrate(&options, dir, MIGRATE_CONNECT)
        .await
        .expect("second run");

    let exists: bool = sqlx::query_scalar("SELECT to_regclass('public.users') IS NOT NULL")
        .fetch_one(&pool)
        .await
        .expect("query");
    assert!(exists);
}

/// Соединение, простоявшее в пуле дольше 1 с, проверяется ping перед выдачей:
/// соединение, разорванное за время простоя, незаметно заменяется новым.
#[sqlx::test(migrations = false)]
async fn long_idle_connection_is_pinged_and_replaced(db: PgPool) {
    let pool = single_connection_pool(&db).await;
    let pid = idle_backend_pid(&pool).await;
    terminate_backend(&db, pid).await;
    tokio::time::sleep(Duration::from_millis(1200)).await;

    let mut conn = pool.acquire().await.unwrap();
    let new_pid = backend_pid(&mut conn)
        .await
        .expect("broken idle connection must be replaced on acquire");

    assert_ne!(new_pid, pid);
    drop(conn);
    pool.close().await;
}

/// Соединение, простоявшее меньше 1 с, выдаётся без ping (правило pgxpool): выдача из
/// пула не добавляет обращения к БД. Поэтому разрыв, случившийся за этот короткий
/// простой, обнаруживает только сам запрос.
#[sqlx::test(migrations = false)]
async fn recently_released_connection_is_not_pinged(db: PgPool) {
    let pool = single_connection_pool(&db).await;
    let pid = idle_backend_pid(&pool).await;
    terminate_backend(&db, pid).await;

    let mut conn = pool.acquire().await.unwrap();
    let res = backend_pid(&mut conn).await;

    assert!(res.is_err(), "acquire must not ping: {res:?}");
    drop(conn);
    pool.close().await;
}

/// Перевод A→B ждёт блокировку строки B, которую держит другая транзакция. `statement_timeout`
/// пула API прерывает ожидание без участия держателя блокировки: перевод получает ошибку
/// примерно через этот предел, а его соединение и блокировка строки A освобождаются, пока
/// B ещё заблокирована.
#[sqlx::test(migrations = "../../migrations")]
async fn statement_timeout_releases_transfer_blocked_on_row_lock(db: PgPool) {
    const STATEMENT_TIMEOUT: Duration = Duration::from_millis(300);
    let a = seed_user(&db, "alice", 1000).await;
    let b = seed_user(&db, "bob", 1000).await;
    let pool = single_connection_pool_with(&db, STATEMENT_TIMEOUT).await;
    let repo = PgUserRepository::new(pool.clone());
    let mut blocker = db.begin().await.unwrap();
    sqlx::query("SELECT id FROM users WHERE id = $1 FOR UPDATE")
        .bind(b)
        .fetch_one(&mut *blocker)
        .await
        .unwrap();

    let started = Instant::now();
    let res = tokio::time::timeout(
        WAIT,
        repo.transfer_money(Transfer {
            from_account_id: a,
            to_account_id: b,
            amount: 1,
        }),
    )
    .await
    .expect("transfer must fail by statement_timeout instead of waiting for the blocker");
    let elapsed = started.elapsed();

    // Отмена по `statement_timeout` (SQLSTATE 57014) — не таймаут пула, поэтому `Internal`.
    assert!(
        matches!(
            res,
            Err(Error::Internal {
                context: "lock accounts",
                ..
            })
        ),
        "{res:?}"
    );
    assert!(
        elapsed >= STATEMENT_TIMEOUT && elapsed < STATEMENT_TIMEOUT + Duration::from_secs(1),
        "{elapsed:?}"
    );
    // Единственное соединение пула свободно, строка A не заблокирована, B всё ещё у blocker.
    let mut tx = pool
        .begin()
        .await
        .expect("connection must be back in the pool");
    sqlx::query("SELECT id FROM users WHERE id = $1 FOR UPDATE NOWAIT")
        .bind(a)
        .fetch_one(&mut *tx)
        .await
        .expect("row A must be unlocked");
    tx.rollback().await.unwrap();
    blocker.rollback().await.unwrap();
    pool.close().await;
}

/// Миграции не подчиняются `statement_timeout` пула API: долгая миграция не прерывается.
#[sqlx::test(migrations = false)]
async fn migrations_ignore_api_statement_timeout(db: PgPool) {
    let pool = single_connection_pool_with(&db, Duration::from_millis(100)).await;
    // Через пул API запрос дольше предела прерывается (SQLSTATE 57014, query_canceled).
    let err = sqlx::query("SELECT pg_sleep(0.5)")
        .execute(&pool)
        .await
        .unwrap_err();
    let code = err.as_database_error().and_then(|e| e.code());
    assert_eq!(code.as_deref(), Some("57014"), "{err}");

    // Те же параметры подключения, из которых построен пул API.
    let dir = Path::new("tests/fixtures/slow_migrations");
    migrate(&db.connect_options(), dir, MIGRATE_CONNECT)
        .await
        .expect("slow migration must not be cancelled");
    pool.close().await;
}
