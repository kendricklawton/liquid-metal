# Liquid Metal — Dev Guide

Local development setup. Follow top to bottom on a fresh machine.

---

## Prerequisites

### Tools

```bash
# Arch Linux
sudo pacman -S rustup go-task docker

# Debian/Ubuntu
# curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
# https://taskfile.dev/installation/

rustup default stable
cargo install cargo-release
```

> `~/.cargo/bin` must be on your `PATH`. Rustup adds this automatically; if not, add `export PATH="$PATH:$HOME/.cargo/bin"` to your shell rc file.

### Environment Variables

```bash
cp .env.example .env
```

Fill in the required values:

| Variable               | Required | Notes                                                                              |
|------------------------|----------|------------------------------------------------------------------------------------|
| `DATABASE_URL`         | Yes      | Pre-filled for local Docker Postgres                                               |
| `NATS_URL`             | Yes      | Pre-filled for local Docker NATS                                                   |
| `INTERNAL_SECRET`      | Yes      | Any string for local dev (`change-me-in-production` is fine)                       |
| `OIDC_ISSUER`          | Yes      | Your Zitadel instance URL (e.g. `https://xxx.us1.zitadel.cloud`)                  |
| `OIDC_CLI_CLIENT_ID`   | Yes      | From Zitadel: Projects → Liquid-Metal → Applications → flux-cli                   |
| `OIDC_WEB_CLIENT_ID`   | Yes      | From Zitadel: Projects → Liquid-Metal → Applications → web-dashboard              |
| `GCP_KMS_KEY`          | Yes      | Full Cloud KMS cryptoKey resource name (see RUNBOOK First-Time Setup)              |
| `GCP_KMS_CREDENTIALS`  | Yes      | Path to KMS service account JSON (e.g. `keys/kms-dev.json`)                       |
| `OBJECT_STORAGE_*`     | Yes      | Pre-filled for local MinIO                                                        |
| `COOKIE_SECRET`        | No       | Optional for dev — auto-generated with warning                                    |

> First time? See the **First-Time Setup** section in `RUNBOOK.md` for Zitadel OIDC app creation, GCP KMS key ring provisioning, and `CERT_DEK_WRAPPED` generation.

Everything else has safe defaults. See `.env.example` for the full list.

---

## Linux / Metal Setup (one-time)

Required for Metal (Firecracker) deploys. Skip if you only need Liquid (Wasm).

### 1. Metal infrastructure

```bash
sudo task metal:setup
```

Creates br0 bridge, downloads Firecracker + jailer + guest kernel, sets up NAT, creates artifact dir. Fails fast if KVM isn't available.

### 2. Base Alpine template

Builds the ext4 rootfs template and uploads to MinIO:

```bash
sudo task metal:build-template
```

Copy the `BASE_IMAGE_KEY` and `BASE_IMAGE_SHA256` values it prints into your `.env`.

> Requires `mc` (MinIO client) with alias `local` configured:
> `mc alias set local http://localhost:9000 <access-key> <secret-key>`

### 3. Security / jailer setup (optional)

```bash
sudo task security:setup
```

Add the printed values to `.env`. Only needed if you want jailed VMs (`USE_JAILER=true`).

### Verify

```bash
ls /dev/kvm                          # KVM available
ip link show br0                     # bridge UP
firecracker --version                # v1.9.0
ls /opt/firecracker/vmlinux          # guest kernel present
ls /var/lib/liquid-metal/artifacts   # artifact dir exists
```

> br0, iptables, and ip_forward are not persistent across reboots. Re-run `sudo task metal:setup` after each reboot.

---

## Running the Dev Environment

### Tab 1 — Docker infrastructure

```bash
task up           # Postgres (:5432), NATS (:4222), MinIO (:9000)
task migrate      # first run only, or after pulling new migrations
```

### Tab 2 — API

```bash
task dev:api
```

Wait for: `api listening bind=0.0.0.0:7070`

### Tab 3 — Daemon

```bash
sudo -E task dev:daemon
```

Wait for: `TAP index set initialized from DB`

> **Wasm-only (no sudo):** `DAEMON_PID_FILE=/tmp/lm.pid NODE_ENGINE=liquid task dev:daemon`

### Tab 4 — Web Dashboard (optional)

```bash
task dev:web
```

Browse to `http://localhost:3000`

### Tab 5 — Proxy (optional)

