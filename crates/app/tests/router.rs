//! Тесты роутера: перегрузка, тайм-аут, паника, пробы, метрики, dev-маршруты.
//!
//! Роутер обслуживается на `127.0.0.1:0` через `axum::serve`, запросы идут через
//! reqwest. Большинству тестов БД не нужна: пул ленивый и указывает на закрытый порт.
//! Тесты с `#[sqlx::test]` требуют `DATABASE_URL`.

// `allow-*-in-tests` из clippy.toml покрывает только функции `#[test]`, а не
// вспомогательный код тестового крейта; паника `TestUseCase` — часть сценария.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test-only helpers"
)]

use std::io;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use adapter_http::usecase::UserUseCase;
use adapter_postgres::PgUserRepository;
use app::database::readiness_pool;
use app::router::{RouterSettings, build_router};
use axum::body::Body;
use domain::command::{
    CreateUser, DeleteUser, FindAllUsers, FindUserById, TransferMoney, UpdateUser,
};
use domain::entity::{User, UserOrders};
use domain::error::{Error, Result};
use domain::service::UserService;
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use reqwest::StatusCode;
use reqwest::header::CONTENT_TYPE;
use serde_json::Value;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tower::ServiceExt;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;

/// Предел ожидания событий синхронизации: сломанная реализация роняет тест, а не вешает его.
const WAIT: Duration = Duration::from_secs(5);

/// Поведение `find_all_users`.
#[derive(Clone, Copy)]
enum Mode {
    /// Сообщает о входе через `entered` и ждёт `release`.
    Block,
    /// Паникует.
    Panic,
}

struct TestUseCase {
    mode: Mode,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

fn unused<T>() -> Result<T> {
    Err(Error::internal("unused", "not used by router tests"))
}

impl UserUseCase for TestUseCase {
    async fn find_all_users(&self, _cmd: FindAllUsers) -> Result<Vec<User>> {
        match self.mode {
            Mode::Block => {
                self.entered.notify_one();
                self.release.notified().await;
                Ok(Vec::new())
            }
            Mode::Panic => panic!("test use case panicked"),
        }
    }

    async fn find_all_users_with_orders(&self, _cmd: FindAllUsers) -> Result<Vec<UserOrders>> {
        unused()
    }

    async fn find_user_by_id(&self, _cmd: FindUserById) -> Result<User> {
        unused()
    }

    async fn create_user(&self, _cmd: CreateUser) -> Result<User> {
        unused()
    }

    async fn update_user(&self, _cmd: UpdateUser) -> Result<User> {
        unused()
    }

    async fn delete_user(&self, _cmd: DeleteUser) -> Result<()> {
        unused()
    }

    async fn transfer_money(&self, _cmd: TransferMoney) -> Result<()> {
        unused()
    }
}

/// Глобальный регистратор метрик ставится один раз на весь процесс тестов.
fn metrics_handle() -> PrometheusHandle {
    static HANDLE: OnceLock<PrometheusHandle> = OnceLock::new();
    HANDLE
        .get_or_init(|| PrometheusBuilder::new().install_recorder().unwrap())
        .clone()
}

fn settings() -> RouterSettings {
    RouterSettings {
        max_concurrency: 64,
        request_timeout: Duration::from_secs(10),
        read_timeout: Duration::from_secs(10),
        write_timeout: Duration::from_secs(10),
        dev_routes: false,
    }
}

struct Server {
    base: String,
    client: reqwest::Client,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

impl Server {
    async fn start(mode: Mode, settings: RouterSettings) -> Self {
        let pool = unreachable_pool();
        let ready_pool = readiness_pool(&pool, Duration::from_secs(1));
        Self::start_with_pools(mode, pool, ready_pool, settings).await
    }

    /// `pool` — пул API, `ready_pool` — пул `/readyz`.
    async fn start_with_pools(
        mode: Mode,
        pool: PgPool,
        ready_pool: PgPool,
        settings: RouterSettings,
    ) -> Self {
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let usecase = TestUseCase {
            mode,
            entered: entered.clone(),
            release: release.clone(),
        };
        let router = build_router(usecase, pool, ready_pool, metrics_handle(), &settings);
        Self {
            base: spawn_router(router).await,
            client: reqwest::Client::new(),
            entered,
            release,
        }
    }

