# ── Services ──────────────────────────────────────────────────────────────────
#
# Postgres 16 + PgBouncer: self-hosted on node-db (provisioned via cloud-init).
# Object storage: Wasabi S3 (managed outside Terraform, credentials in variables).
#
# No managed services — everything runs on Hivelocity bare metal.
