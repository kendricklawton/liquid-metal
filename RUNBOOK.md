# Liquid Metal — Runbook

Operational reference for local development. Follow top to bottom on a fresh session.

> `go-task` (Taskfile) is optional. Every section below shows both the `task` shorthand and the raw command.

---

## Prerequisites (one-time)

### Tools
```bash
brew install rustup go-task   # go-task is optional
rustup default stable
cargo install cargo-release
```

### Install the flux CLI (rebuild after CLI changes)
```bash
cargo install --path crates/cli
flux --help  # verify
```

> `~/.cargo/bin` must be on your `PATH`. Rustup adds this automatically; if not, add `export PATH="$PATH:$HOME/.cargo/bin"` to `~/.zshrc`.

---

## Starting the Dev Environment

Open 3 terminal tabs.

### Tab 1 — Infrastructure (Docker)
```bash
# With Taskfile
task up

# Without Taskfile
docker compose up -d
```
Starts: Postgres (:5432), NATS (:4222), RustFS S3 mock (:9000, console :9001)

### Tab 2 — Rust API
```bash
# With Taskfile
task dev:api

# Without Taskfile
RUST_LOG="api=debug,refinery_core=info" cargo run -p api
```
Waits for: `Listening on 0.0.0.0:7070`

### Tab 3 — Daemon
```bash
# With Taskfile
task dev:daemon

# Without Taskfile
NATS_URL="nats://127.0.0.1:4222" RUST_LOG="daemon=debug" cargo run -p daemon
```
Waits for: `TAP counter initialized from DB`

> On macOS: Firecracker/TAP are skipped automatically. Only Liquid (Wasm) deployments work locally.

---

## Authenticate

```bash
flux login
# Opens browser → WorkOS → token saved to ~/.config/flux/config.yaml
```

Verify:
```bash
flux whoami
```

---

## Deploy a Service (Liquid/Wasm)

Using the markdown-renderer example:

```bash
cd ~/repos/liquid-metal-templates/rust/liquid/markdown-renderer
flux init      # creates project in API, writes project_id into liquid-metal.toml
flux deploy    # builds main.wasm → uploads to RustFS → API → NATS → daemon runs it
```

Expected deploy output:
```
=> Deploying markdown-renderer (Engine: Liquid)...
=> Compiling to WebAssembly...
=> Artifact built: main.wasm (SHA256: xxxxxxxx...)
=> Requesting upload destination...
=> Uploading artifact to object storage...
=> Finalizing deployment...

✅ Deployment Successful!
   Service: markdown-renderer
   Status:  SERVICE_STATUS_PROVISIONING
```

---

## Verify the Service is Running

```bash
flux status
```

Expected output (after daemon provisions it):
```
NAME                ENGINE   STATUS    UPSTREAM
markdown-renderer   liquid   running   127.0.0.1:XXXXX
```

> If status stays `provisioning` for more than a few seconds, check Tab 3 (daemon logs) for errors.

---

## Hit the Service

```bash
# Replace PORT with the value from flux status UPSTREAM column
curl http://127.0.0.1:PORT/
```

---

## Tail Logs

```bash
# Get the service ID from flux status
flux status

# Tail logs (last 100 lines by default)
flux logs <service-id>
flux logs <service-id> --limit 500
```

---

## Workspace & Project Management

```bash
flux workspace list          # list workspaces (* = active)
flux workspace use <slug>    # switch active workspace

flux project list            # list projects in active workspace (* = current dir's project)
flux project use <slug>      # set project_id in ./liquid-metal.toml
```

---

## Testing

### Compile check (no infra needed)
```bash
cargo check --workspace
```

### Unit tests (no infra needed)
```bash
cargo test --workspace
```

### Integration tests (requires infra + API running)
```bash
cargo test -p api --test api
```

### API smoke tests (requires infra + API running)
```bash
# Health check
curl -s http://localhost:7070/healthz | jq

# Provision a test user
curl -s -X POST http://localhost:7070/auth/cli/provision \
  -H "X-Internal-Secret: $(grep INTERNAL_SECRET .env | cut -d= -f2)" \
  -H "Content-Type: application/json" \
  -d '{"sub":"test-001","email":"test@example.com","name":"Test User"}' | jq
```

---

## Releasing

Versioning is controlled by `version` in `[workspace.package]` in the root `Cargo.toml`. All crates share one version. Tagging triggers the GitHub Actions release workflow (cargo-dist builds all targets + updates Homebrew tap).

```bash
# Preview what a release would do — no changes made
task release:dry-run
# or: cargo release minor --dry-run

# Bump and release
task release:patch   # or: cargo release patch    0.1.0 → 0.1.1
task release:minor   # or: cargo release minor    0.1.0 → 0.2.0
task release:major   # or: cargo release major    0.1.0 → 1.0.0
```

`cargo release` will:
1. Bump the version in `Cargo.toml`
2. Commit: `chore: release vX.Y.Z`
3. Tag: `vX.Y.Z`
4. Push commit + tag → GitHub Actions takes it from there

> Set `verify = true` in `release.toml` before real releases to enforce a clean working tree.

