use mockall::predicate::eq;

use super::UserService;
use crate::command::{
    CreateUser, DeleteUser, FindAllUsers, FindUserById, TransferMoney, UpdateUser,
};
use crate::entity::{Transfer, User};
use crate::error::Error;
use crate::port::MockUserRepository;

fn user(id: i64, name: &str) -> User {
    User {
        id,
        name: name.to_owned(),
    }
}

#[tokio::test]
async fn find_all_users_rejects_invalid_pagination() {
    for (page, size) in [(0, 10), (1, 0), (-1, 10)] {
        let mut repo = MockUserRepository::new();
        repo.expect_get_all_users().never();
        let svc = UserService::new(repo);

        let res = svc.find_all_users(FindAllUsers { page, size }).await;

        assert!(
            matches!(res, Err(Error::InvalidPagination)),
            "({page}, {size})"
        );
    }
}

#[tokio::test]
async fn find_all_users_passes_offset_and_limit() {
    let mut repo = MockUserRepository::new();
    repo.expect_get_all_users()
        .with(eq(20), eq(10))
        .times(1)
        .returning(|_, _| Box::pin(std::future::ready(Ok(vec![user(1, "a")]))));
    let svc = UserService::new(repo);

    let res = svc
        .find_all_users(FindAllUsers { page: 3, size: 10 })
        .await
        .unwrap();

    assert_eq!(res, vec![user(1, "a")]);
}

#[tokio::test]
async fn find_all_users_saturates_offset() {
    let mut repo = MockUserRepository::new();
    repo.expect_get_all_users()
        .with(eq(i64::MAX), eq(1000))
        .times(1)
        .returning(|_, _| Box::pin(std::future::ready(Ok(vec![]))));
    let svc = UserService::new(repo);

    let res = svc
        .find_all_users(FindAllUsers {
            page: i64::MAX,
            size: 1000,
        })
        .await
        .unwrap();

    assert!(res.is_empty());
}

#[tokio::test]
async fn find_all_users_with_orders_passes_offset_and_propagates_error() {
    let mut repo = MockUserRepository::new();
    repo.expect_get_all_users_with_orders()
        .with(eq(5), eq(5))
        .times(1)
        .returning(|_, _| Box::pin(std::future::ready(Err(Error::internal("query", "boom")))));
    let svc = UserService::new(repo);

    let res = svc
        .find_all_users_with_orders(FindAllUsers { page: 2, size: 5 })
        .await;

    assert!(matches!(
        res,
        Err(Error::Internal {
            context: "query",
            ..
        })
    ));
}

#[tokio::test]
async fn find_user_by_id_propagates_not_found() {
    let mut repo = MockUserRepository::new();
    repo.expect_get_user_by_id()
        .with(eq(42))
        .times(1)
        .returning(|_| Box::pin(std::future::ready(Err(Error::UserNotFound))));
    let svc = UserService::new(repo);

    let res = svc.find_user_by_id(FindUserById { id: 42 }).await;

    assert!(matches!(res, Err(Error::UserNotFound)));
}

#[tokio::test]
async fn create_user_rejects_empty_name() {
    let mut repo = MockUserRepository::new();
    repo.expect_insert_user().never();
    let svc = UserService::new(repo);

    let res = svc
        .create_user(CreateUser {
            name: String::new(),
        })
        .await;

    assert!(matches!(res, Err(Error::InvalidUserName)));
}

#[tokio::test]
async fn create_user_passes_name() {
    let mut repo = MockUserRepository::new();
    repo.expect_insert_user()
        .withf(|name| name == "Mike")
        .times(1)
        .returning(|name| Box::pin(std::future::ready(Ok(user(1, name)))));
    let svc = UserService::new(repo);

    let res = svc
        .create_user(CreateUser {
            name: "Mike".to_owned(),
        })
        .await
        .unwrap();

    assert_eq!(res, user(1, "Mike"));
}

#[tokio::test]
async fn update_user_rejects_empty_name() {
    let mut repo = MockUserRepository::new();
    repo.expect_update_user().never();
    let svc = UserService::new(repo);

    let res = svc
        .update_user(UpdateUser {
            id: 7,
            name: String::new(),
        })
        .await;

    assert!(matches!(res, Err(Error::InvalidUserName)));
}

#[tokio::test]
async fn update_user_passes_id_and_name() {
    let mut repo = MockUserRepository::new();
    repo.expect_update_user()
        .withf(|id, name| *id == 7 && name == "x")
        .times(1)
        .returning(|id, name| Box::pin(std::future::ready(Ok(user(id, name)))));
    let svc = UserService::new(repo);

    let res = svc
        .update_user(UpdateUser {
            id: 7,
            name: "x".to_owned(),
        })
        .await
        .unwrap();

    assert_eq!(res, user(7, "x"));
}

#[tokio::test]
async fn delete_user_propagates_not_found() {
    let mut repo = MockUserRepository::new();
    repo.expect_delete_user()
        .with(eq(9))
        .times(1)
        .returning(|_| Box::pin(std::future::ready(Err(Error::UserNotFound))));
    let svc = UserService::new(repo);

    let res = svc.delete_user(DeleteUser { id: 9 }).await;

    assert!(matches!(res, Err(Error::UserNotFound)));
}

#[tokio::test]
async fn transfer_rejects_non_positive_amount() {
    for (from, to, amount) in [(1, 2, 0), (1, 2, -5), (3, 3, 0)] {
        let mut repo = MockUserRepository::new();
        repo.expect_transfer_money().never();
        let svc = UserService::new(repo);

        let res = svc
            .transfer_money(TransferMoney {
                transfer: Transfer {
                    from_account_id: from,
                    to_account_id: to,
                    amount,
                },
            })
            .await;

        assert!(
            matches!(res, Err(Error::NonPositiveAmount)),
            "({from}, {to}, {amount})"
        );
    }
}

#[tokio::test]
async fn transfer_rejects_same_account() {
    let mut repo = MockUserRepository::new();
    repo.expect_transfer_money().never();
    let svc = UserService::new(repo);

    let res = svc
        .transfer_money(TransferMoney {
            transfer: Transfer {
                from_account_id: 1,
                to_account_id: 1,
                amount: 100,
            },
        })
        .await;

    assert!(matches!(res, Err(Error::SameAccount)));
}

#[tokio::test]
async fn transfer_passes_transfer() {
    let transfer = Transfer {
        from_account_id: 1,
        to_account_id: 2,
        amount: 100,
    };
    let mut repo = MockUserRepository::new();
    repo.expect_transfer_money()
        .with(eq(transfer))
        .times(1)
        .returning(|_| Box::pin(std::future::ready(Ok(()))));
    let svc = UserService::new(repo);

    svc.transfer_money(TransferMoney { transfer })
        .await
        .unwrap();
}

#[test]
fn error_messages_match_go() {
    let cases = [
        (Error::UserNotFound, "user not found"),
        (Error::SourceAccountNotFound, "source account not found"),
        (Error::DestAccountNotFound, "destination account not found"),
        (
            Error::InvalidUserName,
            "user name must be a non-empty valid UTF-8 string",
        ),
        (
            Error::InvalidPagination,
            "page and size must be greater than zero",
        ),
        (Error::NonPositiveAmount, "transfer amount must be positive"),
        (Error::SameAccount, "cannot transfer to the same account"),
        (Error::InsufficientFunds, "insufficient funds"),
    ];
    for (err, msg) in cases {
        assert_eq!(err.to_string(), msg);
    }
}
