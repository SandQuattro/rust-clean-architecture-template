//! Маппинг сущностей домена в DTO и обратно.

use domain::entity::{Order, Transfer, User, UserOrders};

use crate::dto::{
    OrderDto, TransferBody, UserDto, UserOrdersDto, UsersResponse, UsersWithOrdersResponse,
};

impl From<User> for UserDto {
    fn from(u: User) -> Self {
        Self {
            id: u.id,
            name: u.name,
        }
    }
}

impl From<Order> for OrderDto {
    fn from(o: Order) -> Self {
        Self {
            id: o.id,
            amount: o.amount,
        }
    }
}

impl From<UserOrders> for UserOrdersDto {
    fn from(u: UserOrders) -> Self {
        Self {
            id: u.id,
            name: u.name,
            orders: u.orders.into_iter().map(OrderDto::from).collect(),
        }
    }
}

impl From<Vec<User>> for UsersResponse {
    fn from(users: Vec<User>) -> Self {
        Self {
            users: users.into_iter().map(UserDto::from).collect(),
        }
    }
}

impl From<Vec<UserOrders>> for UsersWithOrdersResponse {
    fn from(users: Vec<UserOrders>) -> Self {
        Self {
            users: users.into_iter().map(UserOrdersDto::from).collect(),
        }
    }
}

impl From<TransferBody> for Transfer {
    fn from(b: TransferBody) -> Self {
        Self {
            from_account_id: b.from_account_id,
            to_account_id: b.to_account_id,
            amount: b.amount,
        }
    }
}
