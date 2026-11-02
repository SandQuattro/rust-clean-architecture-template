//! Стартовая диагностика (аналог `PrintSystemData` и `PrintMemoryInfo` в Go-шаблоне).

use std::net::{IpAddr, Ipv4Addr, UdpSocket};
use std::num::NonZero;

use tracing::info;

use crate::version::VERSION;

/// Пишет версию, окружение, hostname, PID, IPv4, число CPU и worker-потоков tokio,
/// домашний каталог. Вызывается внутри рантайма tokio.
pub fn log_startup_info(app_name: &str, environment: &str) {
    let ip = outbound_ipv4().map_or_else(|| "unknown".to_owned(), |ip| ip.to_string());
    let home =
        std::env::home_dir().map_or_else(|| "unknown".to_owned(), |dir| dir.display().to_string());
    info!(
        app = app_name,
        version = VERSION,
        environment,
        hostname = %hostname(),
        pid = std::process::id(),
        ip,
        cpus = std::thread::available_parallelism().map_or(0, NonZero::get),
        tokio_workers = tokio::runtime::Handle::current().metrics().num_workers(),
        home,
        "application info"
    );
}

/// Пишет статистику аллокатора и `_RJEM_MALLOC_CONF` (настройки jemalloc,
/// в том числе профилирования кучи).
#[cfg(not(target_env = "msvc"))]
pub fn log_memory_info() {
    let malloc_conf = std::env::var("_RJEM_MALLOC_CONF").unwrap_or_default();
    match crate::telemetry::metrics::allocator_stats() {
        Ok((allocated, resident)) => info!(
            allocator = "jemalloc",
            allocated_bytes = allocated,
            resident_bytes = resident,
            malloc_conf,
            "memory info"
        ),
        Err(err) => tracing::warn!(error = %err, malloc_conf, "jemalloc stats are unavailable"),
    }
}

/// На MSVC jemalloc не используется (см. `lib.rs`).
#[cfg(target_env = "msvc")]
pub fn log_memory_info() {
    info!(allocator = "system", "memory info");
}

/// `HOSTNAME`, иначе `/proc/sys/kernel/hostname` (Linux), иначе `unknown`.
fn hostname() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .filter(|name| !name.is_empty())
        .or_else(|| {
            std::fs::read_to_string("/proc/sys/kernel/hostname")
                .ok()
                .map(|name| name.trim().to_owned())
        })
        .unwrap_or_else(|| "unknown".to_owned())
}

/// IPv4 интерфейса исходящего маршрута. В std нет перечисления интерфейсов (в Go —
/// `net.InterfaceAddrs`), поэтому адрес берётся у UDP-сокета после `connect`:
/// `connect` только выбирает маршрут, пакеты не отправляются.
fn outbound_ipv4() -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.connect((Ipv4Addr::new(8, 8, 8, 8), 80)).ok()?;
    match socket.local_addr().ok()?.ip() {
        IpAddr::V4(ip) if !ip.is_loopback() && !ip.is_unspecified() => Some(ip),
        _ => None,
    }
}
