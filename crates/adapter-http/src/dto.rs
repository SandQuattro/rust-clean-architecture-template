//! DTO транспортного слоя: сущности домена наружу не протекают.
//! Doc-комментарии полей попадают в OpenAPI (совпадают с `doc:` Go-шаблона),
//! структурные ограничения задаются `#[garde]` и переносятся в схему schemars.

use garde::Validate;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, JsonSchema)]
pub struct UserDto {
    /// User ID
    pub id: i64,
    /// User name
    pub name: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct OrderDto {
    /// Order ID
    pub id: i64,
    /// Amount in minimal currency units, 100 cents = 1$
    pub amount: i64,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct UserOrdersDto {
    /// User ID
    pub id: i64,
    /// User name
    pub name: String,
    /// User orders; empty array if there are none
    pub orders: Vec<OrderDto>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct UsersResponse {
    pub users: Vec<UserDto>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct UsersWithOrdersResponse {
    pub users: Vec<UserOrdersDto>,
}

#[derive(Debug, Deserialize, Validate, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UserBody {
    /// User name
    #[garde(length(chars, min = 1, max = 255))]
    pub name: String,
}

#[derive(Debug, Deserialize, Validate, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TransferBody {
    /// Source account ID
    #[garde(range(min = 1))]
    pub from_account_id: i64,
    /// Destination account ID
    #[garde(range(min = 1))]
    pub to_account_id: i64,
    /// Amount in minimal currency units, 100 cents = 1$
    #[garde(range(min = 1))]
    pub amount: i64,
}

#[derive(Debug, Deserialize, Validate, JsonSchema)]
pub struct PageParams {
    /// 1-based page number
    #[garde(range(min = 1))]
    pub page: i64,
    /// page size
    #[garde(range(min = 1, max = 1000))]
    pub size: i64,
}

#[derive(Debug, Deserialize, Validate, JsonSchema)]
pub struct IdParam {
    /// user id
    #[garde(range(min = 1))]
    pub id: i64,
}
