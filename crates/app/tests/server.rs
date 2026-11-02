//! Тесты HTTP-сервера: тайм-ауты заголовков и простоя keep-alive, остановка с дедлайном.
//!
//! Сервер слушает `127.0.0.1:0`; медленные клиенты и keep-alive проверяются через сырой
//! `TcpStream`, остановка — через reqwest.

// `allow-*-in-tests` из clippy.toml покрывает только функции `#[test]`, а не
// вспомогательный код тестового крейта.
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test-only helpers")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use app::server::{ServeError, ServerTimeouts, serve};
use axum::Router;
use axum::extract::ConnectInfo;
use axum::routing::get;
use reqwest::StatusCode;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// Предел ожидания событий: сломанная реализация роняет тест, а не вешает его.
const WAIT: Duration = Duration::from_secs(5);

/// Тайм-аут, который в тесте не должен срабатывать.
const LONG: Duration = Duration::from_secs(10);

struct Server {
    addr: SocketAddr,
    shutdown: CancellationToken,
    handle: JoinHandle<Result<tokio::time::Instant, ServeError>>,
    /// Обработчик `/sleep` сообщает о входе.
    entered: Arc<Notify>,
}

impl Server {
    /// `/` отвечает "OK"; `/sleep` сообщает о входе и спит `sleep`; `/ip` возвращает адрес
    /// клиента из `ConnectInfo`.
    async fn start(timeouts: ServerTimeouts, sleep: Duration) -> Self {
        let entered = Arc::new(Notify::new());
        let notify = Arc::clone(&entered);
        let router =
            Router::new()
                .route("/", get(|| async { "OK" }))
                .route(
                    "/sleep",
                    get(move || async move {
                        notify.notify_one();
                        tokio::time::sleep(sleep).await;
                        "slept"
                    }),
                )
                .route(
                    "/ip",
                    get(|ConnectInfo(peer): ConnectInfo<SocketAddr>| async move {
                        peer.ip().to_string()
                    }),
                );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let shutdown = CancellationToken::new();
        let handle = tokio::spawn(serve(listener, router, timeouts, shutdown.clone()));
        Self {
            addr,
            shutdown,
            handle,
            entered,
        }
    }

    /// Запускает запрос к `/sleep` и ждёт входа в обработчик.
    async fn sleeping_request(&self) -> JoinHandle<reqwest::Result<reqwest::Response>> {
        let request = reqwest::Client::new()
            .get(format!("http://{}/sleep", self.addr))
            .send();
        let client = tokio::spawn(request);
        tokio::time::timeout(WAIT, self.entered.notified())
            .await
            .expect("request must enter the handler");
        client
    }

    async fn stopped(self) -> Result<tokio::time::Instant, ServeError> {
        tokio::time::timeout(WAIT, self.handle)
            .await
            .expect("serve must return")
            .unwrap()
    }
}

/// Отправляет `GET /` с keep-alive и читает ответ "OK" целиком; соединение остаётся открытым.
async fn get_keepalive(stream: &mut TcpStream) {
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .unwrap();
    // Ответ целиком: заголовки и тело "OK" (content-length: 2).
    let mut response = Vec::new();
    while !response.ends_with(b"\r\n\r\nOK") {
        let mut buf = [0_u8; 1024];
        let n = tokio::time::timeout(WAIT, stream.read(&mut buf))
            .await
            .expect("response must arrive")
            .unwrap();
        assert_ne!(n, 0, "connection closed before the response");
        response.extend_from_slice(&buf[..n]);
    }
    let response = String::from_utf8_lossy(&response).to_lowercase();
    assert!(response.starts_with("http/1.1 200"), "{response}");
    assert!(!response.contains("connection: close"), "{response}");
}

/// Отправляет незавершённый запрос и ждёт, что сервер закроет соединение через
/// тайм-аут заголовков (200 ms в тестах), считая от отправки.
async fn send_partial_head_expect_cut(stream: &mut TcpStream) {
    let started = Instant::now();
    // Строка запроса без заголовков и пустой строки: запрос не завершён.
    stream.write_all(b"GET / HTTP/1.1\r\n").await.unwrap();
    read_eof(stream).await;
    let elapsed = started.elapsed();

    assert!(elapsed < Duration::from_secs(1), "closed after {elapsed:?}");
    assert!(
        elapsed >= Duration::from_millis(150),
        "closed too early: {elapsed:?}"
    );
}

