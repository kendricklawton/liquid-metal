terraform {
  required_version = ">= 1.6"
  backend "gcs" {}
  required_providers {
    hivelocity = {
      source  = "hivelocity/hivelocity"
      version = "~> 0.5"
    }
    cloudflare = {
      source  = "cloudflare/cloudflare"
      version = "~> 4.0"
    }
    tailscale = {
      source  = "tailscale/tailscale"
      version = "~> 0.16"
    }
    google = {
      source  = "hashicorp/google"
      version = "~> 6.0"
    }
  }
}

# ── Variables ──────────────────────────────────────────────────────────────────

variable "env" { type = string } # dev | prod

# Hivelocity
variable "hivelocity_api_key" { sensitive = true }
variable "hivelocity_ssh_key_id" {
  type        = number
  description = "SSH key ID in Hivelocity portal."
}
variable "hivelocity_location" {
  type    = string
  default = "DAL1"
}
variable "hivelocity_os_name" {
  type    = string
  default = "Ubuntu 24.04 LTS"
}
variable "hivelocity_product_id_gateway" {
  type        = number
  description = "Product ID for gateway node (E3-1230 v6, 32GB, 960GB SSD)."
}
variable "hivelocity_product_id_metal" {
  type        = number
  description = "Product ID for Metal node (EPYC 7452, 384GB, 1TB NVMe)."
}
variable "hivelocity_product_id_liquid" {
  type        = number
  description = "Product ID for Liquid node (E3-1230 v6, 32GB, 960GB SSD)."
}
variable "hivelocity_product_id_db" {
  type        = number
  description = "Product ID for database node (E3-1230 v6, 32GB, 960GB SSD)."
}

# Cloudflare
variable "cloudflare_api_token" { sensitive = true }
variable "cloudflare_zone_id" { type = string }
variable "domain" { type = string }

# Tailscale
variable "tailscale_api_key" { sensitive = true }
variable "tailscale_tailnet" { type = string }

# Wasabi S3 (managed outside Terraform — credentials only)
variable "wasabi_endpoint" {
  type    = string
  default = "s3.us-east-1.wasabisys.com"
}
variable "wasabi_access_key" { sensitive = true }
variable "wasabi_secret_key" { sensitive = true }
variable "wasabi_bucket" { type = string }

# Self-hosted Postgres
variable "pg_user" {
  type    = string
  default = "liquidmetal"
}
variable "pg_password" { sensitive = true }
variable "pg_dbname" {
  type    = string
  default = "liquidmetal"
}

# Services
variable "grafana_admin_password" { sensitive = true }
variable "nats_user" {
  type    = string
  default = "liquidmetal"
}
variable "nats_password"        { sensitive = true }
variable "nats_password_daemon" { sensitive = true }
variable "nats_password_proxy"  { sensitive = true }

variable "slack_webhook_url" {
  type      = string
  default   = ""
  sensitive = true
}

# Dead man's switch URLs for backup cron jobs
variable "heartbeat_url_nomad" {
  type    = string
  default = ""
}
variable "heartbeat_url_postgres" {
  type    = string
  default = ""
}
variable "heartbeat_url_victoriametrics" {
  type    = string
  default = ""
}
variable "heartbeat_url_victorialogs" {
  type    = string
  default = ""
}
variable "heartbeat_url_artifacts" {
  type    = string
  default = ""
}

# GCP (backup destination)
variable "gcp_project" { type = string }
variable "gcp_region" {
  type    = string
  default = "us-central1"
}

# ── Providers ─────────────────────────────────────────────────────────────────

provider "hivelocity" {
  api_key = var.hivelocity_api_key
}

provider "cloudflare" {
  api_token = var.cloudflare_api_token
}

provider "tailscale" {
  api_key = var.tailscale_api_key
  tailnet = var.tailscale_tailnet
}

provider "google" {
  project = var.gcp_project
  region  = var.gcp_region
}

# ── Locals ────────────────────────────────────────────────────────────────────

locals {
  prefix = "${var.env}-dal" # e.g. dev-dal, prod-dal
}
