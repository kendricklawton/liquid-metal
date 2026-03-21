output "gateway_ip"    { value = hivelocity_bare_metal_device.gateway.primary_ip }
output "node_metal_ip" { value = hivelocity_bare_metal_device.node_metal.primary_ip }
output "node_liquid_ip" { value = hivelocity_bare_metal_device.node_liquid.primary_ip }
output "node_db_ip"    { value = hivelocity_bare_metal_device.node_db.primary_ip }

output "database_url" {
  value     = "postgresql://${var.pg_user}:${var.pg_password}@${local.prefix}-node-db:5432/${var.pg_dbname}"
  sensitive = true
}

output "pgbouncer_url" {
  value     = "postgresql://${var.pg_user}:${var.pg_password}@${local.prefix}-node-db:6432/${var.pg_dbname}"
  sensitive = true
}

output "victoriametrics_url" {
  value = "http://${local.prefix}-gateway:8428"
}

output "victorialogs_url" {
  value = "http://${local.prefix}-gateway:9428"
}

output "grafana_url" {
  value = "http://${local.prefix}-gateway:3001"
}

output "object_storage_endpoint" { value = var.wasabi_endpoint }
output "object_storage_access_key" {
  value     = var.wasabi_access_key
  sensitive = true
}
output "object_storage_secret_key" {
  value     = var.wasabi_secret_key
  sensitive = true
}

# ── GitHub Actions WIF ────────────────────────────────────────────────────────

output "github_actions_wif_provider" {
  value       = google_iam_workload_identity_pool_provider.github.name
  description = "Full WIF provider name for google-github-actions/auth"
}

output "github_actions_sa_email" {
  value       = google_service_account.github_actions.email
  description = "Service account email for GitHub Actions to impersonate"
}
