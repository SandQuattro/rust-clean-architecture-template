//! JSON-формат логов с `trace_id` и `span_id` текущего OTel-спана.

use std::cell::Cell;
use std::fmt;

use opentelemetry::trace::TraceContextExt;
use tracing::{Event, Subscriber};
use tracing_subscriber::fmt::format::{Format, Json, Writer};
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormatFields};
use tracing_subscriber::registry::LookupSpan;

thread_local! {
    /// Буфер строки лога, переиспользуемый между событиями потока (без аллокации на строку).
    static LINE: Cell<String> = const { Cell::new(String::new()) };
}

/// JSON-форматтер tracing-subscriber (`flatten_event(true)`, ключ сообщения
/// `message`) с полями `trace_id` и `span_id`, как в Go-логгере. Объекты `span` и
/// `spans` не пишутся: они раздували access-строку примерно до 1 KB.
///
/// Контекст берётся из `opentelemetry::Context::current()`: слой
/// tracing-opentelemetry при входе в спан делает его OTel-контекст текущим
/// (`with_context_activation`, включено по умолчанию). Прочитать `OtelData`
/// спана напрямую нельзя: в tracing-opentelemetry 0.34 тип приватный, а
/// `get_otel_context` требует `Dispatch`, которого у форматтера нет.
pub(super) struct JsonWithTraceIds {
    inner: Format<Json>,
}

impl Default for JsonWithTraceIds {
    fn default() -> Self {
        Self {
            inner: tracing_subscriber::fmt::format()
                .json()
                .flatten_event(true)
                .with_current_span(false)
                .with_span_list(false),
        }
    }
}

