//! Пул соединений PostgreSQL, ожидание БД с ограниченным числом попыток и миграции с диска.
//!
//! Модуль не зависит от `config`: вызывающий код собирает [`PoolSettings`] сам
//! (аналог `pkg/database` в Go-шаблоне).

use std::io;
use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use sqlx::migrate::{MigrateError, Migrator};
use sqlx::postgres::{PgConnectOptions, PgConnection, PgPool, PgPoolOptions};
use sqlx::{ConnectOptions, Connection};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, log::LevelFilter};

/// Число попыток подключения к БД по умолчанию (как `_defaultConnAttempts` в Go).
pub const CONNECT_ATTEMPTS: u32 = 10;

/// Соединение, простоявшее в пуле дольше, перед выдачей проверяется ping. Правило pgxpool
/// по умолчанию (`ShouldPing`: `IdleDuration > time.Second`): только что возвращённое
/// соединение выдаётся без лишнего обращения к БД.
const IDLE_PING_AFTER: Duration = Duration::from_secs(1);

/// Параметры пула и ожидания БД.
#[derive(Debug, Clone)]
pub struct PoolSettings {
    pub max_connections: u32,
    pub min_connections: u32,
    /// Верхняя граница одной попытки подключения и пауза между попытками.
    pub connect_timeout: Duration,
    /// Максимальное ожидание свободного соединения из пула.
    pub acquire_timeout: Duration,
    /// Через сколько простоя соединение закрывается. Вызывающий код передаёт сюда
    /// значение `PG_POOL_HEALTHCHECK` (соответствие описано в README).
    pub idle_timeout: Duration,
    /// Сколько раз пробовать подключиться до отказа.
    pub connect_attempts: u32,
    /// `statement_timeout` соединений пула API. Вызывающий код передаёт сюда
    /// `HTTP_REQUEST_TIMEOUT`.
    pub statement_timeout: Duration,
}

