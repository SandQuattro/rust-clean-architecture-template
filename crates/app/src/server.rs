//! HTTP/1.1-сервер на hyper: тайм-ауты соединения и остановка с дедлайном.
//!
//! `axum::serve` 0.8.9 не настраивает тайм-аут чтения заголовков и при остановке ждёт
//! соединения без дедлайна. Встроенный `header_read_timeout` hyper отсчитывается и во время
//! простоя keep-alive между запросами, поэтому с ним `HTTP_IDLE_TIMEOUT` больше
//! `HTTP_READ_TIMEOUT` не действовал бы. Сервер отключает таймер hyper и сам ведёт фазу
//! каждого соединения:
//!
//! | Фаза | Начало | Предел |
//! |---|---|---|
//! | `Head` | соединение принято или пришёл первый байт следующего запроса | `header_read` (`HTTP_READ_TIMEOUT`) |
//! | `Busy` | заголовки разобраны, запрос передан роутеру | нет: тайм-ауты запроса и тела задаёт роутер |
//! | `Idle` | ответ полностью передан hyper | `idle` (`HTTP_IDLE_TIMEOUT`) |
//!
//! Истечение предела закрывает соединение. По сигналу остановки сервер перестаёт принимать
//! соединения, просит hyper закрыть каждое после текущего ответа и ждёт не дольше `shutdown`;
//! оставшиеся соединения закрываются принудительно. Простаивающие соединения и соединения,
//! не приславшие ни байта, hyper закрывает сразу (`Dispatcher::disable_keep_alive` в hyper 1.12);
//! соединение с недочитанными заголовками ждёт до своего предела `Head`, как `StateActive` в Go.

use std::convert::Infallible;
use std::io;
use std::net::SocketAddr;
use std::pin::{Pin, pin};
use std::task::{Context, Poll};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use hyper::Request;
use hyper::body::{Body as HttpBody, Bytes, Frame, Incoming, SizeHint};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;
use tracing::{debug, error, info, warn};

/// Пауза после ошибки `accept`, не связанной с конкретным клиентом (например, `EMFILE`):
/// за это время другие соединения успевают закрыться и освободить дескрипторы.
const ACCEPT_ERROR_PAUSE: Duration = Duration::from_secs(1);

/// Предел буфера чтения соединения: заголовки, не уместившиеся в него, получают 431.
/// Умолчание hyper — около 408 KiB на соединение: сотни соединений с большими заголовками
/// исчерпали бы лимит памяти контейнера раньше, чем сработал бы load shed. Fiber в Go-шаблоне
/// ограничивает заголовки 4 KiB (`ReadBufferSize`); минимум hyper — 8 KiB.
const MAX_BUF_SIZE: usize = 16 * 1024;

/// Тайм-ауты сервера; `app` заполняет их из конфигурации.
#[derive(Debug, Clone, Copy)]
pub struct ServerTimeouts {
    /// Предел чтения заголовков запроса (`HTTP_READ_TIMEOUT`).
    pub header_read: Duration,
    /// Предел простоя keep-alive между запросами (`HTTP_IDLE_TIMEOUT`).
    pub idle: Duration,
    /// Предел ожидания текущих запросов при остановке (`HTTP_SHUTDOWN_TIMEOUT`).
    pub shutdown: Duration,
}

#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    #[error("HTTP listener: {0}")]
    Io(#[from] io::Error),
    #[error("connections were not drained within {0:?}")]
    ShutdownTimeout(Duration),
}

/// Обслуживает `router` на `listener`, пока не отменён `shutdown`, затем дренирует
/// соединения не дольше `timeouts.shutdown`.
///
/// Возвращает дедлайн остановки: момент отмены `shutdown` плюс `timeouts.shutdown`.
/// В остаток до него вызывающий код укладывает закрытие пула и трейсера.
pub async fn serve(
    listener: TcpListener,
    router: Router,
    timeouts: ServerTimeouts,
    shutdown: CancellationToken,
) -> Result<Instant, ServeError> {
    info!(addr = %listener.local_addr()?, "HTTP server listening");

    let mut connections = JoinSet::new();
    // Пауза после ошибки `accept` живёт вне `select!`: завершение соединения (ветка
    // `join_next`) не прерывает её, ветка `accept` отключена до её конца.
    let mut pause = pin!(tokio::time::sleep(Duration::ZERO));
    let mut paused = false;
    loop {
        tokio::select! {
            () = shutdown.cancelled() => break,
            () = pause.as_mut(), if paused => paused = false,
            // `TcpListener::accept` отменяем без потерь (cancel safe).
            accepted = listener.accept(), if !paused => match accepted {
                Ok((stream, peer)) => {
                    set_nodelay(&stream);
                    connections.spawn(serve_connection(
                        stream,
                        peer,
                        router.clone(),
                        timeouts,
                        shutdown.clone(),
                    ));
                }
                // Клиент сбросил соединение до `accept`: сервер исправен.
                Err(err) if is_client_abort(&err) => {}
                Err(err) => {
                    error!(error = %err, "accept failed");
                    pause.as_mut().reset(Instant::now() + ACCEPT_ERROR_PAUSE);
                    paused = true;
                }
            },
            // Завершённые задачи убираются сразу, иначе набор растёт с каждым соединением.
            Some(_) = connections.join_next() => {}
        }
    }

    let signalled = Instant::now();
    drop(listener);
    info!(
        connections = connections.len(),
        "HTTP server stopped accepting connections, draining"
    );
    let drained = tokio::time::timeout(timeouts.shutdown, async {
        while connections.join_next().await.is_some() {}
    })
    .await;
    if drained.is_err() {
        warn!(
            connections = connections.len(),
            timeout = ?timeouts.shutdown,
            "shutdown timeout, closing remaining connections"
        );
        connections.shutdown().await;
        return Err(ServeError::ShutdownTimeout(timeouts.shutdown));
    }
    // Переполнение возможно только при абсурдно большом `shutdown`: тогда остатка нет.
    Ok(signalled
        .checked_add(timeouts.shutdown)
        .unwrap_or(signalled))
}