/// Ждёт закрытия соединения сервером: `read` возвращает 0.
async fn read_eof(stream: &mut TcpStream) {
    let mut buf = [0_u8; 1024];
    let n = tokio::time::timeout(WAIT, stream.read(&mut buf))
        .await
        .expect("server must close the connection")
        .expect("connection must be closed cleanly");
    assert_eq!(
        n,
        0,
        "unexpected data: {:?}",
        String::from_utf8_lossy(&buf[..n])
    );
}

#[tokio::test]
async fn slow_headers_are_cut() {
    let server = Server::start(
        ServerTimeouts {
            header_read: Duration::from_millis(200),
            idle: LONG,
            shutdown: LONG,
        },
        Duration::ZERO,
    )
    .await;
    let mut stream = TcpStream::connect(server.addr).await.unwrap();

    send_partial_head_expect_cut(&mut stream).await;
}

/// Срок заголовков суммарный: байты, приходящие по одному, не продлевают его.
#[tokio::test]
async fn header_deadline_is_cumulative_under_byte_drip() {
    let server = Server::start(
        ServerTimeouts {
            header_read: Duration::from_millis(300),
            idle: LONG,
            shutdown: LONG,
        },
        Duration::ZERO,
    )
    .await;
    let started = Instant::now();
    let (mut reader, mut writer) = TcpStream::connect(server.addr).await.unwrap().into_split();
    // Строка запроса и заголовок, который растёт на байт каждые 50 ms и не завершается.
    let drip = tokio::spawn(async move {
        let mut written = writer.write_all(b"GET / HTTP/1.1\r\nX-Drip: ").await;
        while written.is_ok() {
            tokio::time::sleep(Duration::from_millis(50)).await;
            written = writer.write_all(b"x").await;
        }
    });

    let mut buf = [0_u8; 64];
    let read = tokio::time::timeout(WAIT, reader.read(&mut buf))
        .await
        .expect("server must close the connection despite the drip");
    let elapsed = started.elapsed();
    drip.abort();

    // Закрытие с непрочитанными байтами может прийти как RST, поэтому ошибка чтения допустима.
    assert!(matches!(read, Ok(0) | Err(_)), "{read:?}");
    assert!(
        elapsed >= Duration::from_millis(250) && elapsed < Duration::from_secs(1),
        "closed after {elapsed:?}"
    );
}

#[tokio::test]
async fn idle_keepalive_is_closed() {
    let server = Server::start(
        ServerTimeouts {
            header_read: LONG,
            idle: Duration::from_millis(200),
            shutdown: LONG,
        },
        Duration::ZERO,
    )
    .await;
    let mut stream = TcpStream::connect(server.addr).await.unwrap();

    get_keepalive(&mut stream).await;
    tokio::time::sleep(Duration::from_millis(600)).await;

    read_eof(&mut stream).await;
}

/// Тайм-аут заголовков не ограничивает простой keep-alive и отсчитывается заново
/// с первого байта следующего запроса.
#[tokio::test]
async fn header_timeout_restarts_after_idle_keepalive() {
    let server = Server::start(
        ServerTimeouts {
            header_read: Duration::from_millis(200),
            idle: LONG,
            shutdown: LONG,
        },
        Duration::ZERO,
    )
    .await;
    let mut stream = TcpStream::connect(server.addr).await.unwrap();

    get_keepalive(&mut stream).await;
    // Простой втрое дольше тайм-аута заголовков: соединение должно уцелеть.
    tokio::time::sleep(Duration::from_millis(600)).await;
    get_keepalive(&mut stream).await;

    send_partial_head_expect_cut(&mut stream).await;
}

/// Заголовки, не уместившиеся в буфер соединения (16 KiB), получают 431 сразу, а не копятся
/// в памяти до ~408 KiB (умолчание hyper).
#[tokio::test]
async fn oversized_head_gets_431() {
    let server = Server::start(
        ServerTimeouts {
            header_read: Duration::from_secs(1),
            idle: LONG,
            shutdown: LONG,
        },
        Duration::ZERO,
    )
    .await;
    let mut stream = TcpStream::connect(server.addr).await.unwrap();
    // Ровно 16 KiB незавершённых заголовков: сервер читает всё отправленное до ответа.
    // Непрочитанные данные при закрытии сокета превратили бы FIN в RST.
    let mut head = b"GET / HTTP/1.1\r\nHost: localhost\r\nX-Big: ".to_vec();
    head.resize(16 * 1024, b'x');
    stream.write_all(&head).await.unwrap();

    let mut response = Vec::new();
    tokio::time::timeout(WAIT, stream.read_to_end(&mut response))
        .await
        .expect("server must answer and close")
        .unwrap();

    let response = String::from_utf8_lossy(&response);
    assert!(response.starts_with("HTTP/1.1 431"), "{response:?}");
}

