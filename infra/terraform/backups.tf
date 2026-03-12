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
  }
}

resource "google_storage_bucket" "postgres_backups" {
  name          = "${var.gcp_project}-${local.backup_buckets.postgres}"
  location      = var.gcp_region
  force_destroy = false

  uniform_bucket_level_access = true

  lifecycle_rule {
    condition { age = 30 }
    action { type = "SetStorageClass"; storage_class = "NEARLINE" }
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
    action { type = "SetStorageClass"; storage_class = "NEARLINE" }
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
    action { type = "SetStorageClass"; storage_class = "NEARLINE" }
  }
  lifecycle_rule {
    condition { age = 90 }
    action { type = "Delete" }
  }
}

resource "google_storage_bucket" "nomad_backups" {
  name          = "${var.gcp_project}-${local.backup_buckets.nomad}"
  location      = var.gcp_region
  force_destroy = false

  uniform_bucket_level_access = true

  lifecycle_rule {
    condition { age = 30 }
    action { type = "SetStorageClass"; storage_class = "NEARLINE" }
  }
  lifecycle_rule {
    condition { age = 90 }
    action { type = "Delete" }
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
    action { type = "SetStorageClass"; storage_class = "NEARLINE" }
  }
  lifecycle_rule {
    condition { age = 90 }
    action { type = "SetStorageClass"; storage_class = "COLDLINE" }
  }
  lifecycle_rule {
    condition { age = 180 }
    action { type = "Delete" }
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

# ── Vultr Block Storage for observability ────────────────────────────────────
# Persistent volume for VictoriaMetrics, VictoriaLogs, and Grafana data.
# Survives NAT VPS instance replacement.

resource "vultr_block_storage" "observability" {
  label                = "${local.prefix}-observability"
  region               = var.region
  size_gb              = 40
  attached_to_instance = vultr_instance.nat_vps.id
  live                 = true # Hot-attach without reboot
}
