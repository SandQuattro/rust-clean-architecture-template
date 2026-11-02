//! Реализация порта `UserRepository` на sqlx/PostgreSQL.

mod user_repository;

pub use user_repository::PgUserRepository;
