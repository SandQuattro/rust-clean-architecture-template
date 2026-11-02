//! Бенчмарк горячего пути `GET /users-with-orders`: маппинг сущностей в DTO и сериализация в JSON.

use adapter_http::dto::UsersWithOrdersResponse;
use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use domain::entity::{Order, UserOrders};
use std::hint::black_box;

fn users(n: i64, orders_per_user: i64) -> Vec<UserOrders> {
    (1..=n)
        .map(|id| UserOrders {
            id,
            name: format!("user-{id}"),
            orders: (1..=orders_per_user)
                .map(|o| Order {
                    id: id * 1000 + o,
                    user_id: id,
                    amount: o * 100,
                })
                .collect(),
        })
        .collect()
}

fn users_with_orders(c: &mut Criterion) {
    let source = users(1000, 10);
    c.bench_function("map_and_serialize_1000_users_x_10_orders", |b| {
        // Клонирование входа вынесено в setup и не входит в измерение.
        b.iter_batched(
            || source.clone(),
            |users| {
                let resp = UsersWithOrdersResponse::from(users);
                // Результат сериализации уходит в black_box целиком: ошибка не маскируется unwrap.
                black_box(serde_json::to_vec(&resp))
            },
            BatchSize::LargeInput,
        );
    });
}

criterion_group!(benches, users_with_orders);
criterion_main!(benches);
