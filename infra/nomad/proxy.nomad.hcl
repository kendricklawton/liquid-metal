# Liquid Metal Proxy (Pingora)
# System job — runs on every gateway node (primary + standby).
# Maps slug → upstream_addr via Postgres DB lookup + in-memory route cache.
# HAProxy on each gateway forwards :443 (TCP pass-through) to Pingora on :8443.
#
# Deploy: nomad job run infra/nomad/proxy.nomad.hcl

job "proxy" {
  datacenters = ["ord"]
  type        = "system"

  update {
    max_parallel = 1
    stagger      = "30s"
  }

  group "proxy" {
    # System jobs run one instance per matching node — no count needed.

    constraint {
      attribute = "${node.class}"
      operator  = "="
      value     = "gateway"
    }

    network {
      # HAProxy owns :443/:80 publicly. Proxy (Pingora) listens on :8443
      # and HAProxy forwards tenant traffic to it on localhost.
      port "proxy" {
        static = 8443
      }
    }

    service {
      name = "proxy"
      port = "proxy"

      check {
        type     = "tcp"
        interval = "10s"
        timeout  = "3s"
      }
    }

    task "proxy" {
      driver = "raw_exec"

      artifact {
        source      = "https://github.com/YOUR_ORG/liquid-metal/releases/download/${NOMAD_META_version}/proxy-x86_64-unknown-linux-gnu.tar.gz"
        destination = "local/"
      }

      config {
        command = "local/proxy"
      }

      # NOTE: database_url should point at PgBouncer (localhost:6432 — each gateway runs PgBouncer).
      # NATS_USER=proxy (proxy role — publish traffic_pulse, subscribe route updates)
      # nomad var put secrets/proxy nats_user=proxy nats_password=<proxy-password> \
      #   cert_dek_wrapped=<base64> gcp_kms_key=<resource-name>
      template {
        data        = <<EOF
{{ with nomadVar "secrets/proxy" }}
DATABASE_URL={{ .database_url }}
NATS_URL={{ .nats_url }}
NATS_USER={{ .nats_user }}
NATS_PASSWORD={{ .nats_password }}
CERT_DEK_WRAPPED={{ .cert_dek_wrapped }}
GCP_KMS_KEY={{ .gcp_kms_key }}
{{ end }}
EOF
        destination = "secrets/env"
        env         = true
      }

      # GCP service account JSON for KMS cert DEK unwrap at startup.
      template {
        data        = <<EOF
{{ with nomadVar "secrets/proxy" }}{{ .gcp_sa_json }}{{ end }}
EOF
        destination = "secrets/gcp-sa.json"
      }

      env {
        BIND_ADDR = "0.0.0.0:8443"
        RUST_LOG  = "info"
        OTEL_EXPORTER_OTLP_ENDPOINT = "${OTEL_ENDPOINT}"
        GOOGLE_APPLICATION_CREDENTIALS = "$${NOMAD_SECRETS_DIR}/gcp-sa.json"
      }

      resources {
        cpu    = 500
        memory = 256
      }

      logs {
        max_files     = 10
        max_file_size = 10
      }
    }
  }

  meta {
    version = "v0.1.0"
  }
}
