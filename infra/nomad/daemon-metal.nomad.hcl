# Liquid Metal Daemon — Metal tier (Firecracker serverless)
# Beta topology: single instance on node-a-01 (node_class = "metal").
# Consumes NATS events: ProvisionEvent (build rootfs + snapshot), WakeEvent (restore from snapshot).
#
# NOTE: raw_exec driver requires privileged access on these nodes
# because the daemon creates TAP devices, attaches eBPF programs,
# and spawns Firecracker processes. Nomad client must be configured
# with: options { "driver.raw_exec.enable" = "1" }
#
# Deploy: nomad job run infra/nomad/daemon-metal.nomad.hcl

job "daemon-metal" {
  datacenters = ["ord"]
  type        = "service"

  update {
    max_parallel     = 1
    min_healthy_time = "20s"
    healthy_deadline = "3m"
    auto_revert      = true
  }

  group "daemon" {
    count = 1

    # Pin exclusively to Metal tier nodes
    constraint {
      attribute = "${node.class}"
      operator  = "="
      value     = "metal"
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
        NODE_ENGINE        = "metal"
        NODE_ID            = "${node.unique.name}" # node-a-01
        ARTIFACT_DIR       = "/var/lib/liquid-metal/artifacts"
        FC_BIN             = "/usr/local/bin/firecracker"
        FC_KERNEL_PATH     = "/opt/firecracker/vmlinux"
        FC_SOCK_DIR        = "/run/firecracker"
        BRIDGE             = "br0"
        BASE_IMAGE_KEY     = "templates/base-alpine-v1.ext4"
        USE_JAILER         = "true"
        JAILER_BIN         = "/usr/local/bin/jailer"
        JAILER_CHROOT_BASE = "/srv/jailer"
        JAILER_UID         = "10000"
        JAILER_GID         = "10000"
        HEALTH_PORT        = "${NOMAD_PORT_health}"
        RUST_LOG           = "info"
        OTEL_EXPORTER_OTLP_ENDPOINT = "${OTEL_ENDPOINT}"
      }

      resources {
        cpu    = 1000 # MHz — daemon itself is lightweight; VMs are separate processes
        memory = 512  # MB
      }

      logs {
        max_files     = 10
        max_file_size = 10
      }

      service {
        name = "daemon-metal"
        port = "health"
        check {
          type     = "http"
          path     = "/health"
          interval = "10s"
          timeout  = "3s"
        }
      }
    }

    network {
      port "health" {}
    }
  }

  meta {
    version = "v0.1.0"
  }
}