impl<S, N> FormatEvent<S, N> for JsonWithTraceIds
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let cx = opentelemetry::Context::current();
        let span = cx.span();
        let span_context = span.span_context();
        if !span_context.is_valid() {
            return self.inner.format_event(ctx, writer, event);
        }

        // Внутренний форматтер пишет один объект `{...}` и перевод строки;
        // поля дописываются перед закрывающей скобкой.
        // Буфер забирается из `Cell` на время форматирования: повторный вход (лог внутри
        // `Display` поля) получит пустой буфер, а не панику `RefCell`.
        let mut line = LINE.take();
        line.clear();
        let result = (|| {
            self.inner
                .format_event(ctx, Writer::new(&mut line), event)?;
            let Some(end) = line.rfind('}') else {
                return writer.write_str(&line);
            };
            let (object, tail) = line.split_at(end);
            write!(
                writer,
                r#"{object},"trace_id":"{}","span_id":"{}"{tail}"#,
                span_context.trace_id(),
                span_context.span_id(),
            )
        })();
        LINE.set(line);
        result
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::{Arc, Mutex};

    use opentelemetry::trace::{TraceContextExt, TracerProvider as _};
    use opentelemetry_sdk::trace::{Sampler, SdkTracerProvider};
    use tracing::{Level, Span, debug, info, info_span, warn};
    use tracing_opentelemetry::OpenTelemetrySpanExt;
    use tracing_subscriber::Layer as _;
    use tracing_subscriber::fmt::MakeWriter;
    use tracing_subscriber::layer::SubscriberExt;

    use crate::telemetry::{TelemetrySettings, fmt_layer, level_filter, span_filter, subscriber};

    /// Общий буфер, в который пишет fmt-слой.
    #[derive(Clone, Default)]
    struct Buffer(Arc<Mutex<Vec<u8>>>);

    impl Buffer {
        fn contents(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }

        fn json(&self) -> serde_json::Value {
            serde_json::from_str(self.contents().trim_end()).unwrap()
        }
    }

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

    /// Локальный провайдер без экспортёра: каждый спан сэмплируется.
    fn provider() -> SdkTracerProvider {
        SdkTracerProvider::builder()
            .with_sampler(Sampler::AlwaysOn)
            .build()
    }

    fn is_lower_hex(value: &str, len: usize) -> bool {
        value.len() == len && value.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f'))
    }

    #[test]
    fn json_log_has_trace_and_span_ids() {
        let provider = provider();
        let buffer = Buffer::default();
        let subscriber = tracing_subscriber::registry()
            .with(fmt_layer(true, buffer.clone()))
            .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")));
        let _guard = tracing::subscriber::set_default(subscriber);

        let span = info_span!("req");
        let _entered = span.enter();
        info!("hello");
        let cx = Span::current().context();
        let expected = cx.span().span_context().clone();

        let line = buffer.contents();
        assert!(line.contains(r#""message":"hello""#), "{line}");
        assert!(line.contains(r#""level":"INFO""#), "{line}");
        let entry = buffer.json();
        let trace_id = entry["trace_id"].as_str().unwrap();
        let span_id = entry["span_id"].as_str().unwrap();
        assert!(is_lower_hex(trace_id, 32), "{trace_id}");
        assert!(is_lower_hex(span_id, 16), "{span_id}");
        assert_eq!(trace_id, expected.trace_id().to_string());
        assert_eq!(span_id, expected.span_id().to_string());
    }

    /// Внутри спана с полями запись не содержит объектов `span` и `spans`, `trace_id` остаётся.
    #[test]
    fn json_log_has_no_span_objects() {
        let provider = provider();
        let buffer = Buffer::default();
        let subscriber = tracing_subscriber::registry()
            .with(fmt_layer(true, buffer.clone()))
            .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")));
        let _guard = tracing::subscriber::set_default(subscriber);

        let span = info_span!("req", http.route = "/user/{id}");
        let _entered = span.enter();
        info!("hello");

        let entry = buffer.json();
        assert!(entry.get("span").is_none(), "{entry}");
        assert!(entry.get("spans").is_none(), "{entry}");
        assert!(entry["trace_id"].is_string(), "{entry}");
    }

    #[test]
    fn json_log_without_span_has_no_trace_id() {
        let provider = provider();
        let buffer = Buffer::default();
        let subscriber = tracing_subscriber::registry()
            .with(fmt_layer(true, buffer.clone()))
            .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")));
        let _guard = tracing::subscriber::set_default(subscriber);

        info!("hello");

        let entry = buffer.json();
        assert_eq!(entry["message"], "hello");
        assert!(entry.get("trace_id").is_none(), "{entry}");
        assert!(entry.get("span_id").is_none(), "{entry}");
    }

    /// Уровень логов выше INFO не отключает трейсинг: спан уровня INFO создаётся для
    /// OTel-слоя, а `warn!` внутри него получает `trace_id`.
    #[test]
    fn warn_log_level_keeps_spans_and_trace_ids() {
        let provider = provider();
        let buffer = Buffer::default();
        let settings = TelemetrySettings {
            service_name: "test",
            otlp_endpoint: "",
            level: Level::WARN,
            debug: false,
            json: true,
        };
        let _guard = tracing::subscriber::set_default(subscriber(
            &settings,
            buffer.clone(),
            provider.tracer("test"),
        ));

        let span = info_span!("req");
        let _entered = span.enter();
        info!("filtered-out");
        warn!("kept");
        let cx = Span::current().context();
        let expected = cx.span().span_context().clone();

        assert!(expected.is_valid(), "INFO span must reach the OTel layer");
        // В буфере одна запись: `info!` отфильтрован уровнем логов.
        let entry = buffer.json();
        assert_eq!(entry["message"], "kept", "{entry}");
        assert_eq!(
            entry["trace_id"],
            expected.trace_id().to_string(),
            "{entry}"
        );
        assert_eq!(entry["span_id"], expected.span_id().to_string(), "{entry}");
    }

    /// Пишет запись `debug!` через fmt-слой с фильтром `level_filter(INFO, debug)`.
    fn debug_output(debug: bool) -> String {
        let buffer = Buffer::default();
        let subscriber = tracing_subscriber::registry()
            .with(level_filter(Level::INFO, debug))
            .with(fmt_layer(false, buffer.clone()));
        tracing::subscriber::with_default(subscriber, || debug!("debug-record"));
        buffer.contents()
    }

    #[test]
    fn level_filter_respects_debug_flag() {
        assert!(!debug_output(false).contains("debug-record"));
        assert!(debug_output(true).contains("debug-record"));
    }

    /// При DEBUG зависимости из списка шума ограничены INFO, `sqlx` и приложение остаются на debug.
    #[test]
    fn debug_level_caps_dependency_noise_but_keeps_sqlx() {
        let buffer = Buffer::default();
        let subscriber = tracing_subscriber::registry()
            .with(level_filter(Level::INFO, true))
            .with(fmt_layer(false, buffer.clone()));
        tracing::subscriber::with_default(subscriber, || {
            debug!(target: "h2::codec::framed_read", "noise-h2");
            debug!(target: "opentelemetry-otlp", "noise-otlp");
            debug!(target: "opentelemetry_sdk", "noise-sdk");
            debug!(target: "tower::buffer::worker", "noise-tower");
            debug!(target: "hyper_util::client::legacy::connect::http", "noise-hyper");
            info!(target: "opentelemetry_sdk", "kept-sdk-info");
            debug!(target: "sqlx::query", "kept-sqlx");
            debug!(target: "app::database", "kept-app");
        });
        let out = buffer.contents();
        assert!(!out.contains("noise-"), "{out}");
        for kept in ["kept-sdk-info", "kept-sqlx", "kept-app"] {
            assert!(out.contains(kept), "{kept}: {out}");
        }
    }

    /// Спаны `aide` не доходят до OTel-слоя, остальные доходят.
    #[test]
    fn otel_layer_skips_aide_spans() {
        let provider = provider();
        let subscriber = tracing_subscriber::registry().with(
            tracing_opentelemetry::layer()
                .with_tracer(provider.tracer("test"))
                .with_filter(span_filter(false)),
        );
        tracing::subscriber::with_default(subscriber, || {
            let aide = tracing::info_span!(target: "aide::axum::routing", "api_route");
            let own = info_span!("req");
            assert!(!aide.context().span().span_context().is_valid());
            assert!(own.context().span().span_context().is_valid());
        });
    }
}
