/// ACME certificate manager — provisions and renews TLS certs for custom domains.
///
/// # Flow
/// 1. Every `CERT_MANAGER_INTERVAL_SECS` (default 60s), query for verified custom
///    domains that have no cert or whose cert expires in < 30 days.
/// 2. For each, run the ACME HTTP-01 challenge flow via Let's Encrypt.
/// 3. Store the issued cert + private key encrypted in `domain_certs`.
/// 4. Update `domains.tls_status` to `active`.
/// 5. Publish `platform.cert_provisioned` so proxy instances hot-reload the cert.
///
/// # ACME HTTP-01 challenge path
/// During provisioning, the ACME server sends a GET to:
///   http://{domain}/.well-known/acme-challenge/{token}
/// The domain already CNAMEs to us (is_verified=true), so this hits HAProxy → Pingora.
/// Pingora's `request_filter` reads `acme_challenges` from Postgres and responds.
///
/// # ACME account
/// The ACME account key is stored in the `ACME_ACCOUNT_KEY` env var as a JSON
/// string (serialized `instant_acme::AccountCredentials`). On first run, if unset,
/// a new account is created and the credentials are logged — the operator must
/// persist them as `ACME_ACCOUNT_KEY` before the next restart.
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use instant_acme::{
    Account, ChallengeType, Identifier, LetsEncrypt, NewAccount, NewOrder,
    OrderStatus,
};
use rcgen::{CertificateParams, KeyPair};
use uuid::Uuid;

use crate::AppState;
use crate::envelope;
use common::events::{CertProvisionedEvent, SUBJECT_CERT_PROVISIONED};

static CERT_MANAGER_INTERVAL_SECS: std::sync::LazyLock<u64> =
    std::sync::LazyLock::new(|| {
        common::config::env_or("CERT_MANAGER_INTERVAL_SECS", "60")
            .parse()
            .unwrap_or(60)
    });

/// Entry point — spawned as a background task from `main.rs`.
pub async fn run(state: Arc<AppState>) {
    let acme_dir = common::config::env_or(
        "ACME_DIRECTORY_URL",
        LetsEncrypt::Production.url(),
    );
    let contact_email = common::config::env_or("ACME_CONTACT_EMAIL", "ops@liquidmetal.dev");

    let account = match load_or_create_account(&acme_dir, &contact_email).await {
        Ok(a) => a,
        Err(e) => {
            tracing::error!(error = %e, "cert_manager: failed to initialize ACME account — TLS provisioning disabled");
            return;
        }
    };

    let mut interval = tokio::time::interval(Duration::from_secs(*CERT_MANAGER_INTERVAL_SECS));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        interval.tick().await;
        if let Err(e) = run_once(&state, &account).await {
            tracing::error!(error = %e, "cert_manager: provision cycle error");
        }
    }
}

async fn run_once(state: &AppState, account: &Account) -> Result<()> {
    let db = state.db.get().await.context("db pool")?;

    // Find verified custom domains that need a cert or need renewal.
    let rows = db.query(
        "SELECT d.id, d.domain
         FROM domains d
         LEFT JOIN domain_certs dc ON dc.domain_id = d.id
         WHERE d.is_verified = true
           AND d.deleted_at IS NULL
           AND d.tls_status != 'provisioning'
           AND (dc.domain_id IS NULL
                OR dc.expires_at < NOW() + INTERVAL '30 days')
         ORDER BY d.created_at
         LIMIT 5",
        &[],
    ).await.context("querying domains for cert provisioning")?;

    for row in &rows {
        let domain_id: Uuid   = row.get("id");
        let domain:    String = row.get("domain");

        if let Err(e) = provision_cert(state, account, domain_id, &domain).await {
            tracing::error!(domain, error = %e, "cert_manager: failed to provision cert");
            // Mark as error so we don't retry in a tight loop.
            let db2 = state.db.get().await.context("db pool")?;
            db2.execute(
                "UPDATE domains SET tls_status = 'error', updated_at = NOW() WHERE id = $1",
                &[&domain_id],
            ).await.ok();
        }
    }

    // GC stale ACME challenges (safety net — normal path deletes them after each attempt).
    db.execute("SELECT gc_acme_challenges()", &[]).await.ok();

    Ok(())
}

