# Database Migrations
# Batch job — runs once per dispatch, applies pending SQL migrations via refinery.
# Uses the API binary with --migrate flag so there's a single migration system.
#
# Run:   nomad job dispatch migrate
# Check: nomad job status migrate

job "migrate" {
  datacenters = ["ord"]
  type        = "batch"

  # Parameterized so it can be dispatched on deploy
  parameterized {}

  group "migrate" {
    count = 1

    # Pin to a single node to avoid concurrent migration runs
    constraint {
      attribute = "${node.class}"
      value     = "metal"
    }

    task "migrate" {
      driver = "raw_exec"

      artifact {
        source      = "https://github.com/YOUR_ORG/liquid-metal/releases/download/${meta.version}/api-x86_64-unknown-linux-gnu.tar.gz"
        destination = "local/"
      }

      config {
        command = "local/api"
        args    = ["--migrate"]
      }

      # MIGRATIONS_DATABASE_URL uses owner-privilege credentials for DDL.
      # NATS_USER=api (api role — needed for nats_connect but migrations don't use NATS)
      # nomad var put secrets/api migrations_database_url=<owner-url> ...
      template {
        data        = <<EOF
{{ with nomadVar "secrets/api" }}
DATABASE_URL={{ .database_url }}
MIGRATIONS_DATABASE_URL={{ .migrations_database_url }}
{{ end }}
RUST_LOG=info
EOF
        destination = "secrets/env"
        env         = true
      }

      resources {
        cpu    = 200
        memory = 128
      }
    }
  }

  meta {
    version = "v0.1.0"
  }
}
