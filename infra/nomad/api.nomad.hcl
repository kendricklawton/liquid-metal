# Liquid Metal API
# Runs active/active on all 4 bare metal nodes.
# Axum REST/JSON server on :7070. Writes Postgres, publishes to NATS.
#
# Deploy: nomad job run infra/jobs/api.nomad.hcl
# Update binary: bump meta.version + artifact URL, re-run above.

job "api" {
  datacenters = ["ord"]
  type        = "service"

  # Rolling deploy — one allocation at a time, zero downtime
  update {
    max_parallel     = 1
    min_healthy_time = "15s"
    healthy_deadline = "2m"
    auto_revert      = true
  }

  group "api" {
    count = 4 # One per node — Nomad spreads across available nodes

    # Spread evenly across all nodes
    spread {
      attribute = "${node.unique.name}"
    }

    network {
      port "http" {
        static = 7070
      }
    }

    service {
      name = "api"
      port = "http"

      check {
        type     = "http"
        path     = "/healthz"
        interval = "10s"
        timeout  = "3s"
      }
    }

    task "api" {
      driver = "raw_exec" # Native Rust binary — no container runtime needed

      # Pull binary from GitHub Releases
      # Update the URL when cutting a new release
      artifact {
        source      = "https://github.com/YOUR_ORG/liquid-metal/releases/download/${meta.version}/api-x86_64-unknown-linux-gnu.tar.gz"
        destination = "local/"
      }

      config {
        command = "local/api"
      }

      env {
        BIND_ADDR                  = "0.0.0.0:7070"
        DATABASE_URL               = "${DATABASE_URL}"
        NATS_URL                   = "${NATS_URL}"
        INTERNAL_SECRET            = "${INTERNAL_SECRET}"
        OBJECT_STORAGE_ENDPOINT    = "${OBJECT_STORAGE_ENDPOINT}"
        OBJECT_STORAGE_BUCKET      = "${OBJECT_STORAGE_BUCKET}"
        OBJECT_STORAGE_ACCESS_KEY  = "${OBJECT_STORAGE_ACCESS_KEY}"
        OBJECT_STORAGE_SECRET_KEY  = "${OBJECT_STORAGE_SECRET_KEY}"
        RUST_LOG                   = "info"
      }

      # Secrets injected from Nomad Variables (nomad var put secrets/api ...)
      # NATS_USER=liquidmetal (API role — publish provision/deprovision/suspend)
      # NOTE: database_url should point at PgBouncer (nat-vps:6432), not Postgres directly.
      template {
        data        = <<EOF
{{ with nomadVar "secrets/api" }}
DATABASE_URL={{ .database_url }}
NATS_URL={{ .nats_url }}
NATS_USER={{ .nats_user }}
NATS_PASSWORD={{ .nats_password }}
INTERNAL_SECRET={{ .internal_secret }}
OBJECT_STORAGE_ENDPOINT={{ .object_storage_endpoint }}
OBJECT_STORAGE_BUCKET={{ .object_storage_bucket }}
OBJECT_STORAGE_ACCESS_KEY={{ .object_storage_access_key }}
OBJECT_STORAGE_SECRET_KEY={{ .object_storage_secret_key }}
{{ end }}
EOF
        destination = "secrets/env"
        env         = true
      }

      resources {
        cpu    = 500  # MHz
        memory = 256  # MB
      }
    }
  }

  meta {
    version = "v0.1.0" # Bump to trigger rolling deploy
  }
}
