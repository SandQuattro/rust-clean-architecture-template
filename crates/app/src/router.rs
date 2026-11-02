//! Сборка HTTP-роутера: API под защитой от перегрузки, пробы, `/metrics` и dev-маршруты.
//!
//! Слои (снаружи внутрь):
//! 1. перехват паники → 500 problem+json;
//! 2. серверный OTel-спан с W3C `traceparent`;
//! 3. HTTP-метрики и access-лог — снаружи от load shed, чтобы отказы 503 были видны;
//! 4. перевод ошибок слоёв 5–8 в problem+json;
//! 5. load shed: лимит занят → немедленный 503, без очереди;
//! 6. общий для всего API лимит одновременных запросов;
//! 7. тайм-аут обработки запроса;
//! 8. тайм-ауты тела запроса и ответа.
//!
//! Слои 4–8 действуют только на API (включая `/openapi.json` и `/docs`); пробы,
//! `/metrics` и dev-маршруты подключаются после них и под лимит не попадают.

use std::any::Any;
use std::time::Duration;

use adapter_http::problem::Problem;
use adapter_http::usecase::UserUseCase;
use axum::error_handling::HandleErrorLayer;
use axum::extract::State;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{BoxError, Router, middleware};
use axum_tracing_opentelemetry::middleware::OtelAxumLayer;
use metrics_exporter_prometheus::PrometheusHandle;
use sqlx::PgPool;
use tower::ServiceBuilder;
use tower::limit::GlobalConcurrencyLimitLayer;
use tower::load_shed::LoadShedLayer;
use tower::load_shed::error::Overloaded;
use tower::timeout::TimeoutLayer;
use tower::timeout::error::Elapsed;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::timeout::{RequestBodyDeadlineLayer, ResponseBodyTimeoutLayer};

use crate::{http_metrics, probes, profiling, telemetry};

/// Параметры роутера; `app` заполняет их из конфигурации.
#[derive(Debug, Clone)]
pub struct RouterSettings {
    /// Предел одновременно обрабатываемых API-запросов (`HTTP_MAX_CONCURRENCY`).
    pub max_concurrency: usize,
    /// Тайм-аут обработки API-запроса до готовности ответа (`HTTP_REQUEST_TIMEOUT`).
    pub request_timeout: Duration,
    /// Предел на чтение всего тела запроса (`HTTP_READ_TIMEOUT`).
    pub read_timeout: Duration,
    /// Предел паузы между фрагментами тела ответа (`HTTP_WRITE_TIMEOUT`).
    pub write_timeout: Duration,
    /// Подключить dev-маршруты (`/debug/pprof/heap`).
    pub dev_routes: bool,
}

/// Строит роутер сервиса. `pool` — пул API (его состояние попадает в `/metrics`),
/// `ready_pool` — пул проверки готовности для `/readyz` (`database::readiness_pool`).
pub fn build_router<U: UserUseCase>(
    usecase: U,
    pool: PgPool,
    ready_pool: PgPool,
    metrics: PrometheusHandle,
    settings: &RouterSettings,
) -> Router {
    http_metrics::describe();

    let (api, _) = adapter_http::routes::build(usecase);
    // `Router::layer` оборачивает каждый маршрут отдельно. `GlobalConcurrencyLimitLayer`
    // раздаёт всем маршрутам один `Arc<Semaphore>`, поэтому лимит общий для API;
    // `ConcurrencyLimitLayer` создал бы свой семафор на каждый маршрут.
    let api = api.layer(
        ServiceBuilder::new()
            .layer(HandleErrorLayer::new(handle_middleware_error))
            .layer(LoadShedLayer::new())
            .layer(GlobalConcurrencyLimitLayer::new(settings.max_concurrency))
            .layer(TimeoutLayer::new(settings.request_timeout))
            .layer(RequestBodyDeadlineLayer::new(settings.read_timeout))
            .layer(ResponseBodyTimeoutLayer::new(settings.write_timeout)),
    );

    // Состояние передаётся через `with_state` отдельных маршрутов: API-запросы не
    // платят за расширения роутера.
    let mut service = Router::new()
        .route("/", get(probes::root))
        .route("/livez", get(probes::livez))
        .route("/readyz", get(probes::readyz).with_state(ready_pool))
        .route("/metrics", get(render_metrics).with_state((metrics, pool)));
    if settings.dev_routes {
        service = service.route("/debug/pprof/heap", get(profiling::heap_profile));
    }

    api.merge(service).layer(
        ServiceBuilder::new()
            .layer(CatchPanicLayer::custom(panic_response))
            .layer(OtelAxumLayer::default())
            .layer(middleware::from_fn(http_metrics::track)),
    )
}

/// Ошибки слоёв 5–8 → problem+json. Тексты общие: внутренние детали клиенту не уходят.
async fn handle_middleware_error(err: BoxError) -> Problem {
    if err.is::<Overloaded>() {
        // Не логируется: при всплеске лог отказов сам стал бы нагрузкой; отказ виден
        // в access-логе и в счётчике.
        metrics::counter!(http_metrics::SHED_TOTAL).increment(1);
        Problem::unavailable()
    } else if err.is::<Elapsed>() {
        tracing::warn!("request timed out");
        Problem::unavailable()
    } else {
        tracing::error!(error = %err, "unhandled middleware error");
        Problem::internal()
    }
}

/// Паника обработчика → 500 problem+json. Текст, место и backtrace паники с `trace_id`
/// запроса уже записал хук `telemetry::install_panic_hook`; здесь они не повторяются.
fn panic_response(_payload: Box<dyn Any + Send + 'static>) -> Response {
    Problem::internal().into_response()
}

/// `/metrics` в текстовом формате Prometheus 0.0.4.
async fn render_metrics(
    State((handle, pool)): State<(PrometheusHandle, PgPool)>,
) -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        telemetry::metrics::render(&handle, &pool),
    )
}
