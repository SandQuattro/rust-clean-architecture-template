//! Входные команды use case.

use crate::entity::Transfer;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindAllUsers {
    pub page: i64,
    pub size: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindUserById {
    pub id: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateUser {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateUser {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteUser {
    pub id: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferMoney {
    pub transfer: Transfer,
}