/// Как TCP_NODELAY по умолчанию в Go (net.TCPConn): без него мелкие ответы keep-alive
/// ждут Nagle и delayed ACK.
fn set_nodelay(stream: &TcpStream) {
    if let Err(err) = stream.set_nodelay(true) {
        debug!(error = %err, "set_nodelay failed");
    }
}

/// Ошибка `accept`, вызванная клиентом (соединение сброшено до `accept`), а не сервером:
/// пауза не нужна.
fn is_client_abort(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionRefused
    )
}

/// Фаза соединения (см. документацию модуля).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Head,
    Busy,
    Idle,
}

impl Phase {
    fn limit(self, timeouts: ServerTimeouts) -> Option<Duration> {
        match self {
            Self::Head => Some(timeouts.header_read),
            Self::Busy => None,
            Self::Idle => Some(timeouts.idle),
        }
    }
}

/// Обслуживает одно соединение до его закрытия, тайм-аута фазы или конца остановки.
/// Адрес клиента `peer` передаётся роутеру в `ConnectInfo` (access-лог).
async fn serve_connection(
    stream: TcpStream,
    peer: SocketAddr,
    router: Router,
    timeouts: ServerTimeouts,
    shutdown: CancellationToken,
) {
    // Новое соединение сразу ждёт заголовки первого запроса.
    let (phase_tx, mut phase) = watch::channel(Phase::Head);
    let io = TokioIo::new(PhaseIo {
        stream,
        phase: phase_tx.clone(),
    });
    let service = service_fn(move |mut req: Request<Incoming>| {
        req.extensions_mut().insert(ConnectInfo(peer));
        let busy = Busy::enter(&phase_tx);
        let router = router.clone();
        async move {
            let response = router.oneshot(req).await?;
            Ok::<_, Infallible>(response.map(|inner| ResponseBody { inner, _busy: busy }))
        }
    });
    let mut conn = pin!(
        http1::Builder::new()
            .header_read_timeout(None)
            .max_buf_size(MAX_BUF_SIZE)
            .serve_connection(io, service)
    );

    let mut deadline = pin!(tokio::time::sleep(timeouts.header_read));
    let mut armed = true;
    let mut cancelled = pin!(shutdown.cancelled());
    let mut draining = false;
    loop {
        tokio::select! {
            result = conn.as_mut() => {
                if let Err(err) = result {
                    debug!(error = %err, "connection closed with error");
                }
                return;
            }
            // Отправители живут внутри `conn`, поэтому канал не закрывается раньше него.
            Ok(()) = phase.changed() => {
                let limit = phase.borrow_and_update().limit(timeouts);
                armed = match limit.and_then(|limit| Instant::now().checked_add(limit)) {
                    Some(at) => {
                        deadline.as_mut().reset(at);
                        true
                    }
                    None => false,
                };
            }
            () = deadline.as_mut(), if armed => {
                debug!(phase = ?*phase.borrow(), "connection timed out, closing");
                return;
            }
            () = cancelled.as_mut(), if !draining => {
                draining = true;
                conn.as_mut().graceful_shutdown();
            }
        }
    }
}

/// Сокет соединения: первый прочитанный байт следующего запроса переводит фазу
/// `Idle` в `Head`.
struct PhaseIo {
    stream: TcpStream,
    phase: watch::Sender<Phase>,
}

impl AsyncRead for PhaseIo {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let before = buf.filled().len();
        let poll = Pin::new(&mut this.stream).poll_read(cx, buf);
        if buf.filled().len() > before {
            this.phase.send_if_modified(|phase| {
                let idle = *phase == Phase::Idle;
                if idle {
                    *phase = Phase::Head;
                }
                idle
            });
        }
        poll
    }
}

impl AsyncWrite for PhaseIo {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().stream).poll_write(cx, buf)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().stream).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.stream.is_write_vectored()
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_shutdown(cx)
    }
}

/// Пока значение живо, соединение в фазе `Busy`; удаление переводит его в `Idle`.
struct Busy(watch::Sender<Phase>);

impl Busy {
    fn enter(phase: &watch::Sender<Phase>) -> Self {
        phase.send_replace(Phase::Busy);
        Self(phase.clone())
    }
}

impl Drop for Busy {
    fn drop(&mut self) {
        self.0.send_replace(Phase::Idle);
    }
}

/// Тело ответа, которое держит фазу `Busy` до конца передачи: hyper удаляет тело,
/// когда отправил его целиком или решил не отправлять (HEAD, 204).
struct ResponseBody {
    inner: Body,
    _busy: Busy,
}

impl HttpBody for ResponseBody {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, axum::Error>>> {
        Pin::new(&mut self.get_mut().inner).poll_frame(cx)
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}
