# ── Tailscale pre-auth keys ───────────────────────────────────────────────────
resource "tailscale_tailnet_key" "nat_vps" {
  reusable      = false
  ephemeral     = false
  preauthorized = true
  expiry        = 3600
  description   = "${local.prefix}-nat-vps"
}

resource "tailscale_tailnet_key" "gateway_b" {
  reusable      = false
  ephemeral     = false
  preauthorized = true
  expiry        = 3600
  description   = "${local.prefix}-gateway-b"
}

resource "tailscale_tailnet_key" "node_metal" {
  reusable      = false
  ephemeral     = false
  preauthorized = true
  expiry        = 3600
  description   = "${local.prefix}-node-metal"
}

resource "tailscale_tailnet_key" "node_liquid" {
  reusable      = false
  ephemeral     = false
  preauthorized = true
  expiry        = 3600
  description   = "${local.prefix}-node-liquid"
}

# ── Reserved IP (floating) ────────────────────────────────────────────────────
# DNS points here, not at any specific VPS. On gateway-a failure, the failover
# script on gateway-b calls the Vultr API to move this IP to itself.
resource "vultr_reserved_ip" "gateway" {
  region      = var.region
  ip_type     = "v4"
  instance_id = vultr_instance.nat_vps.id
  label       = "${local.prefix}-gateway-vip"
}

# ── Gateway A (primary) ──────────────────────────────────────────────────────
# Control plane + data plane. HAProxy, Pingora, API, NATS, Nomad server,
# PgBouncer, observability. If this box dies, gateway-b takes the reserved IP
# and keeps routing traffic — new deploys are unavailable until recovery.

resource "vultr_instance" "nat_vps" {
  label       = "${local.prefix}-nat-vps"
  region      = var.region
  plan        = var.vps_plan
  os_id       = var.os_id
  hostname    = "${local.prefix}-nat-vps"
  ssh_key_ids = [var.ssh_key_id]

  user_data = templatefile("${path.module}/templates/cloud-init-nat.yaml", {
    tailscale_auth_key         = tailscale_tailnet_key.nat_vps.key
    hostname                   = "${local.prefix}-nat-vps"
    grafana_admin_password     = var.grafana_admin_password
    pg_host                    = vultr_database.postgres.host
    pg_port                    = vultr_database.postgres.port
    pg_dbname                  = vultr_database.postgres.dbname
    pg_user                    = vultr_database.postgres.user
    pg_password                = vultr_database.postgres.password
    nats_user                  = var.nats_user
    nats_password              = var.nats_password
    nats_password_daemon       = var.nats_password_daemon
    nats_password_proxy        = var.nats_password_proxy
    node_a_01_hostname         = "${local.prefix}-node-a-01"
    node_b_01_hostname         = "${local.prefix}-node-b-01"
    gcs_sa_key                 = base64decode(google_service_account_key.backup.private_key)
    gcs_bucket_postgres        = google_storage_bucket.postgres_backups.name
    gcs_bucket_victoriametrics = google_storage_bucket.victoriametrics_backups.name
    gcs_bucket_victorialogs    = google_storage_bucket.victorialogs_backups.name
    gcs_bucket_artifacts       = google_storage_bucket.artifacts_backups.name
    s3_endpoint                = vultr_object_storage.artifacts.s3_hostname
    s3_access_key              = vultr_object_storage.artifacts.s3_access_key
    s3_secret_key              = vultr_object_storage.artifacts.s3_secret_key
    s3_bucket                  = "${local.prefix}-artifacts"
    domain                     = var.domain
    cloudflare_api_token              = var.cloudflare_api_token
    heartbeat_url_postgres            = var.heartbeat_url_postgres
    heartbeat_url_victoriametrics     = var.heartbeat_url_victoriametrics
    heartbeat_url_victorialogs        = var.heartbeat_url_victorialogs
    heartbeat_url_artifacts           = var.heartbeat_url_artifacts
    slack_webhook_url                 = var.slack_webhook_url
  })

  tags = ["liquid-metal", var.env, "nat", "gateway-primary"]
}

# ── Gateway B (standby) ──────────────────────────────────────────────────────
# Data plane only. HAProxy + Pingora + Nomad client (gateway class).
# No NATS, no Nomad server, no API, no observability stack.
# Failover script monitors gateway-a and takes the reserved IP on failure.

resource "vultr_instance" "gateway_b" {
  label       = "${local.prefix}-gateway-b"
  region      = var.region
  plan        = var.vps_gateway_plan != "" ? var.vps_gateway_plan : var.vps_plan
  os_id       = var.os_id
  hostname    = "${local.prefix}-gateway-b"
  ssh_key_ids = [var.ssh_key_id]

  user_data = templatefile("${path.module}/templates/cloud-init-gateway.yaml", {
    tailscale_auth_key  = tailscale_tailnet_key.gateway_b.key
    hostname            = "${local.prefix}-gateway-b"
    nat_vps_hostname    = "${local.prefix}-nat-vps"
    pg_host             = vultr_database.postgres.host
    pg_port             = vultr_database.postgres.port
    pg_dbname           = vultr_database.postgres.dbname
    pg_user             = vultr_database.postgres.user
    pg_password         = vultr_database.postgres.password
    nats_password_proxy = var.nats_password_proxy
    domain              = var.domain
    vultr_api_key       = var.vultr_api_key
    reserved_ip_id      = vultr_reserved_ip.gateway.id
    gateway_b_id        = "${local.prefix}-gateway-b"
  })

  tags = ["liquid-metal", var.env, "gateway-standby"]
}

# ── Metal tier ────────────────────────────────────────────────────────────────

resource "vultr_bare_metal" "node_metal" {
  label       = "${local.prefix}-node-a-01"
  region      = var.region
  plan        = var.bare_metal_plan
  os_id       = var.os_id
  hostname    = "${local.prefix}-node-a-01"
  ssh_key_ids = [var.ssh_key_id]

  user_data = templatefile("${path.module}/templates/cloud-init-metal.yaml", {
    tailscale_auth_key = tailscale_tailnet_key.node_metal.key
    hostname           = "${local.prefix}-node-a-01"
    node_id            = "node-a-01"
    nomad_node_class   = "metal"
    vlogs_url          = "${local.prefix}-nat-vps"
    gcs_sa_key         = base64decode(google_service_account_key.backup.private_key)
    gcs_bucket_nomad       = google_storage_bucket.nomad_backups.name
    heartbeat_url_nomad    = var.heartbeat_url_nomad
  })

  tags = ["liquid-metal", var.env, "metal", "primary"]
}

# ── Liquid tier ───────────────────────────────────────────────────────────────

resource "vultr_bare_metal" "node_liquid" {
  label       = "${local.prefix}-node-b-01"
  region      = var.region
  plan        = var.bare_metal_plan
  os_id       = var.os_id
  hostname    = "${local.prefix}-node-b-01"
  ssh_key_ids = [var.ssh_key_id]

  user_data = templatefile("${path.module}/templates/cloud-init-liquid.yaml", {
    tailscale_auth_key = tailscale_tailnet_key.node_liquid.key
    hostname           = "${local.prefix}-node-b-01"
    node_id            = "node-b-01"
    nomad_node_class   = "liquid"
    vlogs_url          = "${local.prefix}-nat-vps"
    gcs_sa_key         = base64decode(google_service_account_key.backup.private_key)
    gcs_bucket_nomad       = google_storage_bucket.nomad_backups.name
    heartbeat_url_nomad    = var.heartbeat_url_nomad
  })

  tags = ["liquid-metal", var.env, "liquid", "active"]
}
