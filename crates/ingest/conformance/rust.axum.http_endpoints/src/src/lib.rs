use axum::{routing::get, Router};

pub async fn users() {}

pub fn app() -> Router {
    Router::new().route("/users", get(users))
}
