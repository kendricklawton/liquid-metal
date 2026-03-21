# Liquid Metal API
# Beta topology: single instance on NAT VPS (node_class = "gateway").
# Axum REST/JSON server on :7070. Writes Postgres, publishes to NATS.
#
# Deploy: nomad job run infra/nomad/api.nomad.hcl
# Update binary: bump meta.version + artifact URL, re-run above.

job "api" {
  datacenters = ["ord"]
  type        = "service"

  update {
    max_parallel     = 1
    min_healthy_time = "15s"
    healthy_deadline = "2m"
    auto_revert      = true
  }

  group "api" {
    count = 1

    # Pin to primary gateway — API is the control plane, only runs on gateway-a.
    # Gateway-b is data plane only (HAProxy + Pingora).
    constraint {
      attribute = "${node.class}"
      operator  = "="
      value     = "gateway"
    }

    constraint {
      attribute = "${meta.gateway_role}"
      operator  = "="
      value     = "primary"
    }

    network {
      port "http" {
        static = 7070
      }
      port "metrics" {
        static = 9090
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
        BIND_ADDR                       = "0.0.0.0:7070"
        RUST_LOG                        = "info"
        OTEL_EXPORTER_OTLP_ENDPOINT     = "${OTEL_ENDPOINT}"
        # Vault on gateway — localhost, no network hop
        VAULT_ADDR                      = "http://127.0.0.1:8200"
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
VAULT_TOKEN={{ .vault_token }}
OIDC_ISSUER={{ .oidc_issuer }}
OIDC_CLI_CLIENT_ID={{ .oidc_cli_client_id }}
{{ end }}
EOF
        destination = "secrets/env"
        env         = true
      }

      resources {
        cpu    = 500  # MHz
        memory = 256  # MB
      }

      logs {
        max_files     = 10
        max_file_size = 10
      }
    }
  }

  meta {
    version = "v0.1.0" # Bump to trigger rolling deploy
  }
}
