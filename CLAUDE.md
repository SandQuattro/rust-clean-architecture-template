# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Rust 1.99.1 (edition 2024) Clean Architecture template: REST API built on axum 0.8 + aide (OpenAPI 3.1 generated from types) over PostgreSQL 16 (sqlx 0.9), with `tracing` logs, OpenTelemetry tracing (OTLP gRPC), Prometheus metrics, and jemalloc. It is a port of the Go template `go-clean-architecture-template`: same domain (User / Order / Transfer), same HTTP contract, same environment variable names. In addition it has explicit overload behaviour: a shared concurrency limit with load shedding (503) and a request timeout.

Dependency versions are pinned in `[workspace.dependencies]` of the root `Cargo.toml`; the toolchain is pinned in `rust-toolchain.toml`. Adding a dependency beyond the existing set needs a justification (version, MSRV, license, maintenance) in the PR description. The design document is `.superpowers/specs/*-rust-clean-architecture-template-design.md` (untracked, local).

## Commands

### Build & Run
- `make run` — run locally (`cargo run -p app`); requires Postgres and env vars (`DB_PASSWORD` and `PG_POOL_MAX` are required by config)
- `DEBUG=true ENV_NAME=dev make cleandc` — full local stack via docker compose (`DB_PASSWORD` from the environment or the git-ignored `.env`; compose fails if it is unset), project `rust-cat` (app on host port 9001, Postgres 5435, Jaeger UI 16687, Prometheus 9091, Grafana 3001), wiping `pg_volume` first; `make dc` keeps the volume
- `make build` — release build with `APP_VERSION` injected (`git describe`), read at compile time via `option_env!`

### Tests
- `make tests` — unit tests (`cargo test --workspace --exclude integration-tests`); `adapter-postgres` and DB-backed `app` tests use `#[sqlx::test]` and need `DATABASE_URL` (the target defaults it to `postgres://postgres@127.0.0.1:5436/postgres`, the database from `make test-db`: trust auth, bound to 127.0.0.1, waits for `pg_isready`; `make test-db-drop` removes it)
- `make test-coverage` — cargo-llvm-cov (installed into `./bin`, pinned version) over the workspace without `integration-tests`, HTML report in `target/llvm-cov/html`
- Single test: `cargo test -p app --test router <name>`; per crate: `cargo test -p domain`
- Integration tests (`integration-tests/`, a workspace member excluded from `default-members`) run against a live app: `HOST=localhost PORT=9001 cargo test -p integration-tests`, or as the `integration-tests` compose service (`make test-integration`); `SKIP_INTEGRATION_TESTS=1` skips them. A successful transfer is unreachable via the public API (new users start with zero balance)
- `make bench` — criterion bench `users_with_orders` (DTO conversion + JSON serialization of 1000 users x 10 orders)
- `make load` / `make spike` — k6 scenarios from `load-test/` (local k6, else pinned `grafana/k6` in docker); `BASE_URL` (default `http://localhost:9001`) and `TARGET` (label of the summary file `k6-results/<scenario>-<target>.json`) are overridable, extra k6 flags go in `K6_ARGS`
- `make heapprof` — fetch `/debug/pprof/heap` into `app.heap` (Linux, `ENV_NAME=dev`, app started with `MALLOC_CONF=prof:true,prof_active:true,lg_prof_sample:19` in compose, i.e. `_RJEM_MALLOC_CONF` in the process)

### Lint
- `make lint` — `cargo fmt --all --check` and `cargo clippy --workspace --all-targets -- -D warnings`; `make fmt` formats
- `make deny` — cargo-deny (licenses, advisories, sources), installed into `./bin`
- Workspace lints: `unsafe_code = "forbid"`; `unwrap_used`, `expect_used`, `panic`, `dbg_macro`, `todo` are denied outside tests (`clippy.toml` allows unwrap/expect/panic in tests)

### Migrations & sqlx offline data
- `make new-migration name=<name>` — create a sqlx `.up.sql` / `.down.sql` pair in `migrations/` (sqlx has no single-file format)
- `make migrations-status` — status against the docker-compose Postgres (host port 5435)
- `make sqlx-prepare` — regenerate `.sqlx/` (offline data for `query!` macros) against a scratch database `sqlx_prepare` in the `make test-db` container; commit the result. The Docker build uses `SQLX_OFFLINE=true`
- Migrations are read from disk (`MIGRATIONS_DIR`, default `migrations`) and applied at startup on a dedicated connection without the API pool's `statement_timeout`, under the Postgres advisory lock that sqlx takes; a failed migration aborts startup. Down migrations never run at runtime. For multi-replica production deployments prefer a separate Job/initContainer

