use std::collections::BTreeSet;

use serde_json::Value;

use crate::{routes::build, usecase::MockUserUseCase};

fn spec() -> Value {
    let (_, api) = build(MockUserUseCase::new());
    serde_json::to_value(&api).unwrap()
}

fn operations(spec: &Value) -> Vec<&Value> {
    spec["paths"]
        .as_object()
        .unwrap()
        .values()
        .flat_map(|item| item.as_object().unwrap().values())
        .filter(|op| op.get("operationId").is_some())
        .collect()
}

fn operation<'a>(spec: &'a Value, id: &str) -> &'a Value {
    operations(spec)
        .into_iter()
        .find(|op| op["operationId"] == id)
        .unwrap_or_else(|| panic!("operation {id} not found"))
}

fn statuses(op: &Value) -> BTreeSet<String> {
    op["responses"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect()
}

fn set(codes: &[&str]) -> BTreeSet<String> {
    codes.iter().map(|c| (*c).to_owned()).collect()
}

#[test]
fn declared_responses_match_contract() {
    let spec = spec();
    let cases: [(&str, &[&str]); 7] = [
        ("list-users", &["200", "422", "500", "503"]),
        ("list-users-with-orders", &["200", "422", "500", "503"]),
        ("get-user-by-id", &["200", "404", "422", "500", "503"]),
        ("create-user", &["201", "400", "422", "500", "503"]),
        ("update-user", &["200", "400", "404", "422", "500", "503"]),
        ("delete-user", &["204", "404", "422", "500", "503"]),
        (
            "transfer-money",
            &["204", "400", "404", "409", "422", "500", "503"],
        ),
    ];
    for (id, codes) in cases {
        assert_eq!(statuses(operation(&spec, id)), set(codes), "{id}");
    }
}

#[test]
fn bodyless_operations_do_not_declare_400() {
    let spec = spec();
    for id in [
        "list-users",
        "list-users-with-orders",
        "get-user-by-id",
        "delete-user",
    ] {
        assert!(!statuses(operation(&spec, id)).contains("400"), "{id}");
    }
}

#[test]
fn operations_have_go_metadata() {
    let spec = spec();
    assert_eq!(spec["openapi"], "3.1.0");
    assert_eq!(spec["info"]["title"], "Clean Architecture Template");
    assert_eq!(spec["info"]["version"], "1.0.0");
    let bearer = &spec["components"]["securitySchemes"]["bearer"];
    assert_eq!(bearer["scheme"], "bearer");
    assert_eq!(bearer["bearerFormat"], "JWT");
    assert_eq!(spec["security"][0]["bearer"], serde_json::json!([]));

    let cases = [
        (
            "list-users",
            "list all users",
            "Get a page of users. Pages are 1-based.",
        ),
        (
            "list-users-with-orders",
            "list users with orders",
            "Get a page of users with their orders. Pages are 1-based.",
        ),
        ("get-user-by-id", "user by id", "Get a user by id."),
        (
            "create-user",
            "create new user",
            "Create a new user record.",
        ),
        (
            "update-user",
            "update user",
            "Update an existing user by ID. The ID is taken from the path only; an `id` field in the body is rejected with 422.",
        ),
        ("delete-user", "delete user", "Delete a user by ID."),
        (
            "transfer-money",
            "transfer money",
            "Transfer money between two accounts.",
        ),
    ];
    for (id, summary, description) in cases {
        let op = operation(&spec, id);
        assert_eq!(op["summary"], summary, "{id}");
        assert_eq!(op["description"], description, "{id}");
        assert_eq!(op["tags"], serde_json::json!(["Users"]), "{id}");
    }
}

fn param<'a>(op: &'a Value, name: &str) -> &'a Value {
    op["parameters"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == name)
        .unwrap_or_else(|| panic!("parameter {name} not found"))
}

#[test]
fn garde_constraints_reach_schema() {
    let spec = spec();
    let schemas = &spec["components"]["schemas"];
    let name = &schemas["UserBody"]["properties"]["name"];
    assert_eq!(name["minLength"], 1);
    assert_eq!(name["maxLength"], 255);
    let transfer = &schemas["TransferBody"]["properties"];
    assert_eq!(transfer["amount"]["minimum"], 1);
    assert_eq!(transfer["from_account_id"]["minimum"], 1);
    assert_eq!(transfer["to_account_id"]["minimum"], 1);

    let list = operation(&spec, "list-users");
    assert_eq!(param(list, "size")["schema"]["minimum"], 1);
    assert_eq!(param(list, "size")["schema"]["maximum"], 1000);
    assert_eq!(param(list, "page")["schema"]["minimum"], 1);
    let get = operation(&spec, "get-user-by-id");
    assert_eq!(param(get, "id")["schema"]["minimum"], 1);
}

#[test]
fn error_responses_use_problem_schema() {
    let spec = spec();
    for op in operations(&spec) {
        for (code, resp) in op["responses"].as_object().unwrap() {
            if code.starts_with('4') || code.starts_with('5') {
                let content = resp["content"].as_object().unwrap();
                assert_eq!(content.len(), 1, "{code}");
                assert_eq!(
                    content["application/problem+json"]["schema"]["$ref"],
                    "#/components/schemas/Problem",
                    "{code}"
                );
            }
        }
    }
}

#[test]
fn documented_paths_and_methods_match_go() {
    let spec = spec();
    let paths = spec["paths"].as_object().unwrap();
    let methods =
        |p: &str| -> BTreeSet<String> { paths[p].as_object().unwrap().keys().cloned().collect() };
    assert_eq!(methods("/users/{page}/{size}"), set(&["get"]));
    assert_eq!(methods("/users-with-orders/{page}/{size}"), set(&["get"]));
    assert_eq!(methods("/user/{id}"), set(&["get", "put", "delete"]));
    assert_eq!(methods("/user"), set(&["post"]));
    assert_eq!(methods("/transfer"), set(&["post"]));
    assert_eq!(paths.len(), 5);
}

#[tokio::test]
async fn serves_openapi_json_and_docs() {
    use axum::{body::Body, http::Request, http::StatusCode};
    use tower::ServiceExt;

    let (app, _) = build(MockUserUseCase::new());
    for uri in ["/openapi.json", "/docs"] {
        let resp = app
            .clone()
            .oneshot(Request::get(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{uri}");
        if uri == "/openapi.json" {
            let ct = resp.headers()["content-type"].to_str().unwrap().to_owned();
            assert_eq!(ct, "application/json");
            let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .unwrap();
            let doc: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(doc, spec());
            assert_eq!(operations(&doc).len(), 7);
        }
    }
}