async fn provision_cert(
    state: &AppState,
    account: &Account,
    domain_id: Uuid,
    domain: &str,
) -> Result<()> {
    tracing::info!(domain, "cert_manager: provisioning TLS cert");

    let db = state.db.get().await.context("db pool")?;
    db.execute(
        "UPDATE domains SET tls_status = 'provisioning', updated_at = NOW() WHERE id = $1",
        &[&domain_id],
    ).await.context("marking domain as provisioning")?;

    // ── ACME order ───────────────────────────────────────────────────────────
    let mut order = account.new_order(&NewOrder {
        identifiers: &[Identifier::Dns(domain.to_string())],
    }).await.context("creating ACME order")?;

    let authorizations = order.authorizations().await.context("fetching ACME authorizations")?;
    let auth = authorizations
        .first()
        .ok_or_else(|| anyhow::anyhow!("ACME returned no authorizations"))?;

    let challenge = auth
        .challenges
        .iter()
        .find(|c| c.r#type == ChallengeType::Http01)
        .ok_or_else(|| anyhow::anyhow!("no HTTP-01 challenge offered for {domain}"))?;

    let key_auth = order.key_authorization(challenge);
    let token = challenge.token.clone();
    let key_auth_str = key_auth.as_str().to_string();
    let challenge_url = challenge.url.clone();

    // ── Write challenge to DB for Pingora to serve ──────────────────────────
    db.execute(
        "INSERT INTO acme_challenges (token, key_authorization, domain)
         VALUES ($1, $2, $3)
         ON CONFLICT (token) DO UPDATE SET key_authorization = EXCLUDED.key_authorization",
        &[&token, &key_auth_str, &domain],
    ).await.context("writing ACME challenge to DB")?;

    // Signal to ACME that the challenge is ready to be validated.
    order.set_challenge_ready(&challenge_url).await.context("setting challenge ready")?;

    // ── Poll until validated (max ~90s) ─────────────────────────────────────
    let order_url = order.url().to_string();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
    loop {
        tokio::time::sleep(Duration::from_secs(5)).await;
        order = account.order(order_url.clone()).await.context("polling order status")?;
        match order.state().status {
            OrderStatus::Ready | OrderStatus::Valid => break,
            OrderStatus::Invalid => {
                // Clean up challenge before bailing.
                cleanup_challenge(&state.db, &token).await;
                bail!("ACME order for {domain} became invalid — check DNS/HTTP-01 reachability");
            }
            _ => {}
        }
        if tokio::time::Instant::now() >= deadline {
            cleanup_challenge(&state.db, &token).await;
            bail!("ACME validation for {domain} timed out after 90s");
        }
    }

    cleanup_challenge(&state.db, &token).await;

    // ── Generate private key + CSR ───────────────────────────────────────────
    let key_pair = KeyPair::generate().context("generating ECDSA key pair")?;
    let params   = CertificateParams::new(vec![domain.to_string()])
        .context("building certificate params")?;
    let csr = params.serialize_request(&key_pair).context("serializing CSR")?;

    // ── Finalize order and download cert chain ───────────────────────────────
    order.finalize(csr.der()).await.context("finalizing ACME order")?;

    // Poll for cert availability (usually immediate after finalize).
    let cert_chain_pem = {
        let wait = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(cert) = order.certificate().await.context("downloading certificate")? {
                break cert;
            }
            if tokio::time::Instant::now() >= wait {
                bail!("cert download timed out for {domain}");
            }
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
    };

    let key_pem = key_pair.serialize_pem();

    // ── Store cert + key in Vault ────────────────────────────────────────────
    envelope::store_cert(&state.vault, domain, &cert_chain_pem, &key_pem)
        .await
        .context("storing cert in vault")?;

    // Let's Encrypt issues 90-day certs. We record 89 days to give renewal
    // headroom without having to parse the X.509 not-after field.
    db.execute(
        "INSERT INTO domain_certs (domain_id, expires_at, updated_at)
         VALUES ($1, NOW() + INTERVAL '89 days', NOW())
         ON CONFLICT (domain_id) DO UPDATE
           SET expires_at = EXCLUDED.expires_at,
               updated_at = NOW()",
        &[&domain_id],
    ).await.context("upserting domain_cert")?;

    db.execute(
        "UPDATE domains SET tls_status = 'active', updated_at = NOW() WHERE id = $1",
        &[&domain_id],
    ).await.context("setting tls_status=active")?;

    // ── Notify proxy instances to hot-reload ─────────────────────────────────
    if let Ok(payload) = serde_json::to_vec(&CertProvisionedEvent { domain: domain.to_string() }) {
        state.nats_client.publish(SUBJECT_CERT_PROVISIONED, payload.into()).await.ok();
    }

    tracing::info!(domain, "cert_manager: TLS cert provisioned successfully");
    Ok(())
}

async fn cleanup_challenge(pool: &deadpool_postgres::Pool, token: &str) {
    if let Ok(db) = pool.get().await {
        db.execute("DELETE FROM acme_challenges WHERE token = $1", &[&token]).await.ok();
    }
}

async fn load_or_create_account(directory_url: &str, contact_email: &str) -> Result<Account> {
    // Try to load existing credentials first.
    if let Some(creds) = std::env::var("ACME_ACCOUNT_KEY")
        .ok()
        .and_then(|json| serde_json::from_str::<instant_acme::AccountCredentials>(&json).ok())
    {
        return Account::from_credentials(creds)
            .await
            .context("loading ACME account from ACME_ACCOUNT_KEY");
    }

    // No credentials — create a new account.
    let (account, new_creds) = Account::create(
        &NewAccount {
            contact:                  &[&format!("mailto:{contact_email}")],
            terms_of_service_agreed:  true,
            only_return_existing:     false,
        },
        directory_url,
        None,
    ).await.context("creating ACME account")?;

    let json = serde_json::to_string(&new_creds)
        .unwrap_or_else(|_| "(serialization failed)".to_string());
    tracing::warn!(
        "ACME account created — SAVE THIS as ACME_ACCOUNT_KEY env var before next restart:\n{}",
        json
    );

    Ok(account)
}

/// Read cert + key PEMs from Vault for a domain.
/// Returns `(cert_pem, key_pem)` as UTF-8 strings, or None if not found.
pub async fn read_cert_pair(
    vault: &common::vault::VaultClient,
    domain: &str,
) -> Result<Option<(String, String)>> {
    envelope::read_cert(vault, domain).await
}
