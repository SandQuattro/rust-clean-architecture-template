//! HTTP-метрики и access-лог (middleware `axum::middleware::from_fn`).

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Instant;

use axum::extract::{ConnectInfo, MatchedPath, Request};
use axum::http::{Method, StatusCode, Uri};
use axum::middleware::Next;
use axum::response::Response;
use metrics::{SharedString, Unit};

const REQUESTS_TOTAL: &str = "http_requests_total";
/// Гистограмма длительности; границы задаёт `telemetry::metrics::build_recorder`.
pub(crate) const REQUEST_DURATION: &str = "http_request_duration_seconds";
const IN_FLIGHT: &str = "http_requests_in_flight";
/// Счётчик отказов load shed; увеличивает обработчик ошибок слоёв API в `router`.
pub(crate) const SHED_TOTAL: &str = "http_requests_shed_total";

/// Метка `path` запроса, не совпавшего ни с одним маршрутом.
const UNMATCHED: &str = "unmatched";

/// Описания HTTP-метрик для `/metrics`.
pub(crate) fn describe() {
    metrics::describe_counter!(
        REQUESTS_TOTAL,
        Unit::Count,
        "HTTP requests by method, route template and status."
    );
    metrics::describe_histogram!(
        REQUEST_DURATION,
        Unit::Seconds,
        "HTTP request latency by method, route template and status."
    );
    metrics::describe_gauge!(IN_FLIGHT, Unit::Count, "HTTP requests being processed.");
    metrics::describe_counter!(
        SHED_TOTAL,
        Unit::Count,
        "API requests rejected with 503 because the concurrency limit was reached."
    );
}

/// Считает запрос в метриках и пишет access-лог.
///
/// Метка `path` — шаблон маршрута (`MatchedPath`), а не сырой путь: число рядов
/// метрики не зависит от идентификаторов в URL. Access-лог дополнительно содержит путь
/// с query как получен (`uri`) и адрес клиента (`ip`, если сервер передал `ConnectInfo`).
pub(crate) async fn track(req: Request, next: Next) -> Response {
    let path = req
        .extensions()
        .get::<MatchedPath>()
        .map_or(UNMATCHED, MatchedPath::as_str);
    let ip = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(peer)| peer.ip());
    // Клон `Uri` разделяет буфер запроса, строка не копируется.
    let request = Tracked::start(method_label(req.method()), path, req.uri().clone(), ip);

    let resp = next.run(req).await;

    request.record(resp.status());
    resp
}

/// Статическая метка метода. Нестандартные методы сводятся к `OTHER`, чтобы клиент
/// не мог раздуть число рядов метрики.
fn method_label(method: &Method) -> &'static str {
    match *method {
        Method::GET => "GET",
        Method::POST => "POST",
        Method::PUT => "PUT",
        Method::DELETE => "DELETE",
        Method::PATCH => "PATCH",
        Method::HEAD => "HEAD",
        Method::OPTIONS => "OPTIONS",
        Method::CONNECT => "CONNECT",
        Method::TRACE => "TRACE",
        _ => "OTHER",
    }
}

/// Учёт одного запроса.
///
/// `Drop` уменьшает `http_requests_in_flight` и тогда, когда future запроса отброшен
/// (клиент ушёл). При раскрутке паникой `Drop` также записывает запрос со статусом 500:
/// ответ на панику строит внешний слой перехвата паники, и обычная запись в [`track`]
/// не выполняется.
struct Tracked {
    method: &'static str,
    /// `Arc<str>` внутри: клон для каждой метки не копирует строку.
    path: SharedString,
    uri: Uri,
    ip: Option<IpAddr>,
    started: Instant,
}

impl Tracked {
    fn start(method: &'static str, path: &str, uri: Uri, ip: Option<IpAddr>) -> Self {
        metrics::gauge!(IN_FLIGHT).increment(1.0);
        Self {
            method,
            path: SharedString::from(Arc::<str>::from(path)),
            uri,
            ip,
            started: Instant::now(),
        }
    }

    /// Пишет access-лог и метрики запроса с итоговым статусом.
    fn record(&self, status: StatusCode) {
        let elapsed = self.started.elapsed();
        // Сообщение `access` — как в access-логе Go-шаблона: конвейеры логов фильтруют по нему.
        tracing::info!(
            target: "access",
            method = self.method,
            path = &*self.path,
            uri = self.uri.path_and_query().map(tracing::field::display),
            ip = self.ip.map(tracing::field::display),
            status = status.as_u16(),
            latency_ms = elapsed.as_secs_f64() * 1e3,
            "access"
        );
        let status = SharedString::from(Arc::<str>::from(status.as_str()));
        metrics::counter!(REQUESTS_TOTAL, "method" => self.method, "path" => self.path.clone(), "status" => status.clone())
            .increment(1);
        metrics::histogram!(REQUEST_DURATION, "method" => self.method, "path" => self.path.clone(), "status" => status)
            .record(elapsed);
    }
}

impl Drop for Tracked {
    fn drop(&mut self) {
        metrics::gauge!(IN_FLIGHT).decrement(1.0);
        if std::thread::panicking() {
            self.record(StatusCode::INTERNAL_SERVER_ERROR);
        }
    }
}
