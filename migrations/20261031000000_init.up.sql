-- Итоговая схема: сведение четырёх миграций Go-шаблона в одну.
-- Деньги хранятся в BIGINT (минимальные единицы валюты, 100 = 1$).
CREATE TABLE users
(
    id         BIGSERIAL PRIMARY KEY,
    name       VARCHAR(255) NOT NULL,
    balance    BIGINT       NOT NULL DEFAULT 0 CHECK (balance >= 0),
    created_at TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE orders
(
    id      BIGSERIAL PRIMARY KEY,
    user_id BIGINT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    amount  BIGINT NOT NULL
);

CREATE INDEX idx_orders_user_id ON orders (user_id);

CREATE TABLE transactions
(
    id           BIGSERIAL PRIMARY KEY,
    from_user_id BIGINT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    to_user_id   BIGINT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    amount       BIGINT NOT NULL,
    created_at   TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX idx_transactions_from_user_id ON transactions (from_user_id);
CREATE INDEX idx_transactions_to_user_id ON transactions (to_user_id);
