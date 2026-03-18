use axum::{
    http::StatusCode,
    response::{Html, IntoResponse, Response},
};

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

        // Minimal HTML error page — will be replaced with Askama templates
        let body = format!(
            r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>{status} — {title}</title>
  <script src="https://cdn.tailwindcss.com"></script>
  <script>tailwind.config = {{ darkMode: 'class', theme: {{ extend: {{ borderRadius: {{ DEFAULT: '0', sm: '0', md: '0', lg: '0', xl: '0', '2xl': '0', '3xl': '0', full: '0' }}, colors: {{ zw: {{ bg: '#191919', surface: '#262626', border: '#4a4546', muted: '#8e8e8e', fg: '#bbbbbb' }}, zwl: {{ bg: '#eeeeee', surface: '#d7d7d7', border: '#aca9a9', muted: '#5c5c5c', fg: '#353535' }} }} }} }} }}</script>
  <script>(function(){{ var t = localStorage.getItem('lm-theme'); if (t === 'dark') document.documentElement.classList.add('dark'); else if (t === 'light') document.documentElement.classList.remove('dark'); else if (window.matchMedia('(prefers-color-scheme: dark)').matches) document.documentElement.classList.add('dark'); }})()</script>
</head>
<body class="bg-zwl-bg dark:bg-zw-bg text-zwl-fg dark:text-zw-fg min-h-screen flex items-center justify-center font-mono antialiased">
  <div class="text-center max-w-md px-6">
    <p class="text-6xl font-black font-mono text-zwl-border dark:text-zw-border mb-4">{status}</p>
    <h1 class="text-2xl font-black tracking-tight mb-2">{title}</h1>
    <p class="text-zwl-muted dark:text-zw-muted mb-8">{message}</p>
    <a href="/" class="inline-block bg-zwl-fg dark:bg-zw-fg text-zwl-bg dark:text-zw-bg px-6 py-3 text-xs font-mono font-bold uppercase tracking-widest hover:bg-zwl-muted dark:hover:bg-zw-muted transition-colors">
      Back to Home
    </a>
  </div>
</body>
</html>"#,
            status = status.as_u16(),
            title = title,
            message = html_escape(message),
        );

        (status, Html(body)).into_response()
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
     .replace('<', "&lt;")
     .replace('>', "&gt;")
     .replace('"', "&quot;")
     .replace('\'', "&#x27;")
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
