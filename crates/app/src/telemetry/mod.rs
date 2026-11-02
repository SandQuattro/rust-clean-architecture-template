//! Телеметрия: логи (`tracing`), трейсы (OTLP gRPC) и метрики (Prometheus).
//!
//! Аналог Go-пакетов `pkg/logger` и `pkg/tracing`. Модуль не зависит от
//! `config`: значения из конфигурации переносит вызывающий код (`main`).

mod json_format;
pub mod metrics;

use std::backtrace::Backtrace;
use std::io;
use std::panic::PanicHookInfo;
use std::sync::mpsc;
use std::time::Duration;

use metrics_exporter_prometheus::{PrometheusHandle, PrometheusRecorder};
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::{SpanExporter, WithExportConfig};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::{Sampler, SdkTracer, SdkTracerProvider};
use tracing::{Level, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;

use self::json_format::JsonWithTraceIds;

/// Доля сэмплируемых корневых трейсов (как `fraction` в Go).
const SAMPLE_RATIO: f64 = 0.6;

#[derive(Debug, thiserror::Error)]
pub enum TelemetryError {
    #[error("OTLP span exporter: {0}")]
    Exporter(#[from] opentelemetry_otlp::ExporterBuildError),
    #[error("global tracing subscriber: {0}")]
    Subscriber(#[from] tracing_subscriber::util::TryInitError),
    #[error("metrics recorder: {0}")]
    MetricsBuild(#[from] metrics_exporter_prometheus::BuildError),
    #[error("global metrics recorder: {0}")]
    MetricsRecorder(#[from] ::metrics::SetRecorderError<PrometheusRecorder>),
    #[error("metrics upkeep thread: {0}")]
    MetricsUpkeep(#[source] io::Error),
    #[error("tracer provider shutdown: {0}")]
    Shutdown(#[from] opentelemetry_sdk::error::OTelSdkError),
}

/// Параметры телеметрии; `main` заполняет их из конфигурации.
#[derive(Debug, Clone, Copy)]
pub struct TelemetrySettings<'a> {
    /// `service.name` всех спанов (`APP_NAME`).
    pub service_name: &'a str,
    /// Адрес OTLP gRPC-коллектора (`TRACING_URL`): `host:port` или URL со схемой.
    pub otlp_endpoint: &'a str,
    /// Минимальный уровень логов (`LOG_LEVEL`).
    pub level: Level,
    /// `DEBUG=true`: уровень debug независимо от `level`.
    pub debug: bool,
    /// JSON-логи (prod), иначе текст.
    pub json: bool,
}

/// Запущенная телеметрия: провайдер трейсов и регистратор метрик.
pub struct Telemetry {
    provider: SdkTracerProvider,
    metrics: PrometheusHandle,
    /// Удаление отправителя останавливает поток обслуживания метрик.
    _metrics_upkeep: mpsc::Sender<()>,
}

impl Telemetry {
    /// Handle регистратора для рендера `/metrics`.
    pub fn metrics(&self) -> PrometheusHandle {
        self.metrics.clone()
    }

    /// Отправляет накопленные спаны и останавливает провайдер трейсов.
    ///
    /// Блокирует поток не дольше `timeout`. Экспорт идёт через задачу tonic в
    /// рантайме tokio, поэтому вызывать после остановки HTTP-сервера, пока
    /// рантайм жив, и не из однопоточного рантайма.
    pub fn shutdown(self, timeout: Duration) -> Result<(), TelemetryError> {
        self.provider.shutdown_with_timeout(timeout)?;
        Ok(())
    }
}

/// Ставит глобальные subscriber `tracing` (fmt- и OTel-слои), хук паники,
/// пропагатор W3C TraceContext и регистратор метрик Prometheus.
///
/// Вызывается один раз внутри рантайма tokio (см. [`tracer_provider`]).
pub fn init(settings: &TelemetrySettings<'_>) -> Result<Telemetry, TelemetryError> {
    let provider = tracer_provider(settings.otlp_endpoint, settings.service_name)?;
    let tracer = provider.tracer(settings.service_name.to_owned());

    subscriber(settings, io::stdout, tracer).try_init()?;
    if settings.otlp_endpoint.is_empty() {
        tracing::info!("OTLP endpoint is empty, tracing export is disabled");
    }
    install_panic_hook();
    opentelemetry::global::set_text_map_propagator(TraceContextPropagator::new());
    opentelemetry::global::set_tracer_provider(provider.clone());

    let recorder = metrics::build_recorder()?;
    let handle = recorder.handle();
    metrics::install(recorder)?;
    let upkeep = metrics::spawn_upkeep(handle.clone()).map_err(TelemetryError::MetricsUpkeep)?;

    Ok(Telemetry {
        provider,
        metrics: handle,
        _metrics_upkeep: upkeep,
    })
}

/// Subscriber `tracing`: fmt-слой пишет логи в `writer`, OTel-слой передаёт спаны `tracer`.
///
/// У слоёв свои фильтры уровня. Общий фильтр отключал бы и спаны: при `LOG_LEVEL=WARN`
/// серверный спан и спаны обработчиков (уровень INFO) не создавались бы, а с ними пропали бы
/// трейсы, продолжение входящего `traceparent` и `trace_id` в оставшихся логах.
fn subscriber<W>(
    settings: &TelemetrySettings<'_>,
    writer: W,
    tracer: SdkTracer,
) -> impl Subscriber + Send + Sync + 'static
where
    W: for<'w> MakeWriter<'w> + Send + Sync + 'static,
{
    tracing_subscriber::registry()
        .with(
            fmt_layer(settings.json, writer)
                .with_filter(level_filter(settings.level, settings.debug)),
        )
        .with(
            tracing_opentelemetry::layer()
                .with_tracer(tracer)
                .with_filter(span_filter(settings.debug)),
        )
}

/// Заменяет стандартный хук паники (печать в stderr) записью `error!` в общий
/// поток логов: текст паники, место и backtrace.
///
/// Хук выполняется в потоке паники до раскрутки стека, пока спан запроса ещё
/// активен, поэтому запись содержит `trace_id` и `span_id` запроса.
pub fn install_panic_hook() {
    std::panic::set_hook(Box::new(log_panic));
}

fn log_panic(info: &PanicHookInfo<'_>) {
    tracing::error!(
        panic = info.payload_as_str().unwrap_or("non-string panic payload"),
        location = info.location().map(tracing::field::display),
        backtrace = %Backtrace::force_capture(),
        "panicked"
    );
}

/// fmt-слой: JSON (ключ сообщения `message`, поля `trace_id`/`span_id`
/// текущего OTel-спана) или человекочитаемый текст.
pub fn fmt_layer<S, W>(json: bool, writer: W) -> impl Layer<S>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    W: for<'w> MakeWriter<'w> + Send + Sync + 'static,
{
    let layer = tracing_subscriber::fmt::layer().with_writer(writer);
    if json {
        layer
            .json()
            .event_format(JsonWithTraceIds::default())
            .boxed()
    } else {
        layer.boxed()
    }
}

/// Цели зависимостей, чьи внутренние debug-события (экспорт спанов по OTLP, tonic, h2, hyper,
/// tower) заливают лог при `LOG_LEVEL=DEBUG`. Названия взяты из вывода приложения:
/// `h2::codec::*`, `h2::client`, `hyper_util::client::*`, `opentelemetry_sdk`,
/// `opentelemetry-otlp` (в этой крейте цель с дефисом), `tower::buffer::worker`.
const NOISY_TARGETS: [&str; 8] = [
    "h2",
    "hyper",
    "hyper_util",
    "tonic",
    "tower",
    "opentelemetry",
    "opentelemetry_sdk",
    "opentelemetry-otlp",
];

/// Уровень логов как в Go: `DEBUG=true` включает debug независимо от `level`.
///
/// Зависимости из [`NOISY_TARGETS`] ограничены уровнем INFO (или `level`, если он строже);
/// события `sqlx` и самого приложения идут с общим уровнем.
fn level_filter(level: Level, debug: bool) -> Targets {
    let base = if debug {
        LevelFilter::DEBUG
    } else {
        LevelFilter::from_level(level)
    };
    let cap = std::cmp::min(base, LevelFilter::INFO);
    NOISY_TARGETS
        .into_iter()
        .fold(Targets::new().with_default(base), |targets, target| {
            targets.with_target(target, cap)
        })
}

/// Уровень спанов для OTel-слоя не зависит от `LOG_LEVEL`: INFO (серверный спан и спаны
/// обработчиков), при `DEBUG=true` — debug. Спаны `aide` (построение маршрутов при старте)
/// исключены: каждый становился отдельным однотрейсовым трейсом.
fn span_filter(debug: bool) -> Targets {
    let level = if debug {
        LevelFilter::DEBUG
    } else {
        LevelFilter::INFO
    };
    Targets::new()
        .with_default(level)
        .with_target("aide", LevelFilter::OFF)
}

/// Провайдер трейсов с пакетным экспортом по OTLP gRPC (как
/// `InitOpenTelemetryGRPC` в Go): сэмплер `ParentBased(TraceIdRatioBased(0.6))`,
/// ресурс `service.name`.
///
/// Соединение с коллектором ленивое: недоступный коллектор не мешает старту,
/// ошибки экспорта SDK пишет в лог. Пустой `endpoint` выключает экспорт: провайдер
/// без экспортёра, спаны и `trace_id` в логах остаются. Вызывать внутри рантайма tokio:
/// канал tonic запускает фоновую задачу.
pub fn tracer_provider(
    endpoint: &str,
    service_name: &str,
) -> Result<SdkTracerProvider, TelemetryError> {
    let resource = Resource::builder_empty()
        .with_service_name(service_name.to_owned())
        .build();
    let provider = SdkTracerProvider::builder()
        .with_sampler(Sampler::ParentBased(Box::new(Sampler::TraceIdRatioBased(
            SAMPLE_RATIO,
        ))))
        .with_resource(resource);
    if endpoint.is_empty() {
        return Ok(provider.build());
    }

    let exporter = SpanExporter::builder()
        .with_tonic()
        .with_endpoint(grpc_endpoint(endpoint))
        .build()?;
    Ok(provider.with_batch_exporter(exporter).build())
}

/// В Go адрес задаётся как `host:port` с `WithInsecure`. Без схемы
/// opentelemetry-otlp подставил бы `https://`, поэтому добавляем `http://`.
fn grpc_endpoint(endpoint: &str) -> String {
    if endpoint.contains("://") {
        endpoint.to_owned()
    } else {
        format!("http://{endpoint}")
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use opentelemetry::trace::{Span as _, Tracer as _, TracerProvider as _};

    use super::tracer_provider;

    /// Пустой адрес (`TRACING_URL=""`) выключает экспорт, а не старт: спаны создаются с
    /// валидным контекстом, поэтому `trace_id` в логах и `traceparent` продолжают работать.
    #[tokio::test]
    async fn empty_endpoint_disables_export_only() {
        let provider = tracer_provider("", "test-service").unwrap();

        let span = provider.tracer("test").start("request");
        assert!(span.span_context().is_valid());
        drop(span);
        provider
            .shutdown_with_timeout(Duration::from_secs(1))
            .unwrap();
    }

    #[tokio::test]
    async fn tracer_provider_starts_without_collector() {
        // Адрес в формате Go (`host:port`), коллектора по нему нет.
        let provider = tracer_provider("127.0.0.1:1", "test-service").unwrap();

        provider
            .shutdown_with_timeout(Duration::from_secs(1))
            .unwrap();
    }
}
