//! Метрики Prometheus: глобальный регистратор и рендер `/metrics`.

use std::io;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::Duration;

use metrics::Unit;
use metrics_exporter_prometheus::{
    Matcher, PrometheusBuilder, PrometheusHandle, PrometheusRecorder,
};
use sqlx::PgPool;

use super::TelemetryError;
use crate::http_metrics;

/// Период обслуживания регистратора (как `upkeep_timeout` по умолчанию в
/// metrics-exporter-prometheus).
const UPKEEP_INTERVAL: Duration = Duration::from_secs(5);

/// Границы гистограммы длительности HTTP-запросов, секунды: от 5 ms до тайм-аута
/// запроса по умолчанию (30 s).
const HTTP_DURATION_BUCKETS: [f64; 12] = [
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0,
];

/// Регистратор без встроенного HTTP-сервера: `/metrics` отдаёт роутер.
///
/// Длительность HTTP-запросов — гистограмма с явными границами: без них экспортер
/// рендерит summary, а квантили summary нельзя агрегировать по репликам.
pub(super) fn build_recorder() -> Result<PrometheusRecorder, TelemetryError> {
    Ok(PrometheusBuilder::new()
        .set_buckets_for_metric(
            Matcher::Full(http_metrics::REQUEST_DURATION.to_owned()),
            &HTTP_DURATION_BUCKETS,
        )?
        .build_recorder())
}

/// Ставит глобальный регистратор и описывает метрики, которые обновляет [`render`].
pub(super) fn install(recorder: PrometheusRecorder) -> Result<(), TelemetryError> {
    metrics::set_global_recorder(recorder)?;

    metrics::describe_gauge!(
        "db_pool_connections",
        Unit::Count,
        "PostgreSQL pool connections by state (idle, active)."
    );
    metrics::describe_gauge!(
        "jemalloc_allocated_bytes",
        Unit::Bytes,
        "Bytes allocated by the application (jemalloc stats.allocated)."
    );
    metrics::describe_gauge!(
        "jemalloc_resident_bytes",
        Unit::Bytes,
        "Bytes in physically resident pages mapped by jemalloc (stats.resident)."
    );
    metrics_process::Collector::default().describe();
    Ok(())
}

/// Запускает поток, который периодически вызывает `run_upkeep`.
///
/// Без встроенного HTTP-сервера регистратор сам себя не обслуживает: при
/// редком скрейпе данные гистограмм копятся в памяти (раздел «Upkeep and
/// maintenance» документации metrics-exporter-prometheus). Поток завершается,
/// когда возвращённый отправитель удалён.
pub(super) fn spawn_upkeep(handle: PrometheusHandle) -> io::Result<mpsc::Sender<()>> {
    let (stop, stopped) = mpsc::channel::<()>();
    thread::Builder::new()
        .name("metrics-upkeep".to_owned())
        .spawn(move || {
            while let Err(RecvTimeoutError::Timeout) = stopped.recv_timeout(UPKEEP_INTERVAL) {
                handle.run_upkeep();
            }
        })?;
    Ok(stop)
}

/// Обновляет gauge-метрики пула, jemalloc и процесса и рендерит текст для `/metrics`.
///
/// Пул не блокируется: читаются только счётчики, соединение не запрашивается.
pub fn render(handle: &PrometheusHandle, pool: &PgPool) -> String {
    record_pool(pool);
    record_allocator();
    metrics_process::Collector::default().collect();
    handle.render()
}

fn record_pool(pool: &PgPool) {
    let size = f64::from(pool.size());
    // Число соединений далеко от предела точности f64.
    let idle = pool.num_idle() as f64;
    metrics::gauge!("db_pool_connections", "state" => "idle").set(idle);
    // size и num_idle читаются не одним снимком: разность может уйти в минус.
    metrics::gauge!("db_pool_connections", "state" => "active").set((size - idle).max(0.0));
}

/// Статистика jemalloc: `(allocated, resident)` в байтах.
#[cfg(not(target_env = "msvc"))]
pub(crate) fn allocator_stats() -> Result<(usize, usize), tikv_jemalloc_ctl::Error> {
    use tikv_jemalloc_ctl::{epoch, stats};

    // jemalloc отдаёт кэшированный снимок статистики; epoch::advance его обновляет.
    epoch::advance()?;
    Ok((stats::allocated::read()?, stats::resident::read()?))
}

#[cfg(not(target_env = "msvc"))]
fn record_allocator() {
    match allocator_stats() {
        Ok((allocated, resident)) => {
            metrics::gauge!("jemalloc_allocated_bytes").set(allocated as f64);
            metrics::gauge!("jemalloc_resident_bytes").set(resident as f64);
        }
        Err(err) => tracing::warn!(error = %err, "jemalloc stats are unavailable"),
    }
}

/// На MSVC jemalloc не используется (см. `lib.rs`).
#[cfg(target_env = "msvc")]
fn record_allocator() {}

#[cfg(test)]
mod tests {
    use std::sync::OnceLock;

    use metrics_exporter_prometheus::PrometheusHandle;
    use sqlx::postgres::PgPoolOptions;

    use super::{build_recorder, install, render};

    /// Глобальный регистратор ставится один раз на весь процесс тестов.
    fn handle() -> &'static PrometheusHandle {
        static HANDLE: OnceLock<PrometheusHandle> = OnceLock::new();
        HANDLE.get_or_init(|| {
            let recorder = build_recorder().unwrap();
            let handle = recorder.handle();
            install(recorder).unwrap();
            handle
        })
    }

    #[tokio::test]
    async fn render_includes_pool_and_allocator_gauges() {
        // connect_lazy не открывает соединений: адрес недостижим, но пул создаётся.
        let pool = PgPoolOptions::new()
            .connect_lazy("postgres://postgres@127.0.0.1:1/postgres")
            .unwrap();

        let output = render(handle(), &pool);

        assert!(output.contains("db_pool_connections"), "{output}");
        assert!(
            output.contains(r#"db_pool_connections{state="idle"} 0"#),
            "{output}"
        );
        assert!(
            output.contains(r#"db_pool_connections{state="active"} 0"#),
            "{output}"
        );
        assert!(output.contains("jemalloc_allocated_bytes"), "{output}");
        assert!(output.contains("jemalloc_resident_bytes"), "{output}");
        assert!(output.contains("process_resident_memory_bytes"), "{output}");
    }

    #[test]
    fn request_duration_is_rendered_as_histogram() {
        let handle = handle();
        metrics::histogram!("http_request_duration_seconds").record(0.02);

        let output = handle.render();

        // Гистограмма агрегируется по репликам; summary (квантили) — нет.
        assert!(
            output.contains("http_request_duration_seconds_bucket{"),
            "{output}"
        );
    }
}
