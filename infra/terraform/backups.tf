# ── GCS Backup Buckets ───────────────────────────────────────────────────────
# Separate bucket per data type. The Terraform state bucket (backend "gcs")
# is created manually outside of this config.

locals {
  backup_buckets = {
    postgres         = "lm-${var.env}-postgres-backups"
    victoriametrics  = "lm-${var.env}-victoriametrics-backups"
    victorialogs     = "lm-${var.env}-victorialogs-backups"
    nomad            = "lm-${var.env}-nomad-backups"
    artifacts        = "lm-${var.env}-artifacts-backups"
    vault            = "lm-${var.env}-vault-backups"
  }
}

resource "google_storage_bucket" "postgres_backups" {
  name          = "${var.gcp_project}-${local.backup_buckets.postgres}"
  location      = var.gcp_region
  force_destroy = false

  uniform_bucket_level_access = true

  lifecycle_rule {
    condition { age = 30 }
    action {
      type = "SetStorageClass"
      storage_class = "NEARLINE"
    }
  }
  lifecycle_rule {
    condition { age = 90 }
    action { type = "Delete" }
  }
}

resource "google_storage_bucket" "victoriametrics_backups" {
  name          = "${var.gcp_project}-${local.backup_buckets.victoriametrics}"
  location      = var.gcp_region
  force_destroy = false

  uniform_bucket_level_access = true

  lifecycle_rule {
    condition { age = 30 }
    action {
      type = "SetStorageClass"
      storage_class = "NEARLINE"
    }
  }
  lifecycle_rule {
    condition { age = 90 }
    action { type = "Delete" }
  }
}

resource "google_storage_bucket" "victorialogs_backups" {
  name          = "${var.gcp_project}-${local.backup_buckets.victorialogs}"
  location      = var.gcp_region
  force_destroy = false

  uniform_bucket_level_access = true

  lifecycle_rule {
    condition { age = 30 }
    action {
      type = "SetStorageClass"
      storage_class = "NEARLINE"
    }
  }
  lifecycle_rule {
    condition { age = 90 }
    action {
      type = "Delete"
    }
  }
}

resource "google_storage_bucket" "nomad_backups" {
  name          = "${var.gcp_project}-${local.backup_buckets.nomad}"
  location      = var.gcp_region
  force_destroy = false

  uniform_bucket_level_access = true

  lifecycle_rule {
    condition { age = 30 }
    action {
      type = "SetStorageClass"
      storage_class = "NEARLINE"
    }
  }
  lifecycle_rule {
    condition { age = 90 }
    action {
      type = "Delete"
    }
  }
}

resource "google_storage_bucket" "artifacts_backups" {
  name          = "${var.gcp_project}-${local.backup_buckets.artifacts}"
  location      = var.gcp_region
  force_destroy = false

  uniform_bucket_level_access = true

  # Artifacts are immutable deploys — keep longer than observability data.
  # Nearline after 30d, Coldline after 90d, delete after 180d.
  lifecycle_rule {
    condition { age = 30 }
    action {
      type = "SetStorageClass"
      storage_class = "NEARLINE"
    }
  }
  lifecycle_rule {
    condition { age = 90 }
    action {
      type = "SetStorageClass"
      storage_class = "COLDLINE"
    }
  }
  lifecycle_rule {
    condition { age = 180 }
    action {
      type = "Delete"
    }
  }
}

# ── Service Account for backup uploads ───────────────────────────────────────
# Machines authenticate with this key to push backups to GCS.
# Single SA with write access to all backup buckets.

resource "google_service_account" "backup" {
  account_id   = "lm-${var.env}-backup"
  display_name = "Liquid Metal ${var.env} backup agent"
}

resource "google_storage_bucket_iam_member" "backup_postgres" {
  bucket = google_storage_bucket.postgres_backups.name
  role   = "roles/storage.objectAdmin"
  member = "serviceAccount:${google_service_account.backup.email}"
}

