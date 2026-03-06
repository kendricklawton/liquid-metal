# CLI Reference

## Global flags

| Flag | Description |
|------|-------------|
| `--project <id>` | Target a specific project (overrides current directory detection) |
| `--token <token>` | Auth token (overrides `PLAT_TOKEN` env var) |
| `--json` | Output as JSON |

## `plat auth`

```bash
plat auth login        # Authenticate via browser OAuth
plat auth logout       # Remove stored credentials
plat auth status       # Show current auth state
```

## `plat deploy`

```bash
plat deploy                    # Deploy current directory
plat deploy --branch staging   # Deploy a specific branch
plat deploy --wait             # Block until deploy is complete
```

## `plat status`

```bash
plat status                    # Show all services and their state
plat status --service api      # Show a specific service
```

## `plat logs`

```bash
plat logs                      # Stream live logs
plat logs --since 1h           # Logs from the last hour
plat logs --service api        # Logs for a specific service
```

## `plat rollback`

```bash
plat rollback                  # Roll back to previous revision
plat rollback --to v12         # Roll back to a specific revision
```

## `plat revisions`

```bash
plat revisions                 # List all revisions
plat revisions --limit 5       # Show last 5 revisions
```

## `plat secret`

```bash
plat secret set KEY VALUE      # Set a secret
plat secret list               # List secret keys (not values)
plat secret delete KEY         # Delete a secret
```

## `plat init`

```bash
plat init                      # Detect language and create plat.toml
plat init --lang go            # Force Go project
plat init --lang rust          # Force Rust project
```

## `plat version`

```bash
plat version
# plat v0.1.0-alpha (go1.25 darwin/arm64)
```
