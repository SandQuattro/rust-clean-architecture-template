//! Хук паники из `telemetry`: запись `error!` с текстом паники, местом, backtrace и
//! `trace_id` спана, активного в потоке паники.
//!
//! Отдельный тестовый бинарник с одним тестом: хук глобален для процесса и не должен
//! подменять вывод паник других тестов. Новые тесты сюда не добавлять.

#![allow(clippy::unwrap_used, clippy::panic, reason = "test-only helpers")]

use std::io;
use std::sync::{Arc, Mutex};

use opentelemetry::trace::{TraceContextExt, TracerProvider as _};
use opentelemetry_sdk::trace::{Sampler, SdkTracerProvider};
use tracing::{Span, info_span};
use tracing_opentelemetry::OpenTelemetrySpanExt;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;

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

#[test]
fn panic_hook_logs_payload_location_backtrace_and_trace_id() {
    let provider = SdkTracerProvider::builder()
        .with_sampler(Sampler::AlwaysOn)
        .build();
    let buffer = Buffer::default();
    let subscriber = tracing_subscriber::registry()
        .with(app::telemetry::fmt_layer(true, buffer.clone()))
        .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")));
    let _guard = tracing::subscriber::set_default(subscriber);
    app::telemetry::install_panic_hook();

    let span = info_span!("request");
    let _entered = span.enter();
    let trace_id = Span::current()
        .context()
        .span()
        .span_context()
        .trace_id()
        .to_string();
    let result = std::panic::catch_unwind(|| panic!("hook-test-panic"));

    assert!(result.is_err());
    let output = String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap();
    let entry: serde_json::Value = serde_json::from_str(output.trim_end()).unwrap();
    assert_eq!(entry["level"], "ERROR", "{entry}");
    assert_eq!(entry["panic"], "hook-test-panic", "{entry}");
    assert!(
        entry["location"]
            .as_str()
            .is_some_and(|l| l.contains("tests/panic_hook.rs")),
        "{entry}"
    );
    assert!(
        entry["backtrace"].as_str().is_some_and(|b| !b.is_empty()),
        "{entry}"
    );
    assert_eq!(entry["trace_id"], trace_id.as_str(), "{entry}");
}
