# Cloudflare DNS — points at the floating reserved IP, not any specific VPS.
# On failover, the reserved IP moves to gateway-b — DNS stays unchanged.
# proxied = false — Cloudflare proxy would interfere with Pingora TLS termination.

resource "cloudflare_record" "wildcard" {
  zone_id = var.cloudflare_zone_id
  name    = "*"
  content = vultr_reserved_ip.gateway.subnet
  type    = "A"
  ttl     = 60
  proxied = false
}

resource "cloudflare_record" "apex" {
  zone_id = var.cloudflare_zone_id
  name    = "@"
  content = vultr_reserved_ip.gateway.subnet
  type    = "A"
  ttl     = 60
  proxied = false
}