    async fn get(&self, path: &str) -> reqwest::Response {
        self.client
            .get(format!("{}{path}", self.base))
            .send()
            .await
            .unwrap()
    }

    /// Отправляет запрос, который блокируется в use case, и ждёт его входа в use case:
    /// после возврата единственное место лимита гарантированно занято.
    async fn occupy(&self, path: &str) -> JoinHandle<reqwest::Response> {
        let request = self.client.get(format!("{}{path}", self.base)).send();
        let first = tokio::spawn(async move { request.await.unwrap() });
        tokio::time::timeout(WAIT, self.entered.notified())
            .await
            .expect("first request must enter the use case");
        first
    }

    /// Отпускает заблокированный запрос и проверяет, что он завершился успешно.
    async fn finish(&self, first: JoinHandle<reqwest::Response>) {
        self.release.notify_one();
        let resp = tokio::time::timeout(WAIT, first)
            .await
            .expect("first request must return after release")
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}

/// Общий буфер, в который пишет fmt-слой.
#[derive(Clone, Default)]
struct Buffer(Arc<Mutex<Vec<u8>>>);

impl io::Write for Buffer {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Buffer {
    type Writer = Buffer;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Ленивый пул к закрытому порту: соединений не открывает, acquire быстро получает ошибку.
fn unreachable_pool() -> PgPool {
    PgPoolOptions::new()
        .acquire_timeout(Duration::from_millis(100))
        .connect_lazy("postgres://postgres@127.0.0.1:1/postgres")
        .unwrap()
}

/// Буфер логов всего тестового процесса: глобальный subscriber ставится один раз.
///
/// Локальный subscriber (`set_default`) здесь ненадёжен: пока зарегистрирован один
/// dispatcher, tracing-core вычисляет интерес callsite по subscriber того потока, который
/// первым выполнил макрос (`Rebuilder::JustOne`), и параллельный тест без subscriber
/// навсегда отключает access-лог.
fn log_buffer() -> Buffer {
    static BUFFER: OnceLock<Buffer> = OnceLock::new();
    BUFFER
        .get_or_init(|| {
            let buffer = Buffer::default();
            let subscriber = tracing_subscriber::registry()
                .with(LevelFilter::INFO)
                .with(app::telemetry::fmt_layer(true, buffer.clone()));
            tracing::subscriber::set_global_default(subscriber).unwrap();
            buffer
        })
        .clone()
}

/// Обслуживает `router` на свободном порту и возвращает базовый URL.
async fn spawn_router(router: axum::Router) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await });
    format!("http://{addr}")
}

async fn assert_problem(resp: reqwest::Response, status: StatusCode, detail: &str) {
    assert_eq!(resp.status(), status);
    assert_eq!(resp.headers()[CONTENT_TYPE], "application/problem+json");
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["status"], status.as_u16());
    assert_eq!(body["detail"], detail);
}

#[tokio::test]
async fn overload_sheds_with_503() {
    let server = Server::start(
        Mode::Block,
        RouterSettings {
            max_concurrency: 1,
            ..settings()
        },
    )
    .await;
    let first = server.occupy("/users/1/10").await;

    // Другой маршрут: лимит общий для всего API, а не для одного маршрута.
    let started = Instant::now();
    let resp = server.get("/users-with-orders/1/10").await;
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_millis(200),
        "shed took {elapsed:?}"
    );
    assert_problem(resp, StatusCode::SERVICE_UNAVAILABLE, "service unavailable").await;
    server.finish(first).await;
}

#[tokio::test]
async fn probes_are_not_shed() {
    let server = Server::start(
        Mode::Block,
        RouterSettings {
            max_concurrency: 1,
            ..settings()
        },
    )
    .await;
    let first = server.occupy("/users/1/10").await;

    assert_eq!(server.get("/livez").await.status(), StatusCode::OK);
    assert_eq!(server.get("/metrics").await.status(), StatusCode::OK);
    server.finish(first).await;
}

