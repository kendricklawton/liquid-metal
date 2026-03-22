use askama::Template;
use axum::{
    http::StatusCode,
    response::{Html, IntoResponse, Response},
};

#[derive(Template)]
#[template(path = "error.html")]
struct ErrorTemplate<'a> {
    status_code: u16,
    title: &'a str,
    message: &'a str,
}

/// Structured error type for the web crate.
///
/// Renders HTML error pages for browser requests, JSON for HTMX partials.
#[derive(Debug)]
pub enum WebError {
    NotFound(String),
    Internal(String),
    Unauthorized,
    ApiError(StatusCode, String),
}

impl std::fmt::Display for WebError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(msg) => write!(f, "not found: {msg}"),
            Self::Internal(msg) => write!(f, "internal error: {msg}"),
            Self::Unauthorized => write!(f, "unauthorized"),
            Self::ApiError(status, msg) => write!(f, "API error ({status}): {msg}"),
        }
    }
}

impl IntoResponse for WebError {
    fn into_response(self) -> Response {
        let (status, title, message) = match &self {
            WebError::NotFound(msg) => (StatusCode::NOT_FOUND, "Not Found", msg.as_str()),
            WebError::Internal(msg) => {
                tracing::error!(error = %msg, "internal server error");
                (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error", "Something went wrong. Please try again later.")
            }
            WebError::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                "Unauthorized",
                "You need to sign in to access this page.",
            ),
            WebError::ApiError(status, msg) => (*status, "Error", msg.as_str()),
        };

        let tmpl = ErrorTemplate { status_code: status.as_u16(), title, message };
        let html = tmpl.render().unwrap_or_else(|_| {
            format!("{} — {}", status.as_u16(), title)
        });

        (status, Html(html)).into_response()
    }
}

impl From<anyhow::Error> for WebError {
    fn from(err: anyhow::Error) -> Self {
        WebError::Internal(err.to_string())
    }
}

impl From<askama::Error> for WebError {
    fn from(err: askama::Error) -> Self {
        WebError::Internal(format!("template render error: {err}"))
    }
}

impl From<reqwest::Error> for WebError {
    fn from(err: reqwest::Error) -> Self {
        if err.is_connect() {
            WebError::Internal("Unable to reach the API. Please try again later.".to_string())
        } else {
            WebError::Internal(err.to_string())
        }
    }
}
