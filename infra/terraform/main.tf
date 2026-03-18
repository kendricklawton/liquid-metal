terraform {
  required_version = ">= 1.6"
  backend "gcs" {}
  required_providers {
    vultr = {
      source  = "vultr/vultr"
      version = "~> 2.21"
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

variable "env" { type = string }    # dev | prod
variable "region" { type = string } # vultr region slug: ord

variable "vultr_api_key" { sensitive = true }
variable "ssh_key_id" { type = string }
variable "bare_metal_plan" { type = string }
variable "vps_plan" { type = string }
variable "vps_gateway_plan" {
  type        = string
  default     = ""
  description = "VPS plan for standby gateway. Defaults to vps_plan if empty."
}
variable "os_id" { type = number }

variable "cloudflare_api_token" { sensitive = true }
variable "cloudflare_zone_id" { type = string }
variable "domain" { type = string }

variable "tailscale_api_key" { sensitive = true }
variable "tailscale_tailnet" { type = string }

variable "grafana_admin_password" { sensitive = true }

variable "nats_user" {
  type    = string
  default = "liquidmetal"
}
variable "nats_password" { sensitive = true }

# Per-role NATS passwords for subject-level ACLs.
# Each service authenticates as a separate NATS user with least-privilege
# publish/subscribe permissions. Generate each: openssl rand -hex 32
variable "nats_password_daemon" { sensitive = true }
variable "nats_password_proxy"  { sensitive = true }

variable "slack_webhook_url" {
  type      = string
  default   = ""
  sensitive = true
}

variable "heartbeat_url_nomad"            {
  type = string
  default = ""
}
variable "heartbeat_url_postgres"         {
  type = string
  default = ""
}
variable "heartbeat_url_victoriametrics"  {
  type = string
  default = ""
}
variable "heartbeat_url_victorialogs"     {
  type = string
  default = ""
}
variable "heartbeat_url_artifacts"        {
  type = string
  default = ""
}

variable "gcp_project" { type = string }
variable "gcp_region" {
  type    = string
  default = "us-central1"
}

# ── Providers ─────────────────────────────────────────────────────────────────

provider "vultr" {
  api_key     = var.vultr_api_key
  rate_limit  = 100
  retry_limit = 3
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
  prefix = "${var.env}-ord" # e.g. dev-ord, prod-ord
}
