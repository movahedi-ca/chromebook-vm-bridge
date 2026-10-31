//! pool route handlers.

use crate::routes::not_implemented;
use crate::state::SharedState;
use axum::{extract::State, response::IntoResponse};

pub async fn acquire(State(_state): State<SharedState>) -> impl IntoResponse {
    not_implemented("acquire").await
}

pub async fn release(State(_state): State<SharedState>) -> impl IntoResponse {
    not_implemented("release").await
}

pub async fn navigate(State(_state): State<SharedState>) -> impl IntoResponse {
    not_implemented("navigate").await
}

pub async fn click(State(_state): State<SharedState>) -> impl IntoResponse {
    not_implemented("click").await
}

pub async fn fill(State(_state): State<SharedState>) -> impl IntoResponse {
    not_implemented("fill").await
}

pub async fn eval(State(_state): State<SharedState>) -> impl IntoResponse {
    not_implemented("eval").await
}

pub async fn order(State(_state): State<SharedState>) -> impl IntoResponse {
    not_implemented("order").await
}

pub async fn snapshot(State(_state): State<SharedState>) -> impl IntoResponse {
    not_implemented("snapshot").await
}

pub async fn text(State(_state): State<SharedState>) -> impl IntoResponse {
    not_implemented("text").await
}

pub async fn status(State(_state): State<SharedState>) -> impl IntoResponse {
    not_implemented("status").await
}

pub async fn speculate(State(_state): State<SharedState>) -> impl IntoResponse {
    not_implemented("speculate").await
}

