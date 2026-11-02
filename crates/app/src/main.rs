//! Точка входа: конфигурация, телеметрия, сигналы остановки, запуск приложения.

use std::process::ExitCode;
use std::time::Duration;

use app::app::{App, AppError, shutdown_budget};
use app::config::{self, Config};
use app::sysinfo;
use app::telemetry::{self, TelemetrySettings};
use metrics_exporter_prometheus::PrometheusHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

/// Минимальное время на отправку спанов, даже если бюджет остановки исчерпан.
const TRACER_SHUTDOWN_MIN: Duration = Duration::from_secs(1);

#[tokio::main]
async fn main() -> ExitCode {
    let cfg = match config::load() {
        Ok(cfg) => cfg,
        Err(err) => {
            eprintln!("failed to load config: {err}");
            return ExitCode::FAILURE;
        }
    };
    // Подписчик логов ещё не установлен: ошибка инициализации уходит в stderr.
    let telemetry = match telemetry::init(&TelemetrySettings {
        service_name: &cfg.app.name,
        otlp_endpoint: &cfg.tracing.url,
        level: cfg.log.level,
        debug: cfg.app.debug,
        json: cfg.app.is_prod(),
    }) {
        Ok(telemetry) => telemetry,
        Err(err) => {
            eprintln!("failed to init telemetry: {err}");
            return ExitCode::FAILURE;
        }
    };

    let shutdown = CancellationToken::new();
    tokio::spawn(cancel_on_signal(shutdown.clone()));

    sysinfo::log_startup_info(&cfg.app.name, &cfg.app.environment);
    sysinfo::log_memory_info();

    let result = run(&cfg, telemetry.metrics(), shutdown).await;

    // Трейсер останавливается после сервера: спаны дренированных запросов успевают
    // уйти в коллектор. Срок — остаток общего дедлайна остановки; при ошибке дедлайн
    // прошёл или остановки по сигналу не было. Вызов блокирует поток `main`, экспорт
    // идёт на worker-потоках.
    let budget = shutdown_budget(&result, TRACER_SHUTDOWN_MIN);
    if let Err(err) = telemetry.shutdown(budget) {
        error!(error = %err, "failed to shutdown tracer provider");
    }

    match result {
        Ok(_) => {
            info!("Server gracefully stopped, bye, bye!");
            ExitCode::SUCCESS
        }
        Err(err) => {
            error!(error = %err, "application terminated");
            ExitCode::FAILURE
        }
    }
}

/// Возвращает дедлайн остановки (см. [`App::run`]).
async fn run(
    cfg: &Config,
    metrics: PrometheusHandle,
    shutdown: CancellationToken,
) -> Result<Instant, AppError> {
    App::new(cfg, metrics, &shutdown).await?.run(shutdown).await
}

/// Отменяет `shutdown` по SIGINT (Ctrl+C) или SIGTERM.
async fn cancel_on_signal(shutdown: CancellationToken) {
    let interrupt = async {
        if let Err(err) = tokio::signal::ctrl_c().await {
            error!(error = %err, "cannot listen for SIGINT");
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{SignalKind, signal};

        match signal(SignalKind::terminate()) {
            Ok(mut sigterm) => {
                sigterm.recv().await;
            }
            Err(err) => {
                error!(error = %err, "cannot listen for SIGTERM");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = interrupt => {}
        () = terminate => {}
    }
    info!("shutdown signal received");
    shutdown.cancel();
}
