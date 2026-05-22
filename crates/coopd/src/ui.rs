//! Minimal static UI: a single-page Farm view with hen list + xterm.js shell.
//!
//! Served from `/` and `/farm`. All assets are embedded — no on-disk dependency.

use axum::{Router, response::Html, routing::get};

const FARM_HTML: &str = include_str!("ui/farm.html");
const CHAT_PREVIEW_HTML: &str = include_str!("ui/chat-preview.html");

pub fn router() -> Router {
    Router::new()
        .route("/", get(index))
        .route("/farm", get(index))
        .route("/chat-preview", get(chat_preview))
}

async fn index() -> Html<&'static str> {
    Html(FARM_HTML)
}

async fn chat_preview() -> Html<&'static str> {
    Html(CHAT_PREVIEW_HTML)
}
