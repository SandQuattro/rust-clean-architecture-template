//! Пробы Kubernetes и корневой маршрут. Успешные ответы — 200 "OK", как у Fiber
//! `healthcheck` и маршрута `/` в Go-шаблоне.

use std::time::Duration;

use adapter_http::problem::Problem;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use sqlx::PgPool;

/// Предел проверки БД в `/readyz`: проба отвечает быстрее периода опроса kubelet.
pub(crate) const READY_TIMEOUT: Duration = Duration::from_secs(1);

/// `/`: 200 "OK".
pub(crate) async fn root() -> &'static str {
    "OK"
}

/// `/livez`: процесс жив и обслуживает HTTP; БД не проверяется.
pub(crate) async fn livez() -> &'static str {
    "OK"
}

/// `/readyz`: соединение из пула проверки готовности (`database::readiness_pool`)
/// отвечает на ping за [`READY_TIMEOUT`], иначе 503.
pub(crate) async fn readyz(State(pool): State<PgPool>) -> Response {
    match crate::database::ping_once(&pool, READY_TIMEOUT).await {
        Ok(()) => "OK".into_response(),
        Err(err) => {
            tracing::warn!(error = %err, timeout = ?READY_TIMEOUT, "readiness check failed");
            Problem::unavailable().into_response()
        }
    }
}
