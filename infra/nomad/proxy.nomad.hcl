# Liquid Metal Proxy (Pingora)
# Runs active/active on all 4 bare metal nodes.
# Maps slug → upstream_addr via Postgres DB lookup.
# HAProxy on NAT VPS routes :443 traffic here via Tailscale.
#
# Deploy: nomad job run infra/jobs/proxy.nomad.hcl

job "proxy" {
  datacenters = ["ord"]
  type        = "service"

  update {
    max_parallel     = 1
    min_healthy_time = "15s"
    healthy_deadline = "2m"
    auto_revert      = true
    stagger          = "20s"
  }

  group "proxy" {
    count = 4

    spread {
      attribute = "${node.unique.name}"
    }

    network {
      port "https" {
        static = 443
      }
      port "http" {
        static = 80
      }
    }

    service {
      name = "proxy"
      port = "https"

      check {
        type     = "tcp"
        interval = "10s"
        timeout  = "3s"
      }
    }

    task "proxy" {
      driver = "raw_exec"

      artifact {
        source      = "https://github.com/YOUR_ORG/liquid-metal/releases/download/${meta.version}/proxy-x86_64-unknown-linux-gnu.tar.gz"
        destination = "local/"
      }

      config {
        command = "local/proxy"
      }

      # NOTE: database_url should point at PgBouncer (nat-vps:6432), not Postgres directly.
      # NATS_USER=proxy (proxy role — publish traffic_pulse, subscribe route updates)
      # nomad var put secrets/proxy nats_user=proxy nats_password=<proxy-password> ...
      template {
        data        = <<EOF
{{ with nomadVar "secrets/proxy" }}
DATABASE_URL={{ .database_url }}
NATS_URL={{ .nats_url }}
NATS_USER={{ .nats_user }}
NATS_PASSWORD={{ .nats_password }}
{{ end }}
EOF
        destination = "secrets/env"
        env         = true
      }

      env {
        BIND_ADDR = "0.0.0.0:443"
        RUST_LOG  = "info"
      }

      resources {
        cpu    = 500
        memory = 256
      }
    }
  }

  meta {
    version = "v0.1.0"
  }
}