---

## Production Infrastructure

### Provisioning

```bash
# One-time: create GCS bucket for Terraform state manually
# Then:
cd infra/terraform
cp terraform.tfvars.example terraform.tfvars   # fill in real values
terraform init -backend-config="bucket=your-tfstate-bucket"
terraform plan
terraform apply
```

This creates: NAT VPS (HAProxy, Nomad server, NATS, PgBouncer, VictoriaMetrics, VictoriaLogs, Grafana), 2 bare metal nodes (Nomad clients, Promtail, node_exporter), Managed Postgres, Object Storage, Block Storage, GCS backup buckets, Cloudflare DNS records, and Tailscale pre-auth keys.

### Observability Access (Tailscale only)

| Service | URL |
|---|---|
| Grafana | `http://{prefix}-nat-vps:3001` (admin / `GRAFANA_ADMIN_PASSWORD`) |
| VictoriaMetrics | `http://{prefix}-nat-vps:8428` |
| VictoriaLogs | `http://{prefix}-nat-vps:9428` |

### Backup Schedule (daily, UTC)

| Time | Backup |
|---|---|
| 02:00 | certbot renewal check |
| 03:00 | Postgres pg_dump → GCS, Nomad state → GCS (per node) |
| 03:30 | VictoriaMetrics snapshot → GCS |
| 04:00 | VictoriaLogs tar → GCS |
| 04:30 | S3 artifacts rclone sync → GCS |

---

## Production Deploy Order

Schema migrations are a separate step from serving traffic — run them before rolling the new binary so the DB is ready before any instance starts.

### Option 1: Nomad batch job (recommended)

```bash
# Upload migrations to S3, then dispatch the batch job
nomad job run infra/jobs/migrate.nomad.hcl
nomad job dispatch migrate

# Then roll the services
nomad job run infra/jobs/api.nomad.hcl
nomad job run infra/jobs/proxy.nomad.hcl
nomad job run infra/jobs/daemon-metal.nomad.hcl
nomad job run infra/jobs/daemon-liquid.nomad.hcl
```

### Option 2: Direct psql

```bash
# 1. Run migrations via psql against PgBouncer
DATABASE_URL="$PGBOUNCER_URL" psql -f migrations/V*.sql

# 2. Roll services via Nomad
nomad job run infra/jobs/api.nomad.hcl
```

### Option 3: Embedded in binary

```bash
# 1. Run migrations (exits when done — no server started)
DATABASE_URL="$DATABASE_URL" ./liquid-metal-api --migrate

# 2. Start (or restart) the API server
./liquid-metal-api
```

`MIGRATIONS_DATABASE_URL` can be set to a separate owner-privilege connection string if your app role doesn't have DDL rights:

```bash
MIGRATIONS_DATABASE_URL="postgres://lm_owner:...@host/liquidmetal" \
DATABASE_URL="postgres://lm_app:...@host/liquidmetal" \
./liquid-metal-api --migrate
```

---

## Stopping the Dev Environment

```bash
# With Taskfile
task down

# Without Taskfile
docker compose down

# Kill API and daemon with Ctrl+C in their respective terminals
```

---

## Re-deploying After Code Changes

```bash
# After changing CLI source
cargo install --path crates/cli

# After changing API or daemon source — restart the relevant tab (Ctrl+C, then rerun)

# Re-deploy a service
cd ~/repos/liquid-metal-templates/rust/liquid/markdown-renderer
flux deploy    # liquid-metal.toml already has project_id from flux init
```

---

## Troubleshooting

| Symptom | Check |
|---------|-------|
| `flux: command not found` | `export PATH="$PATH:$HOME/.cargo/bin"` and reinstall |
| `flux login` fails | `FLUX_WORKOS_CLIENT_ID` in `.env`, confirm Docker infra is running |
| `flux init` fails | API running? (`cargo run -p api` or `task dev:api`) |
| `flux deploy` upload fails | RustFS running? (`docker compose up -d rustfs`) Check `http://localhost:9001` |
| Status stuck at `provisioning` | Check daemon logs for errors |
| `GetUploadUrl` error | API can't reach RustFS — check `.env` S3 vars |

### Production

| Symptom | Check |
|---------|-------|
| Can't reach services externally | HAProxy running? `ssh nat-vps systemctl status haproxy` |
| TLS cert expired | `ssh nat-vps certbot certificates` — check renewal cron |
| Deploys not running | Nomad server up? `ssh nat-vps systemctl status nomad` |
| Events not delivering | NATS up? `ssh nat-vps systemctl status nats` |
| DB connection errors | PgBouncer up? `ssh nat-vps systemctl status pgbouncer` |
| No metrics/logs | Check VictoriaMetrics/VictoriaLogs: `ssh nat-vps systemctl status victoriametrics victorialogs` |
| Grafana inaccessible | `ssh nat-vps systemctl status grafana` — check `:3001` via Tailscale |
| Backup failed | Check `/var/log/backups/*.log` on the relevant node |
| Observability disk full | Check `/mnt/observability` usage — consider increasing block storage |
