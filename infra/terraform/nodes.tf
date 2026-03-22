# ── Tailscale pre-auth keys ───────────────────────────────────────────────────

resource "tailscale_tailnet_key" "gateway" {
  reusable      = false
  ephemeral     = false
  preauthorized = true
  expiry        = 7200 # 2 hours — bare metal provisioning can be slow
  description   = "${local.prefix}-gateway"
}

resource "tailscale_tailnet_key" "node_metal" {
  reusable      = false
  ephemeral     = false
  preauthorized = true
  expiry        = 7200
  description   = "${local.prefix}-node-metal"
}

resource "tailscale_tailnet_key" "node_liquid" {
  reusable      = false
  ephemeral     = false
  preauthorized = true
  expiry        = 7200
  description   = "${local.prefix}-node-liquid"
}

resource "tailscale_tailnet_key" "node_db" {
  reusable      = false
  ephemeral     = false
  preauthorized = true
  expiry        = 7200
  description   = "${local.prefix}-node-db"
}

# ── Private VLAN ─────────────────────────────────────────────────────────────
# All nodes on the same Hivelocity VLAN in DAL1. Internal traffic is unmetered.

data "hivelocity_device_port" "gateway_private" {
  first     = true
  device_id = hivelocity_bare_metal_device.gateway.device_id
  filter {
    name   = "name"
    values = ["eth1"]
  }
}

data "hivelocity_device_port" "node_metal_private" {
  first     = true
  device_id = hivelocity_bare_metal_device.node_metal.device_id
  filter {
    name   = "name"
    values = ["eth1"]
  }
}

data "hivelocity_device_port" "node_liquid_private" {
  first     = true
  device_id = hivelocity_bare_metal_device.node_liquid.device_id
  filter {
    name   = "name"
    values = ["eth1"]
  }
}

data "hivelocity_device_port" "node_db_private" {
  first     = true
  device_id = hivelocity_bare_metal_device.node_db.device_id
  filter {
    name   = "name"
    values = ["eth1"]
  }
}

resource "hivelocity_vlan" "private" {
  facility_code = var.hivelocity_location
  type          = "private"
  port_ids = [
    data.hivelocity_device_port.gateway_private.port_id,
    data.hivelocity_device_port.node_metal_private.port_id,
    data.hivelocity_device_port.node_liquid_private.port_id,
    data.hivelocity_device_port.node_db_private.port_id,
  ]
}

# ── Gateway ──────────────────────────────────────────────────────────────────
# E3-1230 v6, 4c/8t, 32GB RAM, 960GB SSD.
# Runs: HAProxy, Pingora, API, Web, NATS, Nomad server, observability stack.

resource "hivelocity_bare_metal_device" "gateway" {
  product_id        = var.hivelocity_product_id_gateway
  os_name           = var.hivelocity_os_name
  location_name     = var.hivelocity_location
  hostname          = "${local.prefix}-gateway"
  public_ssh_key_id = var.hivelocity_ssh_key_id
  tags              = ["liquid-metal", var.env, "gateway"]

  script = templatefile("${path.module}/templates/cloud-init-gateway.yaml", {
    tailscale_auth_key             = tailscale_tailnet_key.gateway.key
    hostname                       = "${local.prefix}-gateway"
    grafana_admin_password         = var.grafana_admin_password
    pg_host                        = "${local.prefix}-node-db"
    pg_port                        = "5432"
    pg_dbname                      = var.pg_dbname
    pg_user                        = var.pg_user
    pg_password                    = var.pg_password
    nats_user                      = var.nats_user
    nats_password                  = var.nats_password
    nats_password_daemon           = var.nats_password_daemon
    nats_password_proxy            = var.nats_password_proxy
    node_metal_hostname            = "${local.prefix}-node-metal"
    node_liquid_hostname           = "${local.prefix}-node-liquid"
    node_db_hostname               = "${local.prefix}-node-db"
    gcs_sa_key                     = base64decode(google_service_account_key.backup.private_key)
    gcs_bucket_victoriametrics     = google_storage_bucket.victoriametrics_backups.name
    gcs_bucket_victorialogs        = google_storage_bucket.victorialogs_backups.name
    gcs_bucket_artifacts           = google_storage_bucket.artifacts_backups.name
    s3_endpoint                    = var.wasabi_endpoint
    s3_access_key                  = var.wasabi_access_key
    s3_secret_key                  = var.wasabi_secret_key
    s3_bucket                      = var.wasabi_bucket
    domain                         = var.domain
    cloudflare_api_token           = var.cloudflare_api_token
    heartbeat_url_victoriametrics  = var.heartbeat_url_victoriametrics
    heartbeat_url_victorialogs     = var.heartbeat_url_victorialogs
    heartbeat_url_artifacts        = var.heartbeat_url_artifacts
    slack_webhook_url              = var.slack_webhook_url
  })
}

