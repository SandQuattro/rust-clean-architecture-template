## ! ТОЛЬКО ДЛЯ ЛОКАЛЬНЫХ ТЕСТОВ !
FROM rust:1.99.1-slim-bookworm AS builder

# make нужен для сборки jemalloc
RUN apt-get update && \
    apt-get install -y --no-install-recommends make && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY . .

ARG APP_VERSION=dev
ENV APP_VERSION=$APP_VERSION
ENV SQLX_OFFLINE=true
RUN cargo build --release --locked -p app

FROM gcr.io/distroless/cc-debian12:nonroot

WORKDIR /app
COPY --from=builder /app/target/release/clean-arch-template ./app
COPY --from=builder /app/config/config.json ./config/config.json
COPY --from=builder /app/migrations ./migrations

EXPOSE 8000

CMD ["./app"]
