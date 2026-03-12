# Liquid Metal Daemon — Liquid tier (Wasmtime)
# Runs on node-b-01 and node-b-02 only (node_class = "liquid").
# Consumes NATS ProvisionEvent → loads .wasm modules into Wasmtime, spins up HTTP shim.
# Stateless — both nodes run active/active. No KVM, no TAP, no eBPF needed.
#
# Deploy: nomad job run infra/jobs/daemon-liquid.nomad.hcl

job "daemon-liquid" {
  datacenters = ["ord"]
  type        = "service"

  update {
    max_parallel     = 1
    min_healthy_time = "15s"
    healthy_deadline = "2m"
    auto_revert      = true
    stagger          = "15s"
  }

  group "daemon" {
    count = 2 # One per Liquid node

    constraint {
      attribute = "${node.class}"
      operator  = "="
      value     = "liquid"
    }

    constraint {
      operator = "distinct_hosts"
      value    = "true"
    }

    task "daemon" {
      driver = "raw_exec"

      artifact {
        source      = "https://github.com/YOUR_ORG/liquid-metal/releases/download/${meta.version}/daemon-x86_64-unknown-linux-gnu.tar.gz"
        destination = "local/"
      }

      config {
        command = "local/daemon"
      }

      # NOTE: database_url should point at PgBouncer (nat-vps:6432), not Postgres directly.
      # NATS_USER=daemon (daemon role — subscribe provision/deprovision, publish routes/usage)
      template {
        data        = <<EOF
{{ with nomadVar "secrets/daemon" }}
DATABASE_URL={{ .database_url }}
NATS_URL={{ .nats_url }}
NATS_USER={{ .nats_user }}
NATS_PASSWORD={{ .nats_password }}
OBJECT_STORAGE_ENDPOINT={{ .object_storage_endpoint }}
OBJECT_STORAGE_BUCKET={{ .object_storage_bucket }}
OBJECT_STORAGE_ACCESS_KEY={{ .object_storage_access_key }}
OBJECT_STORAGE_SECRET_KEY={{ .object_storage_secret_key }}
{{ end }}
EOF
        destination = "secrets/env"
        env         = true
      }

      env {
        NODE_ENGINE  = "liquid"
        NODE_ID      = "${node.unique.name}" # node-b-01 / node-b-02
        ARTIFACT_DIR = "/var/lib/liquid-metal/artifacts"
        RUST_LOG     = "info"
      }

      resources {
        cpu    = 2000 # MHz — Wasmtime JIT compilation is CPU-intensive at load time
        memory = 1024 # MB — each loaded Wasm module lives in memory
      }
    }
  }

  meta {
    version = "v0.1.0"
  }
}
