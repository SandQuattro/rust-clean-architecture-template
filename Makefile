.DEFAULT_GOAL := help

CARGO_DENY := ./bin/bin/cargo-deny
CARGO_SQLX := ./bin/bin/cargo-sqlx
CARGO_LLVM_COV := ./bin/bin/cargo-llvm-cov
PREPARE_DB_URL := postgres://postgres@127.0.0.1:5436/sqlx_prepare
# БД тестов из `make test-db`; DATABASE_URL из окружения имеет приоритет.
TEST_DATABASE_URL := postgres://postgres@127.0.0.1:5436/postgres
VERSION := $(shell git describe --tags --always --dirty)
COMPOSE := docker compose -p rust-cat
# Пароли в git не хранятся: DB_PASSWORD берётся из окружения, docker compose читает и .env.
DB_USER ?= postgres
DB_HOST ?= localhost
DB_PORT ?= 5435
DB_NAME ?= demo
K6_IMAGE := grafana/k6:2.2.0
BASE_URL ?= http://localhost:9001
TARGET ?= rust
K6_ARGS ?=
HEAP_FILE ?= app.heap
# Локальный k6, если установлен; иначе закреплённый образ в docker.
K6 = $(if $(shell command -v k6),k6,docker run --rm -i --network host --user $(shell id -u):$(shell id -g) -v $(CURDIR):/work -w /work $(K6_IMAGE))

.PHONY: help fmt lint deny tests test-coverage test-db test-db-drop sqlx-prepare build run docker dc cleandc new-migration migrations-status test-integration load spike bench heapprof k8s-deploy k8s-delete k8s-status k8s-logs

