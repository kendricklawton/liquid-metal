# Liquid Metal — Runbook

Production operations, external service setup, and incident response.

> For local development, see `DEV.md`.

---

## First-Time Setup

One-time setup steps for external services. Run these before your first `task dev:api`.

### Zitadel (OIDC Authentication)

Liquid Metal uses Zitadel for identity. You need **two** OIDC applications under the same project — one for the CLI (device flow) and one for the web dashboard (authorization code + PKCE). Both are **public clients** (no client secret) and share one `OIDC_ISSUER`.

#### Prerequisites

1. A Zitadel instance (cloud or self-hosted).
2. A project named `liquid-metal` (or whatever you prefer).

> Zitadel Console → Projects → **+ New Project** → name it `Liquid-Metal`.

#### Application 1: `Liquid-Metal-Native` (CLI)

Used by the `flux` CLI for device-flow authentication (RFC 8628). The user sees a URL + code in their terminal and completes login in a browser.

**Create:**

1. Zitadel Console → Projects → `Liquid-Metal` → **+ New Application**
2. **Name**: `Liquid-Metal-Native`
3. **Type**: Native
4. **Authentication Method**: None (public client — PKCE is not used for device flow)

**Configure:**

| Setting | Value |
|---------|-------|
| Grant Types | `urn:ietf:params:oauth:grant-type:device_code` (Device Authorization) |
| Response Types | (none needed for device flow) |
| Redirect URIs | (none — device flow doesn't redirect) |
| Post-logout URIs | (none) |
| Auth Method | None |
| Token Endpoint Auth | None |

**Scopes:** `openid profile email`

Copy the **Client ID** → set as `OIDC_CLI_CLIENT_ID` in `.env`:

```bash
OIDC_ISSUER=https://your-instance.us1.zitadel.cloud
OIDC_CLI_CLIENT_ID=<client-id-from-above>
```

The API discovers `device_authorization_endpoint`, `token_endpoint`, `userinfo_endpoint`, and `revocation_endpoint` automatically from `OIDC_ISSUER/.well-known/openid-configuration`.

#### Application 2: `Liquid-Metal-Web` (Dashboard)

Used by the web dashboard for browser-based login via Authorization Code + PKCE. The user clicks "Sign In", gets redirected to Zitadel, and comes back to `/auth/callback`.

**Create:**

1. Zitadel Console → Projects → `Liquid-Metal` → **+ New Application**
2. **Name**: `Liquid-Metal-Web`
3. **Type**: Web
4. **Authentication Method**: PKCE (no client secret)

**Create one app per environment.** Each environment gets its own Zitadel application with locked-down redirect URIs. Never mix `http://localhost` and `https://prod` in the same app — an attacker who controls a redirect URI can steal authorization codes.

**`Liquid-Metal-Web-Dev` (local development):**

| Setting | Value |
|---------|-------|
| Grant Types | Authorization Code |
| Response Types | Code |
| Redirect URIs | `http://localhost:3000/auth/callback` |
| Post-logout URIs | `http://localhost:3000` |
| Auth Method | None (PKCE only) |
| Code Challenge Method | S256 |
| Token Endpoint Auth | None |

> `http://` is acceptable here because it's localhost only. Zitadel allows it for development.

**`Liquid-Metal-Web-Staging` (optional):**

| Setting | Value |
|---------|-------|
| Grant Types | Authorization Code |
| Response Types | Code |
| Redirect URIs | `https://staging.liquidmetal.dev/auth/callback` |
| Post-logout URIs | `https://staging.liquidmetal.dev` |
| Auth Method | None (PKCE only) |
| Code Challenge Method | S256 |
| Token Endpoint Auth | None |

**`Liquid-Metal-Web-Prod` (production):**

| Setting | Value |
|---------|-------|
| Grant Types | Authorization Code |
| Response Types | Code |
| Redirect URIs | `https://app.liquidmetal.dev/auth/callback` |
| Post-logout URIs | `https://app.liquidmetal.dev` |
| Auth Method | None (PKCE only) |
| Code Challenge Method | S256 |
| Token Endpoint Auth | None |

> **Strict rules for production:**
> - HTTPS only — never `http://`. Zitadel enforces this for non-localhost URIs.
> - Exact match — no wildcards in redirect URIs. `https://app.liquidmetal.dev/auth/callback` only.
> - One redirect URI — don't add localhost to the prod app. If you need to debug prod auth, use staging.
> - Post-logout URI must also be HTTPS and exact.

**Scopes:** `openid profile email`

Each app gets its own Client ID. Set the correct one for the environment:

| Environment | Env var | Value |
|-------------|---------|-------|
| Dev | `OIDC_WEB_CLIENT_ID` | Client ID from `Liquid-Metal-Web-Dev` |
| Staging | `OIDC_WEB_CLIENT_ID` | Client ID from `Liquid-Metal-Web-Staging` |
| Production | `OIDC_WEB_CLIENT_ID` | Client ID from `Liquid-Metal-Web-Prod` |

**.env (local dev):**

```bash
OIDC_ISSUER=https://your-instance.us1.zitadel.cloud
OIDC_WEB_CLIENT_ID=<client-id-from-Liquid-Metal-Web-Dev>
WEB_PUBLIC_URL=http://localhost:3000
```

**.env (production):**

```bash
OIDC_ISSUER=https://your-instance.us1.zitadel.cloud
OIDC_WEB_CLIENT_ID=<client-id-from-Liquid-Metal-Web-Prod>
WEB_PUBLIC_URL=https://app.liquidmetal.dev
COOKIE_SECRET=<openssl rand -hex 32>
```

> `WEB_PUBLIC_URL` is used to construct the `redirect_uri` sent to Zitadel. It must exactly match what's registered in the app's Redirect URIs. If they don't match, Zitadel returns `redirect_uri_mismatch`.

#### Why Two Applications?

| | CLI (`Liquid-Metal-Native`) | Web (`Liquid-Metal-Web-*`) |
|---|---|---|
| **Flow** | Device Authorization (RFC 8628) | Authorization Code + PKCE (RFC 7636) |
| **User experience** | "Go to this URL and enter this code" | Browser redirect → Zitadel → redirect back |
| **Redirect URI** | None | Exact match per environment |
| **Runs on** | Terminal (headless OK) | Browser |
| **Client type** | Native / public | Web / public (PKCE, no secret) |
| **Apps per env** | 1 (no redirect URI to protect) | 1 per environment (dev, staging, prod) |

Both are public clients — no client secret is stored anywhere. The CLI uses device flow because it can't open a redirect in-process. The web uses Authorization Code + PKCE because it's the standard browser auth flow.

**Why one web app per environment?** A Zitadel app's redirect URIs define where authorization codes can be sent. If `http://localhost:3000/auth/callback` and `https://app.liquidmetal.dev/auth/callback` are on the same app, an attacker who controls localhost on a user's machine can intercept prod authorization codes. Separate apps mean a dev Client ID can only redirect to dev, and a prod Client ID can only redirect to prod.

#### Shared Configuration

Both applications share:

- **OIDC Issuer** (`OIDC_ISSUER`): same Zitadel instance
- **Project**: same `Liquid-Metal` project
- **Scopes**: `openid profile email`
- **Token format**: Zitadel opaque tokens (userinfo endpoint is authoritative)

The API only needs the CLI client ID (`OIDC_CLI_CLIENT_ID`). The web crate needs its own (`OIDC_WEB_CLIENT_ID`). Both use the same issuer for OIDC discovery.

#### Zitadel Troubleshooting

| Symptom | Fix |
|---------|-----|
| `flux login` shows "invalid client" | Check `OIDC_CLI_CLIENT_ID` matches the Native app's Client ID |
| Web login redirects to error page | Check `OIDC_WEB_CLIENT_ID` matches the correct **environment's** Web app Client ID |
| Callback returns `redirect_uri_mismatch` | `WEB_PUBLIC_URL` + `/auth/callback` must exactly match a Redirect URI registered on the Zitadel app. Check: right app for this env? Trailing slash? HTTP vs HTTPS? |
| "PKCE required" error | Ensure the Web app has Authentication Method set to PKCE, not Basic or POST |
| Device flow returns `unauthorized_client` | Ensure the Native app has Device Authorization grant type enabled |
| `OIDC discovery failed` | Check `OIDC_ISSUER` URL is reachable and has no trailing slash |
| Web sessions lost across restarts | Set `COOKIE_SECRET` in `.env` (`openssl rand -hex 32`) |
| Tokens not revoking on logout | Check that `revocation_endpoint` exists in OIDC discovery (Zitadel supports it by default) |
| Prod login works but staging doesn't | Each environment needs its own Zitadel app with its own Client ID and redirect URIs. Don't reuse the prod app's Client ID for staging |
| `http://` rejected on non-localhost | Zitadel only allows `http://` for `localhost` redirect URIs. All other environments must use `https://` |
| `unauthorized_client` on `flux login` | In Zitadel, confirm the CLI app has `urn:ietf:params:oauth:grant-type:device_code` grant type enabled |
| `error: fetch CLI config` | CLI can't reach the API. Set `API_URL=http://localhost:7070` for local dev                              |

### GCP Cloud KMS (Envelope Encryption)

Service environment variables are encrypted at rest using envelope encryption. Each workspace gets a data encryption key (DEK) that is wrapped by a GCP Cloud KMS key encryption key (KEK).

#### Prerequisites

- A GCP project (one project works for all environments)
- `gcloud` CLI installed and authenticated

```bash
gcloud auth login
gcloud config set project your-gcp-project
```

#### Enable the KMS API

```bash
gcloud services enable cloudkms.googleapis.com
```

#### Create Key Rings (one per environment)

```bash
# Development
gcloud kms keyrings create liquid-metal-dev \
  --location us-central1

# Production
gcloud kms keyrings create liquid-metal-prod \
  --location us-central1
```

#### Create Encryption Keys

```bash
# Development
gcloud kms keys create envelope \
  --keyring liquid-metal-dev \
  --location us-central1 \
  --purpose encryption \
  --protection-level software

# Production
gcloud kms keys create envelope \
  --keyring liquid-metal-prod \
  --location us-central1 \
  --purpose encryption \
  --protection-level software
```

#### Create a Service Account (dev)

```bash
gcloud iam service-accounts create lm-kms-dev \
  --display-name "Liquid Metal KMS (dev)"

gcloud kms keyrings add-iam-policy-binding liquid-metal-dev \
  --location us-central1 \
  --member "serviceAccount:lm-kms-dev@your-gcp-project.iam.gserviceaccount.com" \
  --role roles/cloudkms.cryptoKeyEncrypterDecrypter

mkdir -p keys
gcloud iam service-accounts keys create keys/kms-dev.json \
  --iam-account lm-kms-dev@your-gcp-project.iam.gserviceaccount.com
```

> `keys/` is already in `.gitignore`.

#### Set in `.env`

```bash
GCP_KMS_KEY=projects/your-gcp-project/locations/us-central1/keyRings/liquid-metal-dev/cryptoKeys/envelope
# GCP_KMS_CREDENTIALS is read by the API — separate from GOOGLE_APPLICATION_CREDENTIALS
# which Terraform uses for its tfstate bucket SA.
GCP_KMS_CREDENTIALS=keys/kms-dev.json
```

#### Verify It Works

```bash
# Quick test — encrypt and decrypt a string via gcloud
echo -n "hello" | gcloud kms encrypt \
  --keyring liquid-metal-dev \
  --location us-central1 \
  --key envelope \
  --plaintext-file - \
  --ciphertext-file - | base64

# If this succeeds, the API will be able to use KMS
```

#### Generate `CERT_DEK_WRAPPED`

The API requires a KMS-wrapped certificate DEK for encrypting TLS private keys at rest. Generate it once per environment:

```bash
openssl rand -out /tmp/cert_dek.bin 32
gcloud kms encrypt \
  --keyring liquid-metal-dev \
  --location us-central1 \
  --key envelope \
  --plaintext-file /tmp/cert_dek.bin \
  --ciphertext-file /tmp/cert_dek.enc
echo "CERT_DEK_WRAPPED=$(base64 -w0 /tmp/cert_dek.enc)"
rm /tmp/cert_dek.bin /tmp/cert_dek.enc
```

Copy the output line into `.env`. The plaintext DEK only exists in memory at runtime — it's never persisted to disk.

#### `ACME_ACCOUNT_KEY`

On first API start, an ACME account is registered with Let's Encrypt and printed to logs as a `WARN`. Copy the JSON blob into `.env` to persist it across restarts:

```bash
ACME_ACCOUNT_KEY={"id":"https://acme-v02.api.letsencrypt.org/acme/acct/...","key_pkcs8":"...","directory":"..."}
```

Without this, the API registers a new Let's Encrypt account on every restart, wasting rate limits. In local dev this is harmless but noisy.

#### Production KMS Setup

Create a dedicated service account with access only to the prod key ring:

```bash
gcloud iam service-accounts create lm-kms-prod \
  --display-name "Liquid Metal KMS (prod)"

gcloud kms keyrings add-iam-policy-binding liquid-metal-prod \
  --location us-central1 \
  --member "serviceAccount:lm-kms-prod@your-gcp-project.iam.gserviceaccount.com" \
  --role roles/cloudkms.cryptoKeyEncrypterDecrypter
```

Download the key JSON locally (you'll upload it to Nomad, then delete the local copy):

```bash
gcloud iam service-accounts keys create /tmp/lm-kms-prod.json \
  --iam-account lm-kms-prod@your-gcp-project.iam.gserviceaccount.com
```

Store it in Nomad Variables alongside the other API secrets. No key files live on the worker nodes — Nomad renders it to a tmpfs-backed secrets dir that only the task can read, and it's deleted when the allocation stops.

```bash
nomad var put secrets/api \
  gcp_sa_json="$(cat /tmp/lm-kms-prod.json)" \
  gcp_kms_key="projects/your-gcp-project/locations/us-central1/keyRings/liquid-metal-prod/cryptoKeys/envelope" \
  # ... other existing secrets ...

# Delete the local copy
rm /tmp/lm-kms-prod.json
```

The Nomad job (`api.nomad.hcl`) renders the JSON to `secrets/gcp-sa.json` and sets `GOOGLE_APPLICATION_CREDENTIALS` to point at it.

### GCS Terraform State Bucket

Terraform stores its state in a GCS bucket. Each environment (dev/prod) gets its own bucket and service account with minimal permissions.

#### Enable the Storage API

```bash
gcloud services enable storage.googleapis.com
```

#### Create State Buckets

```bash
# Development
gcloud storage buckets create gs://liquid-metal-tfstate-dev \
  --project your-gcp-project \
  --location us-central1 \
  --uniform-bucket-level-access

# Production
gcloud storage buckets create gs://liquid-metal-tfstate-prod \
  --project your-gcp-project \
  --location us-central1 \
  --uniform-bucket-level-access
```

Enable versioning so you can recover from a bad `terraform apply`:

```bash
gcloud storage buckets update gs://liquid-metal-tfstate-dev --versioning
gcloud storage buckets update gs://liquid-metal-tfstate-prod --versioning
```

#### Create Service Accounts

Each SA gets `roles/storage.objectAdmin` on its own bucket — nothing else.

```bash
# -- Dev --
gcloud iam service-accounts create lm-tfstate-dev \
  --display-name "Liquid Metal Terraform State (dev)"

gcloud storage buckets add-iam-policy-binding gs://liquid-metal-tfstate-dev \
  --member "serviceAccount:lm-tfstate-dev@your-gcp-project.iam.gserviceaccount.com" \
  --role roles/storage.objectAdmin

mkdir -p keys
gcloud iam service-accounts keys create keys/tfstate-dev.json \
  --iam-account lm-tfstate-dev@your-gcp-project.iam.gserviceaccount.com

# -- Prod --
gcloud iam service-accounts create lm-tfstate-prod \
  --display-name "Liquid Metal Terraform State (prod)"

gcloud storage buckets add-iam-policy-binding gs://liquid-metal-tfstate-prod \
  --member "serviceAccount:lm-tfstate-prod@your-gcp-project.iam.gserviceaccount.com" \
  --role roles/storage.objectAdmin

gcloud iam service-accounts keys create keys/tfstate-prod.json \
  --iam-account lm-tfstate-prod@your-gcp-project.iam.gserviceaccount.com
```

> `keys/` is already in `.gitignore`.

#### Set in `.env`

```bash
# Terraform reads GOOGLE_APPLICATION_CREDENTIALS for GCS backend auth.
# This is separate from GCP_KMS_CREDENTIALS (used by the API for KMS).
GOOGLE_APPLICATION_CREDENTIALS=keys/tfstate-dev.json
```

#### Initialize Terraform

```bash
cd infra/terraform

# Dev
terraform init -backend-config="bucket=liquid-metal-tfstate-dev"

# Prod (use a separate workspace or directory)
terraform init -backend-config="bucket=liquid-metal-tfstate-prod"
```

#### Production Terraform State

For production applies from CI or a dedicated ops machine, use the prod SA key:

```bash
GOOGLE_APPLICATION_CREDENTIALS=keys/tfstate-prod.json \
  terraform -chdir=infra/terraform plan
```

> Never commit key files. For CI (GitHub Actions), store the JSON as a repository secret and write it to a temp file in the workflow.

### GCS Backup Buckets (Disaster Recovery)

Each environment gets its own backup bucket. Backup scripts use path prefixes to organize by service:

```
gs://liquid-metal-backups-{env}/
  postgres/          # pg_dump snapshots (from node-db)
  wasabi-artifacts/  # rclone sync of Wasabi object storage
  victoriametrics/   # VM snapshots
  victorialogs/      # VL tar archives
  nomad/             # Nomad state exports (per node)
```

#### Create Buckets

```bash
# Development
gcloud storage buckets create gs://liquid-metal-backups-dev \
  --project your-gcp-project \
  --location us-central1 \
  --uniform-bucket-level-access

gcloud storage buckets update gs://liquid-metal-backups-dev --versioning

# Production
gcloud storage buckets create gs://liquid-metal-backups-prod \
  --project your-gcp-project \
  --location us-central1 \
  --uniform-bucket-level-access

gcloud storage buckets update gs://liquid-metal-backups-prod --versioning
```

#### Set Lifecycle Rules (optional)

Auto-delete objects older than 90 days to control costs:

```bash
cat > /tmp/lifecycle.json << 'EOF'
{
  "rule": [
    {
      "action": { "type": "Delete" },
      "condition": { "age": 90 }
    }
  ]
}
EOF

gcloud storage buckets update gs://liquid-metal-backups-dev \
  --lifecycle-file=/tmp/lifecycle.json
gcloud storage buckets update gs://liquid-metal-backups-prod \
  --lifecycle-file=/tmp/lifecycle.json

rm /tmp/lifecycle.json
```

#### Create Service Accounts

Each SA gets `roles/storage.objectUser` on its own bucket — write-only, no delete.

```bash
# -- Dev --
gcloud iam service-accounts create lm-backups-dev \
  --display-name "Liquid Metal Backups (dev)"

gcloud storage buckets add-iam-policy-binding gs://liquid-metal-backups-dev \
  --member "serviceAccount:lm-backups-dev@your-gcp-project.iam.gserviceaccount.com" \
  --role roles/storage.objectUser

mkdir -p keys
gcloud iam service-accounts keys create keys/backups-dev.json \
  --iam-account lm-backups-dev@your-gcp-project.iam.gserviceaccount.com

# -- Prod --
gcloud iam service-accounts create lm-backups-prod \
  --display-name "Liquid Metal Backups (prod)"

gcloud storage buckets add-iam-policy-binding gs://liquid-metal-backups-prod \
  --member "serviceAccount:lm-backups-prod@your-gcp-project.iam.gserviceaccount.com" \
  --role roles/storage.objectUser

gcloud iam service-accounts keys create /tmp/lm-backups-prod.json \
  --iam-account lm-backups-prod@your-gcp-project.iam.gserviceaccount.com

# Copy prod key to NAT VPS, then delete local copy
scp /tmp/lm-backups-prod.json nat-vps:/etc/liquid-metal/gcs-backups.json
rm /tmp/lm-backups-prod.json
```

> `objectUser` (not `objectAdmin`) — backup crons only write new objects, never delete. Retention is handled by the lifecycle rule above.

The backup cron scripts on the NAT VPS set `GOOGLE_APPLICATION_CREDENTIALS=/etc/liquid-metal/gcs-backups.json` when calling `gsutil` or `rclone`. The dev key (`keys/backups-dev.json`) stays local for testing backup scripts.

### GCP Service Account Summary

All GCP usage is scoped to one shared project. Each SA has minimal permissions:

| Service Account | Purpose | Permissions | Key Location |
|---|---|---|---|
| `lm-kms-dev` | API envelope encryption (dev) | `cloudkms.cryptoKeyEncrypterDecrypter` on `liquid-metal-dev` key ring | `keys/kms-dev.json` → `GCP_KMS_CREDENTIALS` |
| `lm-kms-prod` | API envelope encryption (prod) | `cloudkms.cryptoKeyEncrypterDecrypter` on `liquid-metal-prod` key ring | Nomad Variables → tmpfs |
| `lm-tfstate-dev` | Terraform state (dev) | `storage.objectAdmin` on `liquid-metal-tfstate-dev` bucket | `keys/tfstate-dev.json` → `GOOGLE_APPLICATION_CREDENTIALS` |
| `lm-tfstate-prod` | Terraform state (prod) | `storage.objectAdmin` on `liquid-metal-tfstate-prod` bucket | `keys/tfstate-prod.json` or CI secret |
| `lm-backups-dev` | DR backups (dev) | `storage.objectUser` on `liquid-metal-backups-dev` bucket | `keys/backups-dev.json` |
| `lm-backups-prod` | DR backups (prod) | `storage.objectUser` on `liquid-metal-backups-prod` bucket | gateway node `/etc/liquid-metal/gcs-backups.json` |

> **Why two env vars?** `GOOGLE_APPLICATION_CREDENTIALS` is the standard GCP env var — Terraform, `gsutil`, and `gcloud` all read it automatically. The API uses `GCP_KMS_CREDENTIALS` instead so both can coexist in the same `.env` without conflicting. In production (Nomad), there's no conflict — each task has its own isolated env.

### First-Time Setup Checklist

After completing the steps above, your `.env` should have these values filled in:

- [ ] `OIDC_ISSUER` — Zitadel instance URL
- [ ] `OIDC_CLI_CLIENT_ID` — from the `flux-cli` Zitadel app
- [ ] `OIDC_WEB_CLIENT_ID` — from the `web-dashboard` Zitadel app
- [ ] `GCP_KMS_KEY` — full Cloud KMS cryptoKey resource name
- [ ] `GCP_KMS_CREDENTIALS` — path to KMS service account JSON (e.g. `keys/kms-dev.json`)
- [ ] `CERT_DEK_WRAPPED` — KMS-wrapped cert DEK (see [Generate `CERT_DEK_WRAPPED`](#generate-cert_dek_wrapped))
- [ ] `ACME_ACCOUNT_KEY` — saved from first API start (see [`ACME_ACCOUNT_KEY`](#acme_account_key))
- [ ] `GOOGLE_APPLICATION_CREDENTIALS` — path to Terraform state SA JSON (e.g. `keys/tfstate-dev.json`)

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

This creates: 4× Hivelocity dedicated servers (gateway, node-metal, node-liquid, node-db), Hivelocity private VLAN attached to all 4 nodes, Cloudflare DNS records (wildcard + apex → gateway public IP), Tailscale pre-auth keys for operator access, and GCS backup buckets.

**Post-provisioning (manual one-time steps):**
1. Enable Wasabi object storage from the Hivelocity portal — $5.99/TB/mo, S3-compatible
2. Verify private VLAN connectivity: `ssh gateway ping <node-db-vlan-ip>`
3. Initialize Postgres on node-db (see [Postgres Setup](#postgres-setup) below)
4. Run initial migrations: `nomad job run infra/nomad/migrate.nomad.hcl`

### Postgres Setup

Postgres runs self-hosted on node-db. First-time setup:

```bash
ssh node-db

# Install Postgres 16
apt install -y postgresql-16 pgbouncer

# Create database and app role
sudo -u postgres psql <<'EOF'
CREATE DATABASE liquidmetal;
CREATE ROLE lm_app LOGIN PASSWORD 'changeme';
GRANT ALL PRIVILEGES ON DATABASE liquidmetal TO lm_app;
EOF

# Configure PgBouncer (transaction mode, VLAN-only listen)
# /etc/pgbouncer/pgbouncer.ini
#   listen_addr = <vlan-ip>
#   listen_port = 6432
#   pool_mode = transaction

# Restrict Postgres to VLAN interface only
# /etc/postgresql/16/main/postgresql.conf
#   listen_addresses = '<vlan-ip>'
```

Backup cron runs daily at 03:00 UTC — `pg_dump | gzip | gsutil cp` to GCS backup bucket.

### Observability Access (Tailscale only)

| Service           | URL                                                               |
|-------------------|-------------------------------------------------------------------|
| Grafana           | `http://{prefix}-nat-vps:3001` (admin / `GRAFANA_ADMIN_PASSWORD`) |
| VictoriaMetrics   | `http://{prefix}-nat-vps:8428`                                    |
| VictoriaLogs      | `http://{prefix}-nat-vps:9428`                                    |

### Backup Schedule (daily, UTC)

| Time  | Backup                                                      |
|-------|-------------------------------------------------------------|
| 02:00 | certbot renewal check                                       |
| 03:00 | Postgres pg_dump → GCS (runs on node-db)                   |
| 03:00 | Nomad state → GCS (per node)                               |
| 03:30 | VictoriaMetrics snapshot → GCS                              |
| 04:00 | VictoriaLogs tar → GCS                                      |
| 04:30 | Wasabi artifacts rclone sync → GCS                          |

---

## Production Deploy Order

Schema migrations are a separate step from serving traffic — run them before rolling the new binary so the DB is ready before any instance starts.

### Option 1: Nomad batch job (recommended)

```bash
# Upload migrations to S3, then dispatch the batch job
nomad job run infra/nomad/migrate.nomad.hcl
nomad job dispatch migrate

# Then roll the services
nomad job run infra/nomad/api.nomad.hcl
nomad job run infra/nomad/proxy.nomad.hcl
nomad job run infra/nomad/daemon-metal.nomad.hcl
nomad job run infra/nomad/daemon-liquid.nomad.hcl
```

### Option 2: Direct psql

```bash
# 1. Run migrations via psql against PgBouncer
DATABASE_URL="$PGBOUNCER_URL" psql -f migrations/V*.sql

# 2. Roll services via Nomad
nomad job run infra/nomad/api.nomad.hcl
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

## Troubleshooting

> Dev troubleshooting is in `DEV.md`.

| Symptom                                    | Check                                                                                                |
|--------------------------------------------|------------------------------------------------------------------------------------------------------|
| Can't reach services externally            | HAProxy running? `ssh nat-vps systemctl status haproxy`                                              |
| TLS cert expired                           | `ssh nat-vps certbot certificates` — check renewal cron                                              |
| Deploys not running                        | Nomad server up? `ssh nat-vps systemctl status nomad`                                                |
| Events not delivering                      | NATS up? `ssh nat-vps systemctl status nats`                                                         |
| DB connection errors                       | PgBouncer up? `ssh nat-vps systemctl status pgbouncer`                                               |
| No metrics/logs                            | Check VictoriaMetrics/VictoriaLogs: `ssh nat-vps systemctl status victoriametrics victorialogs`      |
| Grafana inaccessible                       | `ssh nat-vps systemctl status grafana` — check `:3001` via Tailscale                                |
| Backup failed                              | Check `/var/log/backups/*.log` on the relevant node                                                  |
| Observability disk full                    | Check `/mnt/observability` usage — consider increasing block storage                                 |
| Bridge down — all VMs unreachable          | See [Bridge Failure](#bridge-failure-all-vms-on-node-unreachable) below                              |
| Single VM unreachable                      | See [Single VM Network Failure](#single-vm-network-failure) below                                    |
| Daemon won't start (PID lock)             | See [Daemon PID Lock](#daemon-pid-lock) below                                                        |
| Disk full on Metal node                    | See [Disk Pressure on Metal Node](#disk-pressure-on-metal-node) below                                |
| Mass provision failures                    | See [Mass Provision Failures](#mass-provision-failures) below                                        |

---

## Incident Response

### Bridge Failure (all VMs on node unreachable)

**Symptom:** Every service on a Metal node returns connection refused or times out simultaneously. Proxy health checks fail for all services assigned to that node. The daemon is still running, crash watcher shows no dead PIDs — the VMs are alive but unreachable.

**Cause:** `br0` interface went down, got misconfigured, or lost its IP. All TAP devices are attached to `br0` — if it goes down, every VM on that node loses network instantly.

**Diagnose:**

```bash
# SSH to the affected Metal node
ssh node-a-01

# 1. Check bridge state
ip link show br0
# Look for: state DOWN, NO-CARRIER, or missing entirely

# 2. Check bridge has an IP
ip addr show br0
# Should show a 172.16.x.x address

# 3. Check TAP devices are still attached
ls /sys/class/net/br0/brif/
# Should list tap0, tap1, etc.

# 4. Check if the host can reach a VM
ping -c1 172.16.0.2  # guest IP of tap0
```

**Fix — bring bridge back up:**

```bash
# If bridge exists but is DOWN
sudo ip link set br0 up

# If bridge lost its IP (check cloud-init config for the correct address)
sudo ip addr add 172.16.0.1/16 dev br0

# If TAPs got detached (rare — usually survive a bridge bounce)
# The daemon's cleanup_orphaned_taps will handle this on restart,
# but for immediate recovery:
for tap in /sys/class/net/tap*/; do
  name=$(basename "$tap")
  sudo ip link set "$name" master br0
done

# Verify connectivity is restored
ping -c1 172.16.0.2
```

**Fix — bridge is completely gone (needs full recreation):**

```bash
# Recreate from scratch (matches cloud-init / task metal:setup)
sudo ip link add br0 type bridge
sudo ip addr add 172.16.0.1/16 dev br0
sudo ip link set br0 up

# Re-attach all TAP devices
for tap in /sys/class/net/tap*/; do
  name=$(basename "$tap")
  sudo ip link set "$name" master br0
done

# Enable forwarding (should already be set, but verify)
sudo sysctl -w net.ipv4.ip_forward=1

# Verify NAT masquerade rule exists
sudo iptables -t nat -L POSTROUTING | grep 172.16.0.0
# If missing:
sudo iptables -t nat -A POSTROUTING -s 172.16.0.0/16 ! -o br0 -j MASQUERADE
```

**If VMs are unrecoverable after bridge restore:**

```bash
# Nuclear option: restart the daemon. It will:
# 1. Kill all VMs (shutdown drain)
# 2. Clean up TAPs, cgroups, eBPF filters
# 3. Re-provision from NATS on startup
sudo systemctl restart nomad  # or: nomad job run daemon-metal.nomad.hcl

# Services will re-provision automatically via the existing NATS consumer.
# Downtime: ~30-60s per service (S3 download + VM boot + startup probe).
```

**Prevention:**

- Monitor `br0` link state via node_exporter (`node_network_up{device="br0"}`)
- Alert on bridge state change in Grafana
- Nomad health check on the daemon catches this indirectly — if the daemon can't reach VMs, the health endpoint stays up but services fail. Consider adding a bridge-health check to the daemon's `/health` endpoint.

---

### Single VM Network Failure

**Symptom:** One service is unreachable while others on the same node work fine.

**Diagnose:**

```bash
ssh node-a-01

# Find the service's TAP device
psql "$DATABASE_URL" -c "SELECT slug, tap_name, upstream_addr FROM services WHERE node_id = 'node-a-01' AND status = 'running'"

# Check TAP state
ip link show tap42
# Should be UP and master br0

# Check eBPF filter is attached
tc filter show dev tap42 egress

# Check tc qdiscs
tc qdisc show dev tap42

# Try reaching the VM directly
ping -c1 172.16.0.43  # guest IP derived from TAP index
curl http://172.16.0.43:8080/  # the service port
```

**Fix:**

```bash
# If TAP is down
sudo ip link set tap42 up

# If TAP lost bridge attachment
sudo ip link set tap42 master br0

# If eBPF filter is missing (tenant isolation broken)
# Restart the daemon — it re-attaches eBPF on startup for all running VMs

# If the VM process died but daemon didn't detect it yet
# Wait for crash watcher (10s interval) or restart the daemon
```

---

### Daemon PID Lock

**Symptom:** Daemon refuses to start with `"another daemon is already running"` or similar flock error.

**Diagnose:**

```bash
# Check if another daemon is actually running
pgrep -f liquid-metal-daemon

# Check the PID file
cat /run/liquid-metal-daemon.pid
# If the PID in the file doesn't correspond to a running daemon:
ls -la /proc/$(cat /run/liquid-metal-daemon.pid)/exe 2>/dev/null
```

**Fix:**

```bash
# If no daemon is actually running (stale lock from a crash)
sudo rm /run/liquid-metal-daemon.pid

# Then restart
sudo systemctl restart nomad  # or start the daemon directly
```

---

### Disk Pressure on Metal Node

**Symptom:** Provisions fail with I/O errors. Daemon logs show `"artifact partition above 80%"` warnings.

**Diagnose:**

```bash
ssh node-a-01
df -h /var/lib/liquid-metal/artifacts

# Find largest artifact directories
du -sh /var/lib/liquid-metal/artifacts/* | sort -rh | head -20

# Check for orphaned artifacts (no matching running service)
ls /var/lib/liquid-metal/artifacts/ | wc -l
psql "$DATABASE_URL" -c "SELECT count(*) FROM services WHERE node_id = 'node-a-01' AND status IN ('running', 'provisioning')"
```

**Fix:**

```bash
# The daemon cleans orphaned artifacts on startup, but you can trigger it manually
# by restarting the daemon. Or manually remove directories for services
# that are no longer running:

# List service IDs that should exist
psql "$DATABASE_URL" -t -c "SELECT id FROM services WHERE node_id = 'node-a-01' AND status IN ('running', 'provisioning') AND deleted_at IS NULL"

# Compare against disk and remove orphans manually if urgent
```

---

### Mass Provision Failures

**Symptom:** All provisions on a node are failing. NATS consumer lag is climbing.

**Note:** The daemon now classifies failures as **Transient** (S3 timeout, DB blip — retries up to 3 times with 15s/30s backoff) or **Permanent** (SHA mismatch, startup probe timeout — no retry). Failed services store a `failure_reason` on the service row, visible via `flux services` and the web dashboard.

**Diagnose:**

```bash
# Check daemon logs for the error pattern
journalctl -u nomad -n 200 | grep -i "provision\|error\|failed"

# Common patterns:
# - "S3 download timed out" → Transient, will retry automatically
# - "sha256 mismatch" → Permanent, bad artifact — user must redeploy
# - "startup probe failed" → Permanent, binary crashes on boot
# - "cgroup limits" → cgroup v2 not enabled or /sys/fs/cgroup not writable
# - "create TAP" → TAP index exhaustion or netlink permission error

# Check failure reasons in the database
psql "$DATABASE_URL" -c "SELECT name, status, failure_reason, provision_attempts FROM services WHERE status = 'failed' ORDER BY updated_at DESC LIMIT 20"

# Check NATS consumer lag
nats consumer info provision-consumer

# Check S3 connectivity
curl -s http://your-object-storage:9000/minio/health/live

# Check cgroup v2 is mounted
mount | grep cgroup2
ls /sys/fs/cgroup/liquid-metal/
```

**Fix depends on root cause:**

```bash
# S3 unreachable: fix network/DNS to object storage endpoint
# Verify: curl the OBJECT_STORAGE_ENDPOINT from the node
# Note: transient S3 failures retry automatically (max 3 attempts)

# cgroup v2 missing: enable in kernel params
# Add to /etc/default/grub: GRUB_CMDLINE_LINUX="systemd.unified_cgroup_hierarchy=1"
# Then: update-grub && reboot

# TAP index exhaustion (>3969 VMs created without cleanup):
# This should not happen with the recycling pool, but if it does:
# Restart the daemon — init_tap_indices rebuilds from DB

# To retry a permanently failed service, the user must redeploy:
# flux deploy
```

---

## Operational Knobs

Every tunable in Liquid Metal is controlled via environment variables with safe defaults.
No recompilation required — change the value, restart the process.

### API (`crates/api`)

| Variable | Default | Unit | What it controls |
|----------|---------|------|------------------|
| `DATABASE_POOL_SIZE` | `16` | connections | Postgres connection pool ceiling |
| `DB_POOL_TIMEOUT_SECS` | `5` | seconds | Max wait for a DB connection before returning 503 |
| `WATCHDOG_INTERVAL_SECS` | `60` | seconds | How often the stuck-provisioning watchdog runs |
| `PROVISIONING_TIMEOUT_MINS` | `10` | minutes | Mark `provisioning` services as `failed` after this long |
| `OUTBOX_POLL_SECS` | `1` | seconds | How often the outbox poller checks for unsent events |
| `OUTBOX_BATCH_SIZE` | `50` | rows | Max outbox rows published per poll pass |
| `OUTBOX_STALE_MINS` | `30` | minutes | Purge unpublished outbox rows older than this |
| `BILLING_INTERVAL_SECS` | `60` | seconds | How often the billing aggregator runs (safe with multiple API instances — uses `FOR UPDATE SKIP LOCKED`) |
| `CREDIT_RESET_INTERVAL_SECS` | `3600` | seconds | How often the monthly credit reset check runs |
| `RATE_LIMIT_AUTH_RPM` | `10` | req/min | Per-IP rate limit on auth routes |
| `RATE_LIMIT_API_RPM` | `60` | req/min | Per-IP rate limit on protected API routes |

### Daemon (`crates/daemon`)

| Variable | Default | Unit | What it controls |
|----------|---------|------|------------------|
| `DATABASE_POOL_SIZE` | `8` | connections | Postgres connection pool ceiling |
| `NODE_MAX_CONCURRENT_PROVISIONS` | `8` | tasks | Semaphore cap on parallel VM/Wasm provisions |
| `IDLE_TIMEOUT_SECS` | `300` | seconds | Stop Metal services with no traffic for this long (0 = disabled) |
| `IDLE_CHECK_INTERVAL_SECS` | `60` | seconds | How often the idle checker runs |
| `PULSE_BATCH_WINDOW_SECS` | `5` | seconds | Traffic pulse accumulation window before batch DB update |
| `ORPHAN_SWEEP_INTERVAL_SECS` | `60` | seconds | How often the deleted-workspace orphan sweep runs |
| `VM_CRASH_CHECK_INTERVAL_SECS` | `10` | seconds | How often Firecracker PIDs are checked for unexpected exit |
| `USAGE_REPORT_INTERVAL_SECS` | `60` | seconds | Metal + Liquid usage event publish frequency |
| `LAG_MONITOR_INTERVAL_SECS` | `30` | seconds | How often provision consumer lag is checked |
| `PROVISION_LAG_THRESHOLD` | `50` | messages | Warn when pending provisions exceed this count |
| `PROVISION_ACK_WAIT_SECS` | `300` | seconds | NATS ack timeout for provision messages |
| `WASM_FUEL` | `1000000000` | units | Fuel budget per Wasm request (~seconds of CPU) |
| `WASM_STACK_BYTES` | `1048576` | bytes | Wasm stack size (1 MiB) |
| `WASM_MAX_RESPONSE_BYTES` | `4194304` | bytes | Max Wasm stdout capture (4 MiB) |
| `WASM_TIMEOUT_SECS` | `30` | seconds | Wall-clock timeout per Wasm request |
| `WASM_MAX_MEMORY_BYTES` | `134217728` | bytes | Per-instance Wasm linear memory cap (128 MiB) |
| `WASM_MAX_CONCURRENT_REQUESTS` | `64` | requests | Per-service Wasm concurrency cap (503 when full) |
| `WASM_QUEUE_TIMEOUT_SECS` | `5` | seconds | Wait time for concurrency permit before returning 503 |
| `CGROUP_MEMORY_HEADROOM_PCT` | `10` | % | Extra headroom above guest RAM for Firecracker process overhead |
| `CGROUP_PIDS_MAX` | `512` | PIDs | Fork-bomb protection per VM cgroup |
| `S3_DOWNLOAD_TIMEOUT_SECS` | `300` | seconds | Artifact download deadline (prevents hung S3 connections) |
| `PROVISION_TIMEOUT_SECS` | `600` | seconds | Per-provision deadline (prevents stuck tasks blocking semaphore) |
| `SHUTDOWN_DRAIN_TIMEOUT_SECS` | `30` | seconds | Max time to drain in-flight tasks on graceful shutdown |
| `HEALTH_PORT` | `9090` | port | Daemon health check HTTP endpoint |
| `DAEMON_PID_FILE` | `/run/liquid-metal-daemon.pid` | path | Lock file to prevent dual-daemon on same node |
| `STARTUP_PROBE_TIMEOUT_SECS` | `30` | seconds | Wait for guest binary to bind its port before marking failed |
| `STARTUP_PROBE_LOG_LINES` | `50` | lines | Serial console lines included in startup failure diagnostics |
| `SUSPEND_DRAIN_SECS` | `30` | seconds | Grace period for in-flight requests to complete before VM kill on suspend |

### Proxy (`crates/proxy`)

| Variable | Default | Unit | What it controls |
|----------|---------|------|------------------|
| `ROUTE_CACHE_RECONCILE_SECS` | `60` | seconds | How often the route cache is reconciled against DB |
| `PULSE_DEBOUNCE_SECS` | `30` | seconds | Min seconds between traffic pulse publishes per slug |

### Resource Quotas (per VM)

Applied to every Metal VM unless overridden per-service. Set `0` for unlimited.

| Variable | Default | Unit | What it controls |
|----------|---------|------|------------------|
| `QUOTA_DISK_READ_BPS` | `104857600` | bytes/s | Disk read bandwidth (100 MB/s) |
| `QUOTA_DISK_WRITE_BPS` | `104857600` | bytes/s | Disk write bandwidth (100 MB/s) |
| `QUOTA_DISK_READ_IOPS` | `5000` | ops/s | Disk read IOPS |
| `QUOTA_DISK_WRITE_IOPS` | `2000` | ops/s | Disk write IOPS |
| `QUOTA_NET_INGRESS_KBPS` | `100000` | kbps | Network ingress bandwidth (100 Mbps) |
| `QUOTA_NET_EGRESS_KBPS` | `100000` | kbps | Network egress bandwidth (100 Mbps) |

### Tuning Guide

#### High-traffic production (32-core bare metal)

```bash
DATABASE_POOL_SIZE=32
NODE_MAX_CONCURRENT_PROVISIONS=16
IDLE_TIMEOUT_SECS=600
USAGE_REPORT_INTERVAL_SECS=30
BILLING_INTERVAL_SECS=30
QUOTA_DISK_READ_BPS=209715200      # 200 MB/s
QUOTA_NET_EGRESS_KBPS=200000        # 200 Mbps
WASM_MAX_CONCURRENT_REQUESTS=128    # more headroom on beefy nodes
WASM_MAX_MEMORY_BYTES=268435456     # 256 MiB per Wasm instance
CGROUP_PIDS_MAX=1024                # higher PID ceiling for complex workloads
```

#### Local development (laptop)

```bash
DATABASE_POOL_SIZE=4
NODE_MAX_CONCURRENT_PROVISIONS=2
IDLE_TIMEOUT_SECS=0                 # disable idle timeout
VM_CRASH_CHECK_INTERVAL_SECS=30     # less frequent checks
QUOTA_DISK_READ_BPS=0               # unlimited IO for dev
QUOTA_NET_EGRESS_KBPS=0             # unlimited network for dev
```

#### Debugging provision issues

```bash
PROVISION_ACK_WAIT_SECS=600         # more time for slow builds
PROVISION_TIMEOUT_SECS=900          # longer per-provision deadline
PROVISIONING_TIMEOUT_MINS=30        # longer before marking failed
PROVISION_LAG_THRESHOLD=10          # earlier warning
LAG_MONITOR_INTERVAL_SECS=10        # more frequent checks
S3_DOWNLOAD_TIMEOUT_SECS=600        # more time for large artifacts
```

---

## OTEL Tracing Verification

The tracing pipeline: Rust binaries → OTLP gRPC → Tempo → Grafana.

### Verify OTEL_ENDPOINT is set

Nomad jobs reference `${OTEL_ENDPOINT}` in their `env` blocks. This must be set in Nomad Variables:

```bash
# Check if it's set
nomad var get secrets/api | grep -i otel

# Set it (Tempo listens on :4317 gRPC on the NAT VPS)
nomad var put secrets/api otel_endpoint=http://<nat-tailscale-hostname>:4317
```

If `OTEL_ENDPOINT` is empty or unset, `OTEL_EXPORTER_OTLP_ENDPOINT=""` causes the OTEL SDK to silently drop all spans.

### Verify traces are landing in Tempo

```bash
# Check Tempo is receiving spans (run on NAT VPS)
curl -s http://127.0.0.1:3200/metrics | grep tempo_distributor_spans_received_total

# If the counter is 0 or missing, traces aren't arriving.
# Check Tempo is healthy:
curl -s http://127.0.0.1:3200/ready

# Search for recent traces in Grafana:
# Explore → Tempo datasource → Search → service.name = "api"
```
