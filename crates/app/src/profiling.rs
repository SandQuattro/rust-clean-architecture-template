//! Профиль кучи jemalloc в формате pprof (`/debug/pprof/heap`, только dev-маршруты).
//!
//! Работает только в Linux. Профилирование включается при запуске переменной
//! `_RJEM_MALLOC_CONF=prof:true,prof_active:true,lg_prof_sample:19` (символы jemalloc
//! с префиксом, поэтому не `MALLOC_CONF`). Просмотр: `go tool pprof -http=:8080 heap.pb.gz`.

use adapter_http::problem::Problem;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

#[cfg(target_os = "linux")]
pub(crate) async fn heap_profile() -> Response {
    use axum::http::header;

    let Some(ctl) = jemalloc_pprof::PROF_CTL.as_ref() else {
        return inactive();
    };
    let mut ctl = ctl.lock().await;
    if !ctl.activated() {
        return inactive();
    }
    match ctl.dump_pprof() {
        Ok(pprof) => ([(header::CONTENT_TYPE, "application/octet-stream")], pprof).into_response(),
        Err(err) => {
            tracing::error!(error = %format_args!("{err:#}"), "heap profile dump failed");
            Problem::internal().into_response()
        }
    }
}

#[cfg(target_os = "linux")]
fn inactive() -> Response {
    Problem::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "jemalloc heap profiling is not active: start with \
         _RJEM_MALLOC_CONF=prof:true,prof_active:true",
    )
    .into_response()
}

#[cfg(not(target_os = "linux"))]
pub(crate) async fn heap_profile() -> Response {
    Problem::new(
        StatusCode::NOT_IMPLEMENTED,
        "heap profiling is available on Linux only",
    )
    .into_response()
}
