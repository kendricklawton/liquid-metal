# ── Vultr Managed PostgreSQL 16 ───────────────────────────────────────────────

resource "vultr_database" "postgres" {
  database_engine         = "pg"
  database_engine_version = "16"
  region                  = var.region
  plan                    = "vultr-dbaas-startup-cc-1-55-2"
  label                   = "${local.prefix}-postgres"
  cluster_time_zone       = "America/Chicago"
  trusted_ips             = ["100.64.0.0/10"] # Tailscale CGNAT range
}

# ── Vultr Object Storage ───────────────────────────────────────────────────────
# S3-compatible artifact registry. No container registry needed.
# Layout: base/, metal/{svc}/{deploy}/app, wasm/{svc}/{deploy}/main.wasm

resource "vultr_object_storage" "artifacts" {
  cluster_id = 2 # Chicago ORD — verify: vultr-cli object-storage cluster list
  tier_id    = 1 # Performance — verify: vultr-cli object-storage tier list
  label      = "${local.prefix}-artifacts"
}
