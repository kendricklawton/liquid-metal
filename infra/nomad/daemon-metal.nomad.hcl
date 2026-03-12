# Liquid Metal Daemon — Metal tier (Firecracker)
# Runs on node-a-01 and node-a-02 only (node_class = "metal").
# Consumes NATS ProvisionEvent → boots Firecracker VMs via KVM.
#
# NOTE: raw_exec driver requires privileged access on these nodes
# because the daemon creates TAP devices, attaches eBPF programs,
# and spawns Firecracker processes. Nomad client must be configured
# with: options { "driver.raw_exec.enable" = "1" }
#
# Deploy: nomad job run infra/jobs/daemon-metal.nomad.hcl

job "daemon-metal" {
  datacenters = ["ord"]
  type        = "service"

  update {
    max_parallel     = 1
    min_healthy_time = "20s"
    healthy_deadline = "3m"
    auto_revert      = true
    # Stagger updates so node-a-01 and node-a-02 never update simultaneously
    stagger          = "30s"
  }

  group "daemon" {
    count = 2 # One per Metal node

    # Pin exclusively to Metal tier nodes
    constraint {
      attribute = "${node.class}"
      operator  = "="
      value     = "metal"
    }

    # One allocation per host — never two daemons on same node
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
        NODE_ENGINE    = "metal"
        NODE_ID        = "${node.unique.name}" # node-a-01 / node-a-02
        ARTIFACT_DIR   = "/var/lib/liquid-metal/artifacts"
        FC_BIN         = "/usr/local/bin/firecracker"
        FC_KERNEL_PATH = "/opt/firecracker/vmlinux"
        FC_SOCK_DIR    = "/run/firecracker"
        BRIDGE         = "br0"
        USE_JAILER     = "true"
        JAILER_BIN     = "/usr/local/bin/jailer"
        RUST_LOG       = "info"
      }

      resources {
        cpu    = 1000 # MHz — daemon itself is lightweight; VMs are separate processes
        memory = 512  # MB
      }
    }
  }

  meta {
    version = "v0.1.0"
  }
}
