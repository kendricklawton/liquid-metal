# Deployments

## Deploy lifecycle

1. **Push** — `plat deploy` or a git push triggers the pipeline
2. **Detect** — Language detected from `go.mod` or `Cargo.toml`
3. **Build** — Static binary compiled in an isolated build environment
4. **Package** — Binary placed in a scratch container (zero base image)
5. **Probe** — Health check endpoint polled until healthy
6. **Shift** — Traffic atomically shifted to the new revision
7. **Live** — Old revision decommissioned after grace period

## Atomic rollbacks

If the health check fails at any point during step 5, the deploy is automatically reverted to the last known-good revision. No manual intervention required.

```bash
plat rollback           # roll back to the previous revision
plat rollback --to v12  # roll back to a specific revision
```

## Preview environments

Every pull request automatically gets a live, isolated URL:

```
https://pr-42.your-project.plat.run
```

Preview environments run your full service — same build, same secrets (scoped), same health checks. They are torn down automatically when the PR is merged or closed.

> **Note:** Preview environments run race-detector builds for Go services (`go build -race`), surfacing data races before code reaches `main`.

## Multi-arch builds

Deployments compile for both `amd64` and `arm64` by default. The platform routes traffic to the most cost-effective architecture available.

```toml
# plat.toml
[build]
targets = ["amd64", "arm64"]   # default
```

## Revisions

Every deploy creates a numbered revision. List them:

```bash
plat revisions
# v14  LIVE    2026-03-01 14:32  (current)
# v13  RETIRED 2026-03-01 10:11
# v12  RETIRED 2026-02-28 22:05
```
