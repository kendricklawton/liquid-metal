# Quickstart

Deploy your first Go or Rust service in under five minutes.

> **Zero-SDK.** No platform library to import. No agent to run. Your service needs two things: a port to listen on (from the `PORT` env var) and a health endpoint that returns `200 OK`. Everything else is standard language tooling.

## 1. Install the CLI

```bash
curl -fsSL https://plat.run/install.sh | sh
```

Verify the install:

```bash
plat version
# plat v0.1.0-alpha (go1.25 darwin/arm64)
```

## 2. Authenticate

```bash
plat auth login
```

This opens a browser window for OAuth. Once complete, your token is stored securely in your system keychain.

## 3. Your service contract

The platform has two requirements for your binary:

| Requirement | Description |
|-------------|-------------|
| `PORT` env var | Your server must listen on `$PORT` |
| Health endpoint | Must return `200` on the path in `plat.toml` |

That's it. No imports. No SDK. Bring any Go or Rust HTTP server.

**Go example (`main.go`):**

```go
package main

import (
    "net/http"
    "os"
)

func main() {
    port := os.Getenv("PORT")
    http.HandleFunc("/healthz", func(w http.ResponseWriter, r *http.Request) {
        w.WriteHeader(http.StatusOK)
    })
    http.ListenAndServe(":"+port, nil)
}
```

**Rust example (`main.rs`):**

```rust
use axum::{Router, routing::get};
use tokio::net::TcpListener;

#[tokio::main]
async fn main() {
    let app = Router::new()
        .route("/health", get(|| async { "ok" }));
    let port = std::env::var("PORT").unwrap_or("3000".into());
    let listener = TcpListener::bind(format!("0.0.0.0:{port}")).await.unwrap();
    axum::serve(listener, app).await.unwrap()
}
```

## 4. Initialize your project

In your project directory:

```bash
plat init
```

The CLI detects `go.mod` or `Cargo.toml` and creates a `plat.toml` file:

```toml
[build]
lang = "go"
main = "./cmd/api"

[serve]
port = 8080
health_path = "/healthz"
```

## 5. Deploy

```bash
plat deploy
```

The platform compiles a static binary, runs your health check, and shifts traffic to the new revision.

## 6. Check status

```bash
plat status
# api  LIVE  v1  2026-03-01 14:32
```

---

> **Next:** See [Deployments](/docs/deployments) for the full build lifecycle, or the [CLI Reference](/docs/cli) for all commands.