# ── Metal node ───────────────────────────────────────────────────────────────
# EPYC 7452, 32c/64t, 384GB RAM, 1TB NVMe.
# Runs: Firecracker microVMs, Nomad client (metal class).

resource "hivelocity_bare_metal_device" "node_metal" {
  product_id        = var.hivelocity_product_id_metal
  os_name           = var.hivelocity_os_name
  location_name     = var.hivelocity_location
  hostname          = "${local.prefix}-node-metal"
  public_ssh_key_id = var.hivelocity_ssh_key_id
  tags              = ["liquid-metal", var.env, "metal"]

  script = templatefile("${path.module}/templates/cloud-init-metal.yaml", {
    tailscale_auth_key  = tailscale_tailnet_key.node_metal.key
    hostname            = "${local.prefix}-node-metal"
    gateway_hostname    = "${local.prefix}-gateway"
    nomad_node_class    = "metal"
    vlogs_url           = "${local.prefix}-gateway"
    gcs_sa_key          = base64decode(google_service_account_key.backup.private_key)
    gcs_bucket_nomad    = google_storage_bucket.nomad_backups.name
    heartbeat_url_nomad = var.heartbeat_url_nomad
  })
}

# ── Liquid node ──────────────────────────────────────────────────────────────
# E3-1230 v6, 4c/8t, 32GB RAM, 960GB SSD.
# Runs: Wasmtime/WASI, Nomad client (liquid class).

resource "hivelocity_bare_metal_device" "node_liquid" {
  product_id        = var.hivelocity_product_id_liquid
  os_name           = var.hivelocity_os_name
  location_name     = var.hivelocity_location
  hostname          = "${local.prefix}-node-liquid"
  public_ssh_key_id = var.hivelocity_ssh_key_id
  tags              = ["liquid-metal", var.env, "liquid"]

  script = templatefile("${path.module}/templates/cloud-init-liquid.yaml", {
    tailscale_auth_key  = tailscale_tailnet_key.node_liquid.key
    hostname            = "${local.prefix}-node-liquid"
    gateway_hostname    = "${local.prefix}-gateway"
    nomad_node_class    = "liquid"
    vlogs_url           = "${local.prefix}-gateway"
    gcs_sa_key          = base64decode(google_service_account_key.backup.private_key)
    gcs_bucket_nomad    = google_storage_bucket.nomad_backups.name
    heartbeat_url_nomad = var.heartbeat_url_nomad
  })
}

# ── Database node ────────────────────────────────────────────────────────────
# E3-1230 v6, 4c/8t, 32GB RAM, 960GB SSD.
# Runs: Postgres 16, PgBouncer. No Nomad — dedicated to the database.

resource "hivelocity_bare_metal_device" "node_db" {
  product_id        = var.hivelocity_product_id_db
  os_name           = var.hivelocity_os_name
  location_name     = var.hivelocity_location
  hostname          = "${local.prefix}-node-db"
  public_ssh_key_id = var.hivelocity_ssh_key_id
  tags              = ["liquid-metal", var.env, "database"]

  script = templatefile("${path.module}/templates/cloud-init-db.yaml", {
    tailscale_auth_key     = tailscale_tailnet_key.node_db.key
    hostname               = "${local.prefix}-node-db"
    gateway_hostname       = "${local.prefix}-gateway"
    pg_user                = var.pg_user
    pg_password            = var.pg_password
    pg_dbname              = var.pg_dbname
    gcs_sa_key             = base64decode(google_service_account_key.backup.private_key)
    gcs_bucket_postgres    = google_storage_bucket.postgres_backups.name
    heartbeat_url_postgres = var.heartbeat_url_postgres
    vlogs_url              = "${local.prefix}-gateway"
  })
}
