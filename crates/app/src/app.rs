//! Сборка и запуск приложения: БД, миграции, роутер, HTTP-сервер.

use std::io;
use std::time::Duration;

use adapter_postgres::PgUserRepository;
use axum::Router;
use domain::service::UserService;
use metrics_exporter_prometheus::PrometheusHandle;
use sqlx::PgPool;
use sqlx::postgres::PgConnectOptions;
use tokio::net::TcpListener;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::config::Config;
use crate::database::{self, CONNECT_ATTEMPTS, DatabaseError, PoolSettings};
use crate::probes::READY_TIMEOUT;
use crate::router::{RouterSettings, build_router};
use crate::server::{self, ServeError, ServerTimeouts};

/// Минимальное время на закрытие пула, даже если бюджет остановки исчерпан.
const POOL_CLOSE_MIN: Duration = Duration::from_millis(500);

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error(transparent)]
    Database(#[from] DatabaseError),
    #[error("startup cancelled by shutdown signal")]
    Cancelled,
    #[error("bind HTTP port {port}: {source}")]
    Bind {
        port: u16,
        #[source]
        source: io::Error,
    },
    #[error("http server: {0}")]
    Serve(#[from] ServeError),
}

/// Готовое к запуску приложение: пулы, роутер и занятый порт.
pub struct App {
    pool: PgPool,
    /// Пул проверки готовности (`/readyz`), см. [`database::readiness_pool`].
    ready_pool: PgPool,
    router: Router,
    listener: TcpListener,
    timeouts: ServerTimeouts,
}

impl App {
    /// Подключается к БД, применяет миграции, собирает роутер и занимает порт.
    ///
    /// `shutdown` прерывает ожидание БД и миграции. Глобальный регистратор метрик
    /// должен быть уже установлен: роутер регистрирует описания HTTP-метрик.
    pub async fn new(
        cfg: &Config,
        metrics: PrometheusHandle,
        shutdown: &CancellationToken,
    ) -> Result<Self, AppError> {
        let settings = PoolSettings {
            max_connections: cfg.db.pool_max,
            min_connections: cfg.db.pool_min,
            connect_timeout: cfg.db.connect_timeout,
            acquire_timeout: cfg.db.acquire_timeout,
            idle_timeout: cfg.db.health_check_period,
            connect_attempts: CONNECT_ATTEMPTS,
            // Запрос к БД не переживает HTTP-запрос, который его начал (см. README).
            statement_timeout: cfg.http.request_timeout,
        };
        let options = database::connect_options(&cfg.db.dsn(), cfg.app.debug)?;
        let pool = database::connect(&options, &settings, shutdown).await?;
        // Миграции идут на `options` без `statement_timeout` пула API.
        let listener = match migrate_and_bind(cfg, &options, shutdown).await {
            Ok(listener) => listener,
            Err(err) => {
                close_pool(&pool, cfg.http.shutdown_timeout).await;
                return Err(err);
            }
        };

        let ready_pool = database::readiness_pool(&pool, READY_TIMEOUT);
        let router = build_router(
            UserService::new(PgUserRepository::new(pool.clone())),
            pool.clone(),
            ready_pool.clone(),
            metrics,
            &RouterSettings {
                max_concurrency: cfg.http.max_concurrency,
                request_timeout: cfg.http.request_timeout,
                read_timeout: cfg.http.read_timeout,
                write_timeout: cfg.http.write_timeout,
                dev_routes: cfg.app.is_dev(),
            },
        );
        Ok(Self {
            pool,
            ready_pool,
            router,
            listener,
            timeouts: ServerTimeouts {
                header_read: cfg.http.read_timeout,
                idle: cfg.http.idle_timeout,
                shutdown: cfg.http.shutdown_timeout,
            },
        })
    }

    /// Обслуживает HTTP до отмены `shutdown`, дренирует соединения, затем закрывает пулы.
    ///
    /// Вся остановка укладывается в один дедлайн: сигнал + `HTTP_SHUTDOWN_TIMEOUT`.
    /// Возвращает его, чтобы `main` ограничил остатком остановку трейсера.
    pub async fn run(self, shutdown: CancellationToken) -> Result<Instant, AppError> {
        let served = server::serve(self.listener, self.router, self.timeouts, shutdown).await;
        // Пулы закрываются после сервера: дренированные запросы ещё пользуются ими.
        // Оба закрываются одновременно в одном бюджете.
        let budget = shutdown_budget(&served, POOL_CLOSE_MIN);
        tokio::join!(
            close_pool(&self.pool, budget),
            close_pool(&self.ready_pool, budget)
        );
        let deadline = served?;
        info!("Server stopped");
        Ok(deadline)
    }
}

/// Остаток бюджета остановки до `deadline`, но не меньше `floor`: даже после
/// исчерпания бюджета пул и трейсер получают короткое время на корректное закрытие.
pub fn remaining(deadline: Instant, now: Instant, floor: Duration) -> Duration {
    deadline.saturating_duration_since(now).max(floor)
}

/// Бюджет на закрытие ресурса после остановки сервера по его результату (см. [`App::run`]).
/// Ошибка сервера: дедлайн уже прошёл (тайм-аут дренажа) или сервер не стартовал.
pub fn shutdown_budget<E>(served: &Result<Instant, E>, floor: Duration) -> Duration {
    let deadline = served.as_ref().copied().unwrap_or_else(|_| Instant::now());
    remaining(deadline, Instant::now(), floor)
}

async fn migrate_and_bind(
    cfg: &Config,
    options: &PgConnectOptions,
    shutdown: &CancellationToken,
) -> Result<TcpListener, AppError> {
    let migrate = database::migrate(options, &cfg.db.migrations_dir, cfg.db.connect_timeout);
    tokio::select! {
        () = shutdown.cancelled() => return Err(AppError::Cancelled),
        migrated = migrate => migrated?,
    }
    let port = cfg.http.port;
    TcpListener::bind(("0.0.0.0", port))
        .await
        .map_err(|source| AppError::Bind { port, source })
}

/// Закрывает пул не дольше `limit`: зависшее соединение не задерживает выход.
async fn close_pool(pool: &PgPool, limit: Duration) {
    if tokio::time::timeout(limit, pool.close()).await.is_err() {
        warn!(?limit, "database pool did not close in time");
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::time::Instant;

    use super::remaining;

    #[test]
    fn remaining_budget_is_never_below_floor() {
        let t0 = Instant::now();
        let floor = Duration::from_millis(500);
        // (дедлайн, текущий момент, ожидаемый бюджет)
        let cases = [
            (t0 + Duration::from_secs(3), t0, Duration::from_secs(3)),
            (t0 + Duration::from_millis(200), t0, floor),
            (t0, t0 + Duration::from_secs(1), floor),
        ];
        for (deadline, now, expected) in cases {
            assert_eq!(remaining(deadline, now, floor), expected);
        }
    }
}
