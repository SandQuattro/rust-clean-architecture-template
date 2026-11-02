//! HTTP-адаптер: DTO, хендлеры, маршруты с OpenAPI-контрактом, ошибки problem+json,
//! валидирующие экстракторы, маппинг доменных ошибок.

pub mod convert;
pub mod dto;
pub mod error;
pub mod extract;
pub mod handlers;
pub mod problem;
pub mod routes;
pub mod usecase;

#[cfg(test)]
mod tests;
