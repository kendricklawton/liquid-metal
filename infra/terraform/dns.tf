# Cloudflare DNS — points directly at the gateway's public IP.
# proxied = false — Cloudflare proxy would interfere with Pingora TLS termination.

resource "cloudflare_record" "wildcard" {
  zone_id = var.cloudflare_zone_id
  name    = "*"
  content = hivelocity_bare_metal_device.gateway.primary_ip
  type    = "A"
  ttl     = 60
  proxied = false
}

resource "cloudflare_record" "apex" {
  zone_id = var.cloudflare_zone_id
  name    = "@"
  content = hivelocity_bare_metal_device.gateway.primary_ip
  type    = "A"
  ttl     = 60
  proxied = false
}