/// Сервер передаёт адрес клиента роутеру (`ConnectInfo`): его пишет access-лог.
#[tokio::test]
async fn connect_info_carries_peer_address() {
    let server = Server::start(
        ServerTimeouts {
            header_read: LONG,
            idle: LONG,
            shutdown: LONG,
        },
        Duration::ZERO,
    )
    .await;

    let resp = reqwest::get(format!("http://{}/ip", server.addr))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.text().await.unwrap(), "127.0.0.1");
}

#[tokio::test]
async fn shutdown_drains_inflight() {
    let server = Server::start(
        ServerTimeouts {
            header_read: LONG,
            idle: LONG,
            shutdown: WAIT,
        },
        Duration::from_millis(300),
    )
    .await;
    let client = server.sleeping_request().await;

    tokio::time::sleep(Duration::from_millis(50)).await;
    let cancelled_at = Instant::now();
    server.shutdown.cancel();

    let response = tokio::time::timeout(WAIT, client)
        .await
        .expect("client must get a response")
        .unwrap()
        .expect("in-flight request must complete");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.text().await.unwrap(), "slept");
    let deadline = server.stopped().await.expect("drain must finish in time");

    // Дедлайн остановки отсчитывается от сигнала, а не от конца дренажа (~250 ms позже).
    let budget = deadline.into_std().duration_since(cancelled_at);
    assert!(
        budget >= WAIT && budget < WAIT + Duration::from_millis(100),
        "{budget:?}"
    );
}

#[tokio::test]
async fn shutdown_closes_idle_keepalive_at_once() {
    let server = Server::start(
        ServerTimeouts {
            header_read: LONG,
            idle: LONG,
            shutdown: LONG,
        },
        Duration::ZERO,
    )
    .await;
    let mut stream = TcpStream::connect(server.addr).await.unwrap();
    get_keepalive(&mut stream).await;

    let started = Instant::now();
    server.shutdown.cancel();
    read_eof(&mut stream).await;
    server
        .stopped()
        .await
        .expect("idle connection must not delay shutdown");

    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(1),
        "stopped after {elapsed:?}"
    );
}

/// Соединение, не приславшее ни байта, не задерживает остановку до тайм-аута заголовков:
/// hyper закрывает его сразу при graceful shutdown.
#[tokio::test]
async fn shutdown_closes_silent_connection_at_once() {
    let server = Server::start(
        ServerTimeouts {
            header_read: LONG,
            idle: LONG,
            shutdown: LONG,
        },
        Duration::ZERO,
    )
    .await;
    let mut stream = TcpStream::connect(server.addr).await.unwrap();
    // Сервер успевает принять соединение до сигнала остановки.
    tokio::time::sleep(Duration::from_millis(100)).await;

    let started = Instant::now();
    server.shutdown.cancel();
    read_eof(&mut stream).await;
    server
        .stopped()
        .await
        .expect("silent connection must not delay shutdown");

    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(1),
        "stopped after {elapsed:?}"
    );
}

#[tokio::test]
async fn shutdown_respects_deadline() {
    let server = Server::start(
        ServerTimeouts {
            header_read: LONG,
            idle: LONG,
            shutdown: Duration::from_millis(200),
        },
        LONG,
    )
    .await;
    let client = server.sleeping_request().await;

    let started = Instant::now();
    server.shutdown.cancel();
    let result = server.stopped().await;
    let elapsed = started.elapsed();

    assert!(
        matches!(result, Err(ServeError::ShutdownTimeout(_))),
        "{result:?}"
    );
    assert!(
        elapsed < Duration::from_secs(1),
        "stopped after {elapsed:?}"
    );
    // Оставшееся соединение закрыто: клиент получает ошибку, а не ответ через 10 s.
    let response = tokio::time::timeout(WAIT, client)
        .await
        .expect("client must be released")
        .unwrap();
    assert!(response.is_err(), "{response:?}");
}