## Architecture

Workspace (dependency rule: inner crates know nothing about outer ones): `domain` <- `adapter-postgres`, `domain` <- `adapter-http`, all three <- `app`.

- `crates/domain` — entities (`User`, `Order`, `UserOrders`), command structs, `UserService` (business logic and validation), the `UserRepository` port, and the domain `Error` enum. Money is `i64` in minimal currency units (100 = 1$). Depends only on `thiserror`; no serde, axum, sqlx or tracing. Does not log.
- `crates/adapter-postgres` — `PgUserRepository`: sqlx `query!` / `query_as!` (checked at compile time against `.sqlx/`). Does not log.
- `crates/adapter-http` — axum handlers, transport DTOs (`dto.rs`, validated with garde), entity-to-DTO conversion (`convert.rs`), domain-to-HTTP error mapping (`error.rs`), `problem+json` bodies (`problem.rs`), OpenAPI routes (`routes.rs`). Declares its own consumer-side `UserUseCase` trait. Must not depend on `adapter-postgres` or `sqlx`. Package is named `adapter-http`, not `http`, to avoid a clash with the `http` crate.
- `crates/app` — composition root, binary `clean-arch-template`. `main.rs` loads config, initializes telemetry, installs signal handling; `app.rs` (`App::new` connects to the DB, applies migrations, builds the router, binds the port; `App::run` serves until cancelled, then drains and closes the pool); `config.rs`, `duration.rs` (Go-style durations: `h`, `m`, `s`, `ms`); `database.rs` (API pool, readiness pool, bounded connect retries, migrations); `server.rs` (hyper-util `http1::Builder` server with per-phase timeouts and bounded graceful shutdown); `router.rs` (layers, probes, dev routes); `telemetry/` (logs with `trace_id`/`span_id`, OTLP tracer, Prometheus recorder, panic hook); `sysinfo.rs` (startup diagnostics); `profiling.rs` (heap profile). `database` and `telemetry` take plain parameters and do not import `config`; mapping from `config::Config` lives in `app`/`main`.
- `integration-tests` — reqwest against a running app.
- `config` — `config.toml` / `config.json`. Lookup order: `CONFIG_PATH`; `config/config.toml`, `config/config.json` from the CWD; the same files next to the `app` crate. Env overrides files. The env-name-to-key table is `ENV_KEYS` in `crates/app/src/config.rs`; defaults are `DEFAULTS` there. `DbConfig::dsn()` is the single connection-string source (pool and migrator).

### Key conventions

