//! Доменные сущности. Деньги хранятся в `i64` в минимальных единицах валюты (100 = 1$).

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Order {
    pub id: i64,
    pub user_id: i64,
    pub amount: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserOrders {
    pub id: i64,
    pub name: String,
    pub orders: Vec<Order>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transfer {
    pub from_account_id: i64,
    pub to_account_id: i64,
    pub amount: i64,
}
