# Cloudflare DNS — all traffic enters NAT VPS, never moves.
# proxied = false — Cloudflare proxy would interfere with Pingora TLS termination.

resource "cloudflare_record" "wildcard" {
  zone_id = var.cloudflare_zone_id
  name    = "*"
  content = vultr_instance.nat_vps.main_ip
  type    = "A"
  ttl     = 60
  proxied = false
}

resource "cloudflare_record" "apex" {
  zone_id = var.cloudflare_zone_id
  name    = "@"
  content = vultr_instance.nat_vps.main_ip
  type    = "A"
  ttl     = 60
  proxied = false
}