- **No passwords or secrets in git**, not even in tests, examples or local defaults: tests generate password values at runtime, DSNs in tests carry no password, compose reads `DB_PASSWORD` from the environment or `.env`, the k8s Secret is created by `make k8s-deploy` from the environment. GitGuardian scans every commit of a PR.
- **Consumer-side ports**: `domain` declares `UserRepository`, `adapter-http` declares `UserUseCase`; mocks come from `mockall` (`automock` under `cfg(test)`). Traits use native `async fn` / `impl Future`, not `async_trait` (no per-call boxing on the hot path). A missing impl is a compile error.
- **Domain failures are enum variants** of `domain::error::Error` (`UserNotFound`, `InsufficientFunds`, ...), never `Ok(None)`. `ApiError` maps them with an exhaustive `match` without a `_` arm, so a new variant does not compile until it is mapped. `Internal` and `Unavailable` become a generic 500/503 problem+json; the internal text is logged, never sent to clients. Error texts match the Go template byte for byte.
- **Error bodies** are `application/problem+json` shaped like Huma's `ErrorModel` (`errors[]` with `location` such as `body.name`, `path.id`).
- **Transactions**: a repository method opens its own transaction (`pool.begin()` ... `commit()`); dropping the future without commit rolls back. `transfer_money` locks both account rows in one query (`WHERE id = ANY($1) ORDER BY id FOR UPDATE`) — deterministic lock order prevents deadlocks; keep that pattern for any multi-row locking.
- **Write operations are single-statement** (`UPDATE ... RETURNING` / `DELETE` plus affected-rows check): no read-then-write.
- **Pagination happens before the JOIN** in `users-with-orders`; offset arithmetic saturates (a huge page number returns an empty list, no overflow).
- **Request validation split**: structural validation lives in garde attributes on DTOs and `deny_unknown_fields` (-> 422, including path parameters out of `i64` range); business validation lives in `domain` (-> 400/404/409). `PUT /user/{id}` takes the ID from the path only; an `id` in the body is a 422. Declare a code in OpenAPI only if it is reachable over HTTP (the contract test in `adapter-http` guards this).
- **Overload behaviour**: API layers, outside to inside: catch-panic, OTel server span, HTTP metrics and access log, error-to-problem mapping, `LoadShed`, `GlobalConcurrencyLimitLayer` (`HTTP_MAX_CONCURRENCY`, one semaphore for the whole API — `ConcurrencyLimitLayer` would create one per route), request timeout (`HTTP_REQUEST_TIMEOUT`), body timeouts. Shed and timeout both return 503. `/livez`, `/readyz`, `/`, `/metrics` and dev routes sit outside the limit.
- **Timeouts**: no wait is unbounded; zero durations are rejected at config load (Go treats 0 as unlimited). `server.rs` disables hyper's own header timer and applies `HTTP_READ_TIMEOUT` (headers), `HTTP_IDLE_TIMEOUT` (keep-alive); `HTTP_REQUEST_TIMEOUT` is in the router and is also the `statement_timeout` of API pool connections (`PoolSettings::statement_timeout`, set via `PgConnectOptions::options`), so a dropped request holds a pool slot and row locks at most `HTTP_REQUEST_TIMEOUT` from statement start; sqlx then closes that connection (its release ping fails, one WARN line) and opens a new one on demand; the readiness pool shares these options.
- **Graceful shutdown**: one deadline D = signal time + `HTTP_SHUTDOWN_TIMEOUT`; connections drain until D, the API and readiness pools close concurrently within `max(D - now, 500 ms)`, the tracer flushes within `max(D - now, 1 s)`. Startup errors propagate to `main` and exit with code 1.
- **Config**: `DB_SSLMODE` accepts all six libpq modes; sqlx is built with `tls-rustls-ring-native-roots` (rustls + ring, system trust store, like pgx), so `verify-ca`/`verify-full` trust CAs installed in the image; `PG_POOL_HEALTHCHECK` (minutes) maps to sqlx `idle_timeout` (sqlx has no periodic health check); `test_before_acquire` is off, a `before_acquire` hook pings only connections idle for more than 1 s (pgxpool's rule); DB connect makes up to `CONNECT_ATTEMPTS` (10) attempts, each bounded by a tokio timeout; `DEBUG=true` enables SQL statement logging and debug level.
- **Logging**: `tracing` global facade (no injected logger); only `app` and `adapter-http` log; `domain` and `adapter-postgres` do not. `ENV_NAME=prod` selects JSON output. `LOG_LEVEL` filters only the fmt layer; the OTel layer has its own filter (INFO, DEBUG with `DEBUG=true`), so a higher log level never disables spans or `trace_id`. Panics are logged by a panic hook with backtrace and `trace_id`; clients get a generic 500.
- **Allocator**: jemalloc is the global allocator (not on MSVC); heap profile only on Linux in dev; CPU profiling via external `perf` / `samply` (no endpoint).
- **Testing per layer**: `domain` — `UserService` with `MockUserRepository`; `adapter-http` — router via `tower::ServiceExt::oneshot` with `MockUserUseCase`; `adapter-postgres` — `#[sqlx::test]` on a real Postgres; `app` — config, database, server, router and panic-hook tests; `integration-tests` — HTTP against the live app.

### Runtime endpoints

App listens on `HTTP_PORT` (default 8000; docker-compose publishes it as host port 9001): `/docs` (OpenAPI UI, Scalar), `/openapi.json`, `/metrics` (HTTP, pool, process and jemalloc metrics, Prometheus text format), `/livez`, `/readyz` (pings the DB within 1 s through its own one-connection pool, so a saturated API pool does not fail it — returns 503 when Postgres is down), `/` (200 `OK`); dev-only (`ENV_NAME=dev`): `/debug/pprof/heap` (jemalloc heap profile in pprof format; 503 when profiling is not active, 501 outside Linux). Jaeger UI in compose: http://localhost:16687.