help: ## Показать список целей
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "}; {printf "\033[36m%-30s\033[0m %s\n", $$1, $$2}'

fmt: ## Отформатировать код
	cargo fmt --all

lint: ## Проверить форматирование и запустить clippy
	cargo fmt --all --check
	cargo clippy --workspace --all-targets -- -D warnings

$(CARGO_DENY):
	cargo install --locked --root ./bin cargo-deny@0.20.2

deny: $(CARGO_DENY) ## Проверить лицензии, advisories и источники зависимостей
	$(CARGO_DENY) check

tests: export DATABASE_URL ?= $(TEST_DATABASE_URL)
tests: ## Юнит-тесты (без integration-tests); нужна БД из make test-db или DATABASE_URL
	cargo test --workspace --exclude integration-tests

$(CARGO_LLVM_COV):
	cargo install --locked --root ./bin cargo-llvm-cov@0.9.1

test-coverage: export DATABASE_URL ?= $(TEST_DATABASE_URL)
test-coverage: $(CARGO_LLVM_COV) ## Покрытие юнит-тестами (без integration-tests), HTML-отчёт target/llvm-cov/html
	rustup component add llvm-tools-preview
	PATH="$(CURDIR)/bin/bin:$$PATH" cargo llvm-cov --workspace --exclude integration-tests --open

$(CARGO_SQLX):
	cargo install --locked --root ./bin sqlx-cli@0.9.0 --no-default-features --features rustls,postgres

test-db: ## Поднять тестовый PostgreSQL (контейнер rust-cat-test-db, порт 5436) и дождаться готовности
	@# Одноразовая БД тестов без пароля (trust); порт открыт только на 127.0.0.1.
	@docker start rust-cat-test-db 2>/dev/null || docker run -d --name rust-cat-test-db -p 127.0.0.1:5436:5432 -e POSTGRES_HOST_AUTH_METHOD=trust postgres:16
	@# Проверка по TCP: во время initdb временный сервер образа слушает только unix-сокет.
	@for i in $$(seq 1 30); do \
		docker exec rust-cat-test-db pg_isready -h 127.0.0.1 -U postgres -q && exit 0; \
		sleep 1; \
	done; \
	echo "rust-cat-test-db is not ready after 30 s" >&2; exit 1

test-db-drop: ## Удалить тестовый PostgreSQL
	-docker rm -f rust-cat-test-db

sqlx-prepare: $(CARGO_SQLX) ## Обновить offline-данные sqlx (.sqlx/) по схеме из migrations/
	# Схема для проверки запросов макросами: чистая БД `sqlx_prepare` с применёнными миграциями.
	-docker exec rust-cat-test-db psql -U postgres -c 'DROP DATABASE IF EXISTS sqlx_prepare WITH (FORCE)'
	docker exec rust-cat-test-db psql -U postgres -c 'CREATE DATABASE sqlx_prepare'
	PATH="$(CURDIR)/bin/bin:$$PATH" DATABASE_URL=$(PREPARE_DB_URL) cargo sqlx migrate run --source migrations
	PATH="$(CURDIR)/bin/bin:$$PATH" DATABASE_URL=$(PREPARE_DB_URL) cargo sqlx prepare --workspace -- --all-targets

build: ## Собрать release-бинарник с версией
	APP_VERSION=$(VERSION) cargo build --release -p app

run: ## Запустить приложение локально (нужны Postgres и DB_PASSWORD, PG_POOL_MAX)
	cargo run -p app

docker: ## Запустить приложение в docker-контейнере
	docker build --build-arg APP_VERSION=$(VERSION) -t rust-template .
	docker run --rm \
		--name rust-template \
		--network host \
		-e DB_PASSWORD \
		rust-template

dc: ## Поднять все сервисы через docker compose
	APP_VERSION=$(VERSION) $(COMPOSE) up --remove-orphans --build

cleandc: ## Поднять все сервисы через docker compose, очистив том postgres
	$(COMPOSE) down --remove-orphans
	rm -rf pg_volume
	APP_VERSION=$(VERSION) $(COMPOSE) up --remove-orphans --build

new-migration: $(CARGO_SQLX) ## Создать SQL-миграцию: make new-migration name=add_something
	PATH="$(CURDIR)/bin/bin:$$PATH" cargo sqlx migrate add -r --source migrations $(name)

migrations-status: $(CARGO_SQLX) ## Статус миграций в локальной БД docker compose (порт 5435)
	PATH="$(CURDIR)/bin/bin:$$PATH" DATABASE_URL="postgres://$(DB_USER):$(DB_PASSWORD)@$(DB_HOST):$(DB_PORT)/$(DB_NAME)" cargo sqlx migrate info --source migrations

test-integration: ## Интеграционные тесты в docker compose
	ENV_NAME=dev APP_VERSION=$(VERSION) $(COMPOSE) up --build --exit-code-from integration-tests integration-tests

load: ## Нагрузочный k6-сценарий: make load [BASE_URL=http://localhost:9001] [TARGET=rust|go]
	@mkdir -p k6-results
	$(K6) run -e BASE_URL=$(BASE_URL) -e TARGET=$(TARGET) $(K6_ARGS) load-test/load_test.js

spike: ## k6-сценарий всплеска нагрузки: make spike [BASE_URL=...] [TARGET=rust|go]
	@mkdir -p k6-results
	$(K6) run -e BASE_URL=$(BASE_URL) -e TARGET=$(TARGET) $(K6_ARGS) load-test/spike_test.js

bench: ## Criterion-бенчмарк преобразования и сериализации users-with-orders
	cargo bench -p adapter-http --bench users_with_orders

heapprof: ## Снять профиль кучи с dev-приложения в app.heap или HEAP_FILE (нужен запуск с MALLOC_CONF, Linux)
	curl -fsS -o $(HEAP_FILE) $(BASE_URL)/debug/pprof/heap
	@echo "Профиль сохранён в $(HEAP_FILE). Просмотр: go tool pprof -http=:8080 $(HEAP_FILE)"

k8s-deploy: ## Задеплоить приложение в Minikube (нужен DB_PASSWORD в окружении)
	@test -n "$$DB_PASSWORD" || { echo "DB_PASSWORD is not set" >&2; exit 1; }
	eval $$(minikube docker-env) && \
	docker build --build-arg APP_VERSION=$(VERSION) -t rust-clean-arch-template:latest . && \
	printf '%s' "$$DB_PASSWORD" | kubectl create secret generic app-secrets --from-file=DB_PASSWORD=/dev/stdin --dry-run=client -o yaml | kubectl apply -f - && \
	kubectl apply -f k8s.yaml

k8s-delete: ## Удалить приложение из Minikube
	kubectl delete -f k8s.yaml
	kubectl delete secret app-secrets --ignore-not-found

k8s-status: ## Статус приложения в Minikube
	kubectl get pods -l app=clean-arch-template
	kubectl get services clean-arch-template
	minikube service clean-arch-template --url

k8s-logs: ## Логи приложения в Minikube
	kubectl logs -f deployment/clean-arch-template
