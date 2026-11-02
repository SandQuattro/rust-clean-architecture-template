//! Композиционный корень сервиса: конфигурация, телеметрия, HTTP-сервер и запуск.

pub mod app;
pub mod config;
pub mod database;
mod duration;
mod http_metrics;
mod probes;
mod profiling;
pub mod router;
pub mod server;
pub mod sysinfo;
pub mod telemetry;
pub mod version;

// jemalloc — глобальный аллокатор бинарника и тестов `app`: статистика
// allocated/resident в `/metrics` реальная.
#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;