#[tokio::test]
async fn request_timeout_returns_503() {
    let server = Server::start(
        Mode::Block,
        RouterSettings {
            request_timeout: Duration::from_millis(50),
            ..settings()
        },
    )
    .await;

    // Use case ждёт `release`, которого не будет.
    let started = Instant::now();
    let resp = server.get("/users/1/10").await;
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_millis(500),
        "timeout took {elapsed:?}"
    );
    assert_problem(resp, StatusCode::SERVICE_UNAVAILABLE, "service unavailable").await;
}

#[tokio::test]
async fn panic_returns_500_problem() {
    let server = Server::start(Mode::Panic, settings()).await;

    let resp = server.get("/users/1/10").await;

    assert_problem(
        resp,
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal server error",
    )
    .await;
    assert_eq!(server.get("/livez").await.status(), StatusCode::OK);

    // Паника учтена в метриках как 500, хотя ответ строит внешний слой перехвата паники.
    // Другие тесты не дают 500 на этом маршруте, поэтому строка появляется только отсюда.
    let body = server.get("/metrics").await.text().await.unwrap();
    assert!(
        body.lines()
            .any(|line| line.starts_with("http_requests_total{")
                && line.contains(r#"path="/users/{page}/{size}""#)
                && line.contains(r#"status="500""#)),
        "{body}"
    );
}

#[tokio::test]
async fn readyz_returns_503_when_db_unreachable() {
    let server = Server::start(Mode::Block, settings()).await;

    let resp = server.get("/readyz").await;

    assert_problem(resp, StatusCode::SERVICE_UNAVAILABLE, "service unavailable").await;
}

/// Все соединения пула API заняты, а `/readyz` отвечает 200: проба не ждёт в очереди
/// пула API, поэтому всплеск нагрузки не выводит реплику из балансировки.
#[sqlx::test(migrations = false)]
async fn readyz_ignores_saturated_api_pool(db: PgPool) {
    let api = PgPoolOptions::new()
        .max_connections(2)
        .acquire_timeout(Duration::from_millis(100))
        .connect_with((*db.connect_options()).clone())
        .await
        .unwrap();
    let ready = readiness_pool(&api, Duration::from_secs(1));
    let held = [api.acquire().await.unwrap(), api.acquire().await.unwrap()];
    assert!(api.acquire().await.is_err(), "API pool must be saturated");
    let server =
        Server::start_with_pools(Mode::Block, api.clone(), ready.clone(), settings()).await;

    let resp = server.get("/readyz").await;

    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "OK");
    // Соединения закрываются до удаления тестовой БД: иначе DROP DATABASE ждёт их 5 s.
    drop(held);
    api.close().await;
    ready.close().await;
}

/// Access-лог в форме Go: сообщение `access`, шаблон маршрута в `path`, путь с query как
/// получен в `uri`. Без `ConnectInfo` (здесь роутер обслуживает `axum::serve`) поля `ip`
/// нет, и запрос не падает.
#[tokio::test]
async fn access_log_has_message_route_and_uri() {
    let buffer = log_buffer();
    let server = Server::start(Mode::Block, settings()).await;

    // Query уникален: буфер общий для всех тестов процесса.
    let resp = server.get("/user/abc?access-log-test=1").await;

    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let output = String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap();
    let entry: Value = output
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|entry| entry["target"] == "access" && entry["uri"] == "/user/abc?access-log-test=1")
        .unwrap_or_else(|| panic!("no access line in {output}"));
    assert_eq!(entry["message"], "access", "{entry}");
    assert_eq!(entry["path"], "/user/{id}", "{entry}");
    assert_eq!(entry["status"], 422, "{entry}");
    assert!(entry.get("ip").is_none(), "{entry}");
}

/// Тело больше лимита axum (2 MiB) даёт 413 и через полный стек слоёв роутера, где тело
/// обёрнуто слоем дедлайна чтения (`RequestBodyDeadlineLayer`).
#[tokio::test]
async fn oversized_body_returns_413_through_router_layers() {
    let usecase = TestUseCase {
        mode: Mode::Block,
        entered: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
    };
    let pool = unreachable_pool();
    let ready_pool = readiness_pool(&pool, Duration::from_secs(1));
    let router = build_router(usecase, pool, ready_pool, metrics_handle(), &settings());
    let body = format!(r#"{{"name":"{}"}}"#, "x".repeat(3 * 1024 * 1024));
    let req = axum::http::Request::post("/user")
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .unwrap();

    let resp = router.oneshot(req).await.unwrap();

    assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(resp.headers()[CONTENT_TYPE], "application/problem+json");
}

/// Огромный номер страницы: offset насыщается, ответ — 200 и пустой список, а не паника
/// или ошибка переполнения (HTTP, роутер, use case и PostgreSQL вместе).
#[sqlx::test(migrations = "../../migrations")]
async fn huge_page_returns_empty_list(db: PgPool) {
    let ready = readiness_pool(&db, Duration::from_secs(1));
    let usecase = UserService::new(PgUserRepository::new(db.clone()));
    let router = build_router(
        usecase,
        db.clone(),
        ready.clone(),
        metrics_handle(),
        &settings(),
    );
    let base = spawn_router(router).await;

    let resp = reqwest::get(format!("{base}/users/9223372036854775807/1000"))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body, serde_json::json!({ "users": [] }));
    db.close().await;
    ready.close().await;
}

#[tokio::test]
async fn livez_and_root_return_ok() {
    let server = Server::start(Mode::Block, settings()).await;

    for path in ["/livez", "/"] {
        let resp = server.get(path).await;
        assert_eq!(resp.status(), StatusCode::OK, "{path}");
        assert_eq!(resp.text().await.unwrap(), "OK", "{path}");
    }
}

#[tokio::test]
async fn metrics_report_shed_requests() {
    let server = Server::start(
        Mode::Block,
        RouterSettings {
            max_concurrency: 1,
            ..settings()
        },
    )
    .await;
    let first = server.occupy("/users/1/10").await;
    let shed = server.get("/users/1/10").await;
    assert_eq!(shed.status(), StatusCode::SERVICE_UNAVAILABLE);

    let resp = server.get("/metrics").await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()[CONTENT_TYPE], "text/plain; version=0.0.4");
    let body = resp.text().await.unwrap();

    assert!(body.contains("http_requests_shed_total"), "{body}");
    assert!(
        body.lines()
            .any(|line| line.starts_with("http_requests_total{")
                && line.contains(r#"path="/users/{page}/{size}""#)
                && line.contains(r#"status="503""#)),
        "{body}"
    );
    server.finish(first).await;
}

#[tokio::test]
async fn dev_routes_disabled_in_prod() {
    let server = Server::start(Mode::Block, settings()).await;

    let resp = server.get("/debug/pprof/heap").await;

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn dev_routes_serve_heap_profile_endpoint() {
    let server = Server::start(
        Mode::Block,
        RouterSettings {
            dev_routes: true,
            ..settings()
        },
    )
    .await;

    let resp = server.get("/debug/pprof/heap").await;

    // В тестах профилирование jemalloc не включено: Linux отвечает 503, остальные ОС — 501.
    let expected = if cfg!(target_os = "linux") {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::NOT_IMPLEMENTED
    };
    assert_eq!(resp.status(), expected);
    assert_eq!(resp.headers()[CONTENT_TYPE], "application/problem+json");
}

#[tokio::test]
async fn docs_are_served() {
    let server = Server::start(Mode::Block, settings()).await;

    let spec = server.get("/openapi.json").await;
    assert_eq!(spec.status(), StatusCode::OK);
    assert_eq!(spec.headers()[CONTENT_TYPE], "application/json");
    let spec: Value = spec.json().await.unwrap();
    assert!(spec["openapi"].is_string(), "{spec}");

    assert_eq!(server.get("/docs").await.status(), StatusCode::OK);
}
