# Secrets

## Overview

Liquid Metal never stores `.env` files. Secrets are encrypted at rest using envelope encryption and injected as environment variables at runtime — never baked into your binary.

> **Caution:** Never commit secrets to your repository. The platform scans pushes for common secret patterns and will warn (not block) if it detects them.

## Set a secret

```bash
plat secret set DATABASE_URL "postgres://user:pass@host/db"
plat secret set API_KEY "sk_live_..."
```

Secrets are scoped per project by default. Preview environments get a separate secret scope.

## List secrets

```bash
plat secret list
# DATABASE_URL  set 2026-03-01
# API_KEY       set 2026-02-28
```

Secret values are never displayed after being set.

## Delete a secret

```bash
plat secret delete API_KEY
```

## Scopes

| Scope | Description |
|-------|-------------|
| `production` | Live deployments on `main` branch |
| `preview` | All PR preview environments |
| `build` | Available during the build step only |

```bash
plat secret set --scope preview TEST_API_KEY "sk_test_..."
```

## Runtime injection

Secrets are injected as standard environment variables. Access them the same way you would any env var:

```go
// Go
dbURL := os.Getenv("DATABASE_URL")
```

```rust
// Rust
let db_url = std::env::var("DATABASE_URL").expect("DATABASE_URL not set");
```
