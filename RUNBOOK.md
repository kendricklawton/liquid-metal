# Liquid Metal — Runbook

Everything you need to develop, deploy, and operate Liquid Metal.

---

## Local Development

### Prerequisites

```bash
# Arch Linux
sudo pacman -S rustup go-task docker

# Debian/Ubuntu
# curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
# https://taskfile.dev/installation/

rustup default stable
cargo install cargo-release
```

### Environment Variables

```bash
cp .env.example .env
```

| Variable | Required | Notes |
|----------|----------|-------|
| `DATABASE_URL` | Yes | Pre-filled for local Docker Postgres |
| `NATS_URL` | Yes | Pre-filled for local Docker NATS |
| `INTERNAL_SECRET` | Yes | Any string for local dev |
| `OIDC_ISSUER` | Yes | Your Zitadel instance URL |
| `OIDC_CLI_CLIENT_ID` | Yes | From Zitadel native app |
| `OIDC_WEB_CLIENT_ID` | Yes | From Zitadel web app |
| `VAULT_ADDR` | Yes | Pre-filled (`http://localhost:8200`) |
| `VAULT_TOKEN` | Yes | Pre-filled (`dev-root-token`) |
| `OBJECT_STORAGE_*` | Yes | Pre-filled for local MinIO |
| `COOKIE_SECRET` | No | Auto-generated with warning |

See `.env.example` for the full list.

### Running

```bash
# Tab 1 — infrastructure
task up              # Postgres, NATS, MinIO, Vault
task migrate         # first run or after new migrations

# Tab 2 — API
task dev:api         # :7070

# Tab 3 — Daemon
sudo -E task dev:daemon
# Wasm-only (no sudo): DAEMON_PID_FILE=/tmp/lm.pid NODE_ENGINE=liquid task dev:daemon

# Tab 4 — Web (optional)
task dev:web         # :3000

# Tab 5 — Proxy (optional)
task dev:proxy       # :8080
```

### Metal Setup (one-time, Linux only)

Skip if you only need Liquid (Wasm).

```bash
sudo task metal:setup          # br0, Firecracker, NAT, kernel
sudo task metal:build-template # Alpine rootfs → MinIO
sudo task security:setup       # jailer (optional)
```

> br0 and iptables are not persistent across reboots. Re-run `sudo task metal:setup` after reboot.

### Deploy a Service

```bash
task install:cli
export API_URL=http://localhost:7070
flux login
flux init        # creates project, writes liquid-metal.toml
flux deploy      # build → upload → provision → URL
```

### Testing

```bash
task test                   # unit tests (no infra needed)
task test:api:integration   # integration tests (needs task up + task migrate)
curl -s localhost:7070/healthz | jq   # smoke test
```

| Resource | URL |
|----------|-----|
| Swagger UI | `http://localhost:7070/docs` |
| OpenAPI JSON | `http://localhost:7070/docs/openapi.json` |

### Day-to-Day

```bash
flux services                     # list services
flux stop <service>               # stop
flux restart <service>            # restart
flux env set <service> KEY=VALUE  # set env var
flux env set --project <id> KEY=VALUE  # set project-level env var
flux logs <service> --follow      # tail logs
flux deploy                       # redeploy
```

After changing source: Ctrl+C the relevant tab and rerun. After changing CLI: `task install:cli`.

### Releasing

```bash
task release:patch    # 0.1.0 → 0.1.1
task release:minor    # 0.1.0 → 0.2.0
task release:major    # 0.1.0 → 1.0.0
```

### Stopping

```bash
task down    # stop Docker containers
```

---

## First-Time Setup (External Services)

### Zitadel (OIDC)

Two OIDC apps under one Zitadel project. Both public clients (no secret).

**CLI** (`Liquid-Metal-Native`): Native type, device code grant, no redirect URIs.

