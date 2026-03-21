# HashiCorp Vault
# Single instance on primary gateway node. Raft storage, GCP KMS auto-unseal.
# Listens on localhost:8200 only — API and other services reach it without a network hop.
#
# Deploy: nomad job run infra/nomad/vault.nomad.hcl
# Init:   vault operator init (first time only, after deploy)

job "vault" {
  datacenters = ["ord"]
  type        = "service"

  update {
    max_parallel     = 1
    min_healthy_time = "30s"
    healthy_deadline = "3m"
    auto_revert      = true
  }

  group "vault" {
    count = 1

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
      port "api" {
        static = 8200
      }
    }

    service {
      name = "vault"
      port = "api"

      check {
        type     = "http"
        path     = "/v1/sys/health"
        interval = "10s"
        timeout  = "3s"

        # Vault returns 200 when initialized+unsealed, 429 when standby,
        # 472 when in DR secondary mode, 501 when not initialized,
        # 503 when sealed. Accept 200 and 429 as healthy.
        check_restart {
          limit           = 3
          grace           = "60s"
          ignore_warnings = true
        }
      }
    }

    task "vault" {
      driver = "raw_exec"

      artifact {
        source      = "https://releases.hashicorp.com/vault/${NOMAD_META_vault_version}/vault_${NOMAD_META_vault_version}_linux_amd64.zip"
        destination = "local/"
      }

      config {
        command = "local/vault"
        args    = ["server", "-config=${NOMAD_TASK_DIR}/vault.hcl"]
      }

      # GCP KMS credentials for auto-unseal
      template {
        data        = <<EOF
{{ with nomadVar "secrets/vault" }}
GOOGLE_APPLICATION_CREDENTIALS={{ .gcp_credentials_path }}
GCP_KMS_PROJECT={{ .gcp_kms_project }}
GCP_KMS_LOCATION={{ .gcp_kms_location }}
GCP_KMS_KEY_RING={{ .gcp_kms_key_ring }}
GCP_KMS_CRYPTO_KEY={{ .gcp_kms_crypto_key }}
{{ end }}
EOF
        destination = "secrets/env"
        env         = true
      }

      # Vault server configuration
      template {
        data        = <<EOF
ui = false

listener "tcp" {
  address     = "127.0.0.1:8200"
  tls_disable = true
}

storage "raft" {
  path    = "/var/lib/vault/data"
  node_id = "gateway-primary"
}

seal "gcpckms" {
  project     = "{{ env "GCP_KMS_PROJECT" }}"
  region      = "{{ env "GCP_KMS_LOCATION" }}"
  key_ring    = "{{ env "GCP_KMS_KEY_RING" }}"
  crypto_key  = "{{ env "GCP_KMS_CRYPTO_KEY" }}"
}

audit "file" {
  file_path = "/var/log/vault/audit.log"
}

api_addr     = "http://127.0.0.1:8200"
cluster_addr = "http://127.0.0.1:8201"

disable_mlock = true
EOF
        destination = "local/vault.hcl"
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
    vault_version = "1.17.0"
  }
}
