//! TLS certificate storage backed by HashiCorp Vault.
//!
//! Cert + key PEMs are stored in Vault KV v2 at `platform/certs/{domain}`.
//! Vault handles encryption at rest and audit logging.
//!
//! This replaces the previous AES-GCM encryption scheme that used a
//! platform-scoped DEK wrapped by GCP KMS.

// Re-export from envelope for backward compatibility with cert_manager.
pub use crate::envelope::{store_cert, read_cert};