#[derive(Debug, thiserror::Error)]
pub enum DatabaseError {
    #[error("invalid database dsn: {0}")]
    InvalidDsn(#[source] sqlx::Error),
    #[error("database is unreachable after {attempts} attempts: {source}")]
    Unreachable {
        attempts: u32,
        #[source]
        source: sqlx::Error,
    },
    #[error("database connection wait cancelled")]
    Cancelled,
    #[error("migrations failed: {0}")]
    Migrations(#[source] MigrateError),
}

/// Параметры подключения из строки `dsn`; `log_statements` включает логирование SQL
/// на уровне debug.
pub fn connect_options(dsn: &str, log_statements: bool) -> Result<PgConnectOptions, DatabaseError> {
    let level = if log_statements {
        LevelFilter::Debug
    } else {
        LevelFilter::Off
    };
    Ok(PgConnectOptions::from_str(dsn)
        .map_err(DatabaseError::InvalidDsn)?
        .log_statements(level))
}

/// Создаёт пул API и ждёт доступности БД: до `connect_attempts` попыток, между
/// неудачными пауза `connect_timeout`. Сигнал `shutdown` прерывает ожидание.
pub async fn connect(
    options: &PgConnectOptions,
    settings: &PoolSettings,
    shutdown: &CancellationToken,
) -> Result<PgPool, DatabaseError> {
    let pool = PgPoolOptions::new()
        .max_connections(settings.max_connections)
        .min_connections(settings.min_connections)
        .acquire_timeout(settings.acquire_timeout)
        .idle_timeout(settings.idle_timeout)
        // `test_before_acquire` пинговал бы перед каждой выдачей. При возврате в пул sqlx
        // пингует соединение всегда, поэтому перед выдачей проверяются только простоявшие.
        .test_before_acquire(false)
        .before_acquire(|conn, meta| {
            Box::pin(async move {
                if meta.idle_for <= IDLE_PING_AFTER {
                    return Ok(true);
                }
                // Как в pgxpool: разорванное соединение молча заменяется новым.
                match conn.ping().await {
                    Ok(()) => Ok(true),
                    Err(err) => {
                        debug!(error = %err, "idle connection failed ping, replacing it");
                        Ok(false)
                    }
                }
            })
        })
        .connect_lazy_with(with_statement_timeout(options, settings.statement_timeout));

    let mut attempts_left = settings.connect_attempts;
    loop {
        let failure = tokio::select! {
            () = shutdown.cancelled() => return Err(DatabaseError::Cancelled),
            res = ping_once(&pool, settings.connect_timeout) => match res {
                Ok(()) => return Ok(pool),
                Err(err) => err,
            },
        };

        attempts_left = attempts_left.saturating_sub(1);
        debug!(attempts_left, error = %failure, "database is not ready, retrying");
        if attempts_left == 0 {
            return Err(DatabaseError::Unreachable {
                attempts: settings.connect_attempts,
                source: failure,
            });
        }

        tokio::select! {
            () = shutdown.cancelled() => return Err(DatabaseError::Cancelled),
            () = tokio::time::sleep(settings.connect_timeout) => {}
        }
    }
}

/// Параметры соединений пула API: `options` и `statement_timeout`.
///
/// Отброшенный future запроса (таймаут HTTP-запроса, клиент ушёл) не отменяет запрос в
/// PostgreSQL, а соединение возвращается в пул только после его завершения. Без предела
/// запрос, ждущий блокировку строки, держал бы соединение и свои блокировки, пока держатель
/// блокировки не завершится. PostgreSQL включает ожидание блокировок в `statement_timeout`,
/// поэтому отдельный `lock_timeout` не нужен.
fn with_statement_timeout(options: &PgConnectOptions, timeout: Duration) -> PgConnectOptions {
    // Целые миллисекунды (значение без единиц PostgreSQL читает как ms) в диапазоне параметра;
    // 0 отключил бы предел.
    let ms = timeout.as_millis().clamp(1, i32::MAX as u128);
    options.clone().options([("statement_timeout", ms)])
}

/// Пул проверки готовности (`/readyz`): одно соединение с параметрами пула API.
///
/// Проба не стоит в очереди пула API: при всплеске все его соединения заняты, и общая
/// очередь давала бы 503 доступной БД, выводя из балансировки все реплики сразу. Пул
/// ленивый (старт его не ждёт); `acquire_timeout` — предел пробы. Перед выдачей соединение
/// не проверяется: проба сама отправляет ping.
pub fn readiness_pool(api: &PgPool, acquire_timeout: Duration) -> PgPool {
    PgPoolOptions::new()
        .max_connections(1)
        .min_connections(0)
        .acquire_timeout(acquire_timeout)
        .test_before_acquire(false)
        .connect_lazy_with((*api.connect_options()).clone())
}

/// Одна попытка: соединение из пула и ping, ограниченные `limit`.
pub(crate) async fn ping_once(pool: &PgPool, limit: Duration) -> Result<(), sqlx::Error> {
    let attempt = async {
        let mut conn = pool.acquire().await?;
        sqlx::Connection::ping(&mut *conn).await
    };
    tokio::time::timeout(limit, attempt)
        .await
        .unwrap_or(Err(sqlx::Error::PoolTimedOut))
}

/// Применяет миграции из каталога `dir` на отдельном соединении с параметрами `options`,
/// без `statement_timeout` пула API: долгая миграция не прерывается пределом HTTP-запроса.
/// Подключение ограничено `connect_timeout`. Блокировку (advisory lock) ставит sqlx.
pub async fn migrate(
    options: &PgConnectOptions,
    dir: &Path,
    connect_timeout: Duration,
) -> Result<(), DatabaseError> {
    let migrator = Migrator::new(dir)
        .await
        .map_err(DatabaseError::Migrations)?;
    let mut conn = tokio::time::timeout(connect_timeout, PgConnection::connect_with(options))
        .await
        .unwrap_or_else(|_| Err(sqlx::Error::Io(io::ErrorKind::TimedOut.into())))
        .map_err(|err| DatabaseError::Migrations(MigrateError::Execute(err)))?;
    let applied = migrator.run(&mut conn).await;
    if let Err(err) = conn.close().await {
        debug!(error = %err, "migration connection did not close cleanly");
    }
    applied.map_err(DatabaseError::Migrations)?;
    // iter() отдаёт и up-, и down-файлы; run() применяет только up.
    let count = migrator
        .iter()
        .filter(|m| !m.migration_type.is_down_migration())
        .count();
    info!(target: "migrations", count, "migrations applied");
    Ok(())
}