Only needed for slug-based routing (`{slug}.localhost:8080`).

```bash
task dev:proxy
```

---

## Deploy a Service

### Install the CLI

```bash
task install:cli
flux --help
```

### Authenticate

```bash
export API_URL=http://localhost:7070
flux login                    # OIDC device flow → Zitadel
flux login --invite <code>    # first-time signup with invite code
```

### Deploy (Liquid / Wasm)

```bash
cd ~/repos/liquid-metal-templates/rust/liquid/markdown-renderer
flux init       # creates project, writes liquid-metal.toml
flux deploy     # builds .wasm → uploads to MinIO → daemon runs it
```

### Deploy (Metal / Firecracker)

```bash
cd ~/repos/liquid-metal-templates/rust/metal/some-app
flux init       # select engine: metal, set port
flux deploy     # builds musl binary → uploads → daemon builds rootfs → boots VM
```

### Verify

```bash
flux services                        # list running services
curl http://<upstream_addr>/         # hit the service directly
flux logs <service-slug> --follow    # tail logs
```

---

## Day-to-Day

```bash
flux services                     # list services in active workspace
flux stop <service>               # stop a running service
flux restart <service>            # restart a service
flux env set <service> KEY=VALUE  # set an env var (encrypted at rest)
flux logs <service> --follow      # live tail logs
flux deploy                       # redeploy after code changes
```

After changing API or daemon source: Ctrl+C the relevant tab and rerun.
After changing CLI source: `task install:cli`

---

## Testing

### Unit tests (no infra needed)

```bash
cargo test --workspace
```

### Integration tests (requires Docker infra)

```bash
DATABASE_URL=postgres://postgres:postgres@localhost:5432/liquidmetal \
INTERNAL_SECRET=test-secret \
cargo test -p api --test api -- --include-ignored
```

### API smoke tests

```bash
curl -s http://localhost:7070/healthz | jq
```

### Swagger UI

| Resource       | URL                                       |
|----------------|-------------------------------------------|
| Swagger UI     | `http://localhost:7070/docs`               |
| OpenAPI JSON   | `http://localhost:7070/docs/openapi.json`  |

---

## Releasing

```bash
task release:dry-run             # preview
task release:patch               # 0.1.0 → 0.1.1
task release:minor               # 0.1.0 → 0.2.0
task release:major               # 0.1.0 → 1.0.0
```

`cargo release` bumps `Cargo.toml`, commits, tags, pushes → GitHub Actions builds + publishes.

---

## Stopping

```bash
task down          # stop Docker containers
# Ctrl+C in API, daemon, web, proxy tabs
```

---

## Troubleshooting

| Symptom                                    | Fix                                                                                                  |
|--------------------------------------------|------------------------------------------------------------------------------------------------------|
| `flux: command not found`                  | Add `$HOME/.cargo/bin` to `PATH`, rerun `task install:cli`                                           |
| `flux login` fails                         | Check `OIDC_CLI_CLIENT_ID` + `OIDC_ISSUER` in `.env`. Is the API running?                           |
| `error: fetch CLI config`                  | CLI can't reach the API — `export API_URL=http://localhost:7070`                                     |
| `unauthorized_client` on `flux login`      | Zitadel CLI app missing `device_code` grant type                                                     |
| API: `GCP_KMS_KEY is required`             | See RUNBOOK First-Time Setup                                                                         |
| API: `CERT_DEK_WRAPPED not set`            | See RUNBOOK First-Time Setup                                                                         |
| Daemon: `Permission denied` on PID file    | Run with `sudo -E task dev:daemon`                                                                   |
| Daemon: `Address already in use`           | Health port collision — Taskfile sets `:9091`, check `HEALTH_PORT`                                   |
| Daemon: `br0` / artifact warnings          | Run `sudo task metal:setup` first                                                                    |
| Status stuck at `provisioning`             | Check daemon logs. Is the daemon running?                                                            |
| `flux deploy` upload fails                 | MinIO running? `task up`. Check `http://localhost:9001`                                              |
| Web: `INTERNAL_SECRET not set`             | Add `INTERNAL_SECRET=change-me-in-production` to `.env`                                              |
| Web login redirects wrong                  | Set `WEB_PUBLIC_URL=http://localhost:3000` in `.env`                                                 |
| OIDC discovery fails                       | Check `OIDC_ISSUER` is reachable, no trailing slash                                                  |