**Web** (`Liquid-Metal-Web-{Dev,Prod}`): Web type, PKCE, redirect = `{WEB_PUBLIC_URL}/auth/callback`. One app per environment.

```bash
OIDC_ISSUER=https://your-instance.us1.zitadel.cloud
OIDC_CLI_CLIENT_ID=<native-app-client-id>
OIDC_WEB_CLIENT_ID=<web-app-client-id>
```

### Vault (Secrets)

Local dev: Docker dev server via `task up`. Production: Nomad job on gateway with GCP KMS auto-unseal.

### GCP (State + Backups)

**Local:** `gcloud auth application-default login` — no JSON keys.

**CI:** Workload Identity Federation (configured in `backups.tf`).

**Nodes:** JSON SA key via cloud-init for backup crons.

```bash
# One-time: create Terraform state bucket
gcloud storage buckets create gs://liquid-metal-tfstate-dev \
  --project your-gcp-project --location us-central1 --uniform-bucket-level-access
gcloud storage buckets update gs://liquid-metal-tfstate-dev --versioning
```

### ACME Account Key

First API start registers with Let's Encrypt and prints the key to logs. Save it:

```bash
ACME_ACCOUNT_KEY={"id":"...","key_pkcs8":"...","directory":"..."}
```

---

## Production

### Provisioning

```bash
cd infra/terraform
cp terraform.tfvars.example terraform.tfvars
terraform init -backend-config="bucket=your-tfstate-bucket"
terraform plan && terraform apply
```

Creates: 4 Hivelocity servers, private VLAN, Cloudflare DNS, Tailscale keys, GCS backup buckets.

**Post-provisioning:**
1. Enable Wasabi storage from Hivelocity portal
2. Verify VLAN connectivity between nodes
3. Run migrations: `nomad job run infra/nomad/migrate.nomad.hcl`

### Deploy Order

```bash
nomad job run infra/nomad/migrate.nomad.hcl
nomad job run infra/nomad/vault.nomad.hcl
nomad job run infra/nomad/api.nomad.hcl
nomad job run infra/nomad/proxy.nomad.hcl
nomad job run infra/nomad/daemon-metal.nomad.hcl
nomad job run infra/nomad/daemon-liquid.nomad.hcl
```

### Backups (daily, UTC)

| Time | What | From → To |
|------|------|-----------|
| 02:30 | Vault Raft snapshot | gateway → GCS |
| 03:00 | Postgres pg_dump | node-db → GCS |
| 03:00 | Nomad state | each node → GCS |
| 03:30 | VictoriaMetrics | gateway → GCS |
| 04:00 | VictoriaLogs | gateway → GCS |
| 04:30 | Wasabi artifacts | gateway → GCS |

### Observability (Tailscale only)

| Service | URL |
|---------|-----|
| Grafana | `http://{prefix}-gateway:3001` |
| VictoriaMetrics | `http://{prefix}-gateway:8428` |
| VictoriaLogs | `http://{prefix}-gateway:9428` |

---

## Troubleshooting

### Local Dev

| Symptom | Fix |
|---------|-----|
| `flux: command not found` | `task install:cli`, add `~/.cargo/bin` to PATH |
| `flux login` fails | Check `OIDC_CLI_CLIENT_ID` + `OIDC_ISSUER`. API running? |
| `error: fetch CLI config` | `export API_URL=http://localhost:7070` |
| `unauthorized_client` | Zitadel CLI app missing `device_code` grant |
| `VAULT_ADDR not set` | `task up` starts Vault. Check `.env` |
| Daemon: `Permission denied` | `sudo -E task dev:daemon` |
| Daemon: `Address already in use` | Check `HEALTH_PORT` (default `:9091`) |
| Daemon: `br0` warnings | `sudo task metal:setup` |
| Stuck at `provisioning` | Is the daemon running? Check daemon logs |
| Upload fails | `task up`. MinIO running? Check `:9001` |
| Web login redirects wrong | `WEB_PUBLIC_URL=http://localhost:3000` |
| OIDC discovery fails | Check `OIDC_ISSUER` — reachable? No trailing slash? |

