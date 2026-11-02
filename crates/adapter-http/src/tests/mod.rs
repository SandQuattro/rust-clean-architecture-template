mod error;
mod extract;
mod handlers;
mod openapi;

use serde_json::Value;

/// Значения `location` из списка `errors` тела Problem.
fn locations(body: &Value) -> Vec<&str> {
    body["errors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["location"].as_str().unwrap())
        .collect()
}
