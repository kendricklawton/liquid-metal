output "gateway_vip" { value = vultr_reserved_ip.gateway.subnet }
output "nat_vps_ip" { value = vultr_instance.nat_vps.main_ip }
output "gateway_b_ip" { value = vultr_instance.gateway_b.main_ip }
output "node_metal_ip" {
  value = var.enable_metal ? vultr_bare_metal.node_metal[0].main_ip : "not deployed"
}
output "node_liquid_ip" {
  value = var.enable_liquid ? vultr_bare_metal.node_liquid[0].main_ip : "not deployed"
}

output "database_url" {
  value     = "postgresql://${vultr_database.postgres.user}:${vultr_database.postgres.password}@${vultr_database.postgres.host}:${vultr_database.postgres.port}/${vultr_database.postgres.dbname}?sslmode=require"
  sensitive = true
}

# PgBouncer URL — all services should use this instead of database_url directly.
# PgBouncer runs on the NAT VPS in transaction mode, multiplexing app connections.
output "pgbouncer_url" {
  value     = "postgresql://${vultr_database.postgres.user}:${vultr_database.postgres.password}@${local.prefix}-nat-vps:6432/${vultr_database.postgres.dbname}"
  sensitive = true
}

output "victoriametrics_url" {
  value = "http://${local.prefix}-nat-vps:8428"
}

output "victorialogs_url" {
  value = "http://${local.prefix}-nat-vps:9428"
}

output "grafana_url" {
  value = "http://${local.prefix}-nat-vps:3001"
}

output "object_storage_endpoint" { value = vultr_object_storage.artifacts.s3_hostname }
output "object_storage_access_key" {
  value     = vultr_object_storage.artifacts.s3_access_key
  sensitive = true
}
output "object_storage_secret_key" {
  value     = vultr_object_storage.artifacts.s3_secret_key
  sensitive = true
}