### Production

| Symptom | Check |
|---------|-------|
| Can't reach services | `ssh gateway systemctl status haproxy` |
| TLS cert expired | `ssh gateway certbot certificates` |
| Deploys stuck | `ssh gateway systemctl status nomad` |
| Events not delivering | `ssh gateway systemctl status nats` |
| DB errors | `ssh node-db systemctl status pgbouncer` |
| No metrics/logs | `ssh gateway systemctl status victoriametrics victorialogs` |
| Backup failed | `/var/log/backups/*.log` on the relevant node |

---

## Incident Response

### Bridge Failure (all VMs unreachable)

```bash
ssh node-metal
ip link show br0 && ip addr show br0

# Fix
sudo ip link set br0 up
sudo ip addr add 172.16.0.1/16 dev br0
for tap in /sys/class/net/tap*/; do
  sudo ip link set "$(basename "$tap")" master br0
done

# Nuclear: restart daemon
sudo systemctl restart nomad
```

### Single VM Unreachable

```bash
ssh node-metal
ip link show tap42
sudo ip link set tap42 up
sudo ip link set tap42 master br0
```

### Daemon PID Lock

```bash
pgrep -f liquid-metal-daemon    # is it actually running?
sudo rm /run/liquid-metal-daemon.pid
sudo systemctl restart nomad
```

### Disk Pressure

```bash
ssh node-metal
df -h /var/lib/liquid-metal/artifacts
sudo systemctl restart nomad    # triggers orphan cleanup
```

---

## Operational Knobs

Every tunable is an env var with a safe default.

### API

| Variable | Default | What |
|----------|---------|------|
| `DATABASE_POOL_SIZE` | `16` | Postgres pool ceiling |
| `DB_POOL_TIMEOUT_SECS` | `5` | Max wait for DB connection |
| `OUTBOX_POLL_SECS` | `1` | Outbox poller interval |
| `BILLING_INTERVAL_SECS` | `60` | Liquid usage aggregation |
| `RATE_LIMIT_AUTH_RPM` | `10` | Per-IP auth rate limit |
| `RATE_LIMIT_API_RPM` | `60` | Per-IP API rate limit |
| `RATE_LIMIT_BFF_RPM` | `120` | Per-user BFF rate limit (via X-On-Behalf-Of) |

### Daemon

| Variable | Default | What |
|----------|---------|------|
| `NODE_MAX_CONCURRENT_PROVISIONS` | `8` | Parallel provision cap |
| `IDLE_TIMEOUT_SECS` | `300` | Stop idle Metal (0 = disabled) |
| `VM_CRASH_CHECK_INTERVAL_SECS` | `10` | Check Firecracker PIDs |
| `WASM_TIMEOUT_SECS` | `30` | Per-request Wasm timeout |
| `WASM_MAX_CONCURRENT_REQUESTS` | `64` | Per-service Wasm concurrency |
| `STARTUP_PROBE_TIMEOUT_SECS` | `30` | VM startup probe deadline |

### Proxy

| Variable | Default | What |
|----------|---------|------|
| `ROUTE_CACHE_RECONCILE_SECS` | `60` | Cache DB reconciliation |
| `PULSE_DEBOUNCE_SECS` | `30` | Traffic pulse debounce |

### Resource Quotas (per VM)

| Variable | Default | What |
|----------|---------|------|
| `QUOTA_DISK_READ_BPS` | `104857600` | 100 MB/s |
| `QUOTA_DISK_WRITE_BPS` | `104857600` | 100 MB/s |
| `QUOTA_NET_INGRESS_KBPS` | `100000` | 100 Mbps |
| `QUOTA_NET_EGRESS_KBPS` | `100000` | 100 Mbps |