resource "google_storage_bucket_iam_member" "backup_victoriametrics" {
  bucket = google_storage_bucket.victoriametrics_backups.name
  role   = "roles/storage.objectAdmin"
  member = "serviceAccount:${google_service_account.backup.email}"
}

resource "google_storage_bucket_iam_member" "backup_victorialogs" {
  bucket = google_storage_bucket.victorialogs_backups.name
  role   = "roles/storage.objectAdmin"
  member = "serviceAccount:${google_service_account.backup.email}"
}

resource "google_storage_bucket_iam_member" "backup_nomad" {
  bucket = google_storage_bucket.nomad_backups.name
  role   = "roles/storage.objectAdmin"
  member = "serviceAccount:${google_service_account.backup.email}"
}

resource "google_storage_bucket_iam_member" "backup_artifacts" {
  bucket = google_storage_bucket.artifacts_backups.name
  role   = "roles/storage.objectAdmin"
  member = "serviceAccount:${google_service_account.backup.email}"
}

resource "google_service_account_key" "backup" {
  service_account_id = google_service_account.backup.name
}

# ── Vault Backup Bucket ──────────────────────────────────────────────────────

resource "google_storage_bucket" "vault_backups" {
  name          = "${var.gcp_project}-${local.backup_buckets.vault}"
  location      = var.gcp_region
  force_destroy = false

  uniform_bucket_level_access = true

  lifecycle_rule {
    condition { age = 30 }
    action {
      type          = "SetStorageClass"
      storage_class = "NEARLINE"
    }
  }
  lifecycle_rule {
    condition { age = 90 }
    action { type = "Delete" }
  }
}

resource "google_storage_bucket_iam_member" "backup_vault" {
  bucket = google_storage_bucket.vault_backups.name
  role   = "roles/storage.objectAdmin"
  member = "serviceAccount:${google_service_account.backup.email}"
}

# ── Workload Identity Federation (GitHub Actions) ────────────────────────────
# Allows GitHub Actions to authenticate to GCP without JSON keys.
# Used by CI/CD workflows for Terraform operations.

variable "github_repo" {
  type        = string
  description = "GitHub repo in 'owner/repo' format (e.g. 'kendricklawton/liquid-metal')."
}

resource "google_iam_workload_identity_pool" "github" {
  workload_identity_pool_id = "github-actions"
  display_name              = "GitHub Actions"
  description               = "WIF pool for GitHub Actions CI/CD"
}

resource "google_iam_workload_identity_pool_provider" "github" {
  workload_identity_pool_id          = google_iam_workload_identity_pool.github.workload_identity_pool_id
  workload_identity_pool_provider_id = "github-oidc"
  display_name                       = "GitHub OIDC"

  attribute_mapping = {
    "google.subject"       = "assertion.sub"
    "attribute.actor"      = "assertion.actor"
    "attribute.repository" = "assertion.repository"
  }

  attribute_condition = "assertion.repository == '${var.github_repo}'"

  oidc {
    issuer_uri = "https://token.actions.githubusercontent.com"
  }
}

# SA that GitHub Actions impersonates for Terraform state + KMS
resource "google_service_account" "github_actions" {
  account_id   = "lm-${var.env}-github-actions"
  display_name = "Liquid Metal ${var.env} GitHub Actions"
}

# Allow the WIF pool to impersonate the SA
resource "google_service_account_iam_member" "github_actions_wif" {
  service_account_id = google_service_account.github_actions.name
  role               = "roles/iam.workloadIdentityUser"
  member             = "principalSet://iam.googleapis.com/${google_iam_workload_identity_pool.github.name}/attribute.repository/${var.github_repo}"
}

# Grant the SA access to Terraform state bucket
resource "google_project_iam_member" "github_actions_storage" {
  project = var.gcp_project
  role    = "roles/storage.admin"
  member  = "serviceAccount:${google_service_account.github_actions.email}"
}

