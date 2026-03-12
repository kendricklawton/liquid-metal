# ── Tailscale pre-auth keys ───────────────────────────────────────────────────
resource "tailscale_tailnet_key" "nat_vps" {
  reusable      = false
  ephemeral     = false
  preauthorized = true
  expiry        = 3600
  description   = "${local.prefix}-nat-vps"
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

# ── NAT VPS ───────────────────────────────────────────────────────────────────
# Public entry point. HAProxy :80/:443, NATS tiebreaker, Tailscale node.

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
  })

  tags = ["liquid-metal", var.env, "nat"]
}

# ── Metal tier ────────────────────────────────────────────────────────────────

resource "vultr_bare_metal" "node_metal" {
  label       = "${local.prefix}-node-a-01"
  region      = var.region
  plan        = var.bare_metal_plan
  os_id       = var.os_id
  hostname    = "${local.prefix}-node-a-01"
  ssh_key_ids = [var.ssh_key_id]

  user_data = templatefile("${path.module}/templates/cloud-init-node.yaml", {
    tailscale_auth_key = tailscale_tailnet_key.node_metal.key
    hostname           = "${local.prefix}-node-a-01"
    node_id            = "node-a-01"
    node_engine        = "metal"
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

  user_data = templatefile("${path.module}/templates/cloud-init-node.yaml", {
    tailscale_auth_key = tailscale_tailnet_key.node_liquid.key
    hostname           = "${local.prefix}-node-b-01"
    node_id            = "node-b-01"
    node_engine        = "liquid"
    nomad_node_class   = "liquid"
    vlogs_url          = "${local.prefix}-nat-vps"
    gcs_sa_key         = base64decode(google_service_account_key.backup.private_key)
    gcs_bucket_nomad       = google_storage_bucket.nomad_backups.name
    heartbeat_url_nomad    = var.heartbeat_url_nomad
  })

  tags = ["liquid-metal", var.env, "liquid", "active"]
}
