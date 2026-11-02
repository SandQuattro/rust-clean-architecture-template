# Сборка тестов при сборке образа; запуск через cargo (проще, чем искать бинарник
# с хешем в имени), артефакты берутся из кэша target.
FROM rust:1.99.1-slim-bookworm

RUN apt-get update && \
    apt-get install -y --no-install-recommends make && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY . .

ENV SQLX_OFFLINE=true
RUN cargo test -p integration-tests --release --no-run

CMD ["cargo", "test", "-p", "integration-tests", "--release"]
