//! Cold-start construction of everything the handler needs.
//!
//! All of this happens once per execution environment and is then reused. That
//! matters more here than it usually would: this account's Lambda concurrency
//! limit is **10 across all functions**, and `api` shares that pool with
//! `generate`. A burst of document generations can leave the interactive
//! endpoint waiting for an execution slot, so every millisecond of per-request
//! work — and especially every per-request network call — is latency the user
//! sees at the worst moment.
//!
//! The concrete consequence is the JWT signing key: it is fetched from SSM here
//! and held for the life of the environment. Fetching per request would add an
//! SSM round trip to every authenticated call and would burn Parameter Store
//! throughput for a value that changes roughly never.

use std::time::Duration;

use aws_sdk_s3::presigning::PresigningConfig;
use trainer_core::config;
use trainer_core::error::{aws, Error, Result};
use trainer_core::store::Store;
use url::Url;
use webauthn_rs::prelude::{Passkey, Webauthn, WebauthnBuilder};

/// How long a presigned upload or download URL is valid.
///
/// Five minutes. Long enough to survive a slow phone upload starting, short
/// enough that a URL leaked through a screenshot, a shared link or a proxy log
/// is worthless by the time anyone finds it. The URL carries the caller's
/// authority — it is not additionally checked by anything — so its lifetime is
/// the whole of its access control.
pub const PRESIGN_TTL: Duration = Duration::from_secs(300);

/// Ceiling on a single upload, enforced by pinning `Content-Length` into the
/// signature. See `docs::create` for why this is a hard bound rather than a
/// suggestion.
///
/// Note this is *not* the limit that actually binds in practice: Bedrock's
/// Converse API caps a document block at roughly 4.5 MB, so `generate` refuses
/// anything larger long before this does. This value is the bound on what can
/// be *stored* — the thing that stops an idle S3 bill — while the Bedrock limit
/// is the bound on what can be processed.
pub const DEFAULT_MAX_UPLOAD_BYTES: i64 = 50 * 1024 * 1024;

pub struct AppState {
    pub store: Store,
    pub s3: aws_sdk_s3::Client,
    pub docs_bucket: String,

    /// Fetched once from SSM. Never logged, never returned, never included in
    /// an error message — `Error::Unauthorized` is deliberately opaque partly
    /// so that no code path can be tempted to explain itself with key material.
    pub jwt_key: JwtKeys,

    pub webauthn: Webauthn,
    /// The registered credentials, from `WEBAUTHN_CREDENTIALS`. Public data —
    /// a credential id and a public key are not secrets — which is why they can
    /// live in Lambda configuration rather than in Parameter Store.
    pub credentials: Vec<Passkey>,

    /// Exact origin allowed by CORS, and the origin WebAuthn assertions are
    /// checked against. One value, used for both, because a mismatch between
    /// them is a class of bug where the app appears to work in one browser and
    /// not another.
    pub origin: String,

    pub max_upload_bytes: i64,
}

/// Encoding and decoding keys derived from the same HMAC secret.
///
/// Kept together so there is no way to construct a state that signs with one
/// key and verifies with another.
pub struct JwtKeys {
    pub encoding: jsonwebtoken::EncodingKey,
    pub decoding: jsonwebtoken::DecodingKey,
}

impl AppState {
    pub async fn load() -> Result<Self> {
        let sdk = aws_config::load_from_env().await;

        let table = config::require("TABLE_NAME")?;
        let docs_bucket = config::require("DOCS_BUCKET")?;
        let origin = config::require("APP_ORIGIN")?;
        let rp_id = config::require("WEBAUTHN_RP_ID")?;
        let jwt_param = config::require("JWT_SIGNING_KEY_PARAM")?;
        let max_upload_bytes = config::parse_or("MAX_UPLOAD_BYTES", DEFAULT_MAX_UPLOAD_BYTES)?;

        let store = Store::new(aws_sdk_dynamodb::Client::new(&sdk), table);
        let s3 = aws_sdk_s3::Client::new(&sdk);

        let secret = fetch_signing_key(&aws_sdk_ssm::Client::new(&sdk), &jwt_param).await?;
        let jwt_key = JwtKeys {
            encoding: jsonwebtoken::EncodingKey::from_secret(secret.as_bytes()),
            decoding: jsonwebtoken::DecodingKey::from_secret(secret.as_bytes()),
        };
        // `secret` is dropped here. It is never placed in a struct field, never
        // formatted, and never passed to anything that takes `impl Debug`.
        drop(secret);

        let rp_origin = Url::parse(&origin)
            .map_err(|e| Error::Config(format!("APP_ORIGIN is not a URL: {e}")))?;

        // `allow_subdomains` is left at its default of false. The RP ID is the
        // apex (`marcusdunn.ca`) so that passkeys stay valid if the app ever
        // moves to another subdomain — an RP ID cannot be widened after
        // registration without re-enrolling every credential — but the *origin*
        // check stays exact. Those are different controls: the RP ID says which
        // credentials the authenticator will offer, the origin says which page
        // is allowed to use them. Widening the second because the first is wide
        // would let any subdomain, including one served by a future
        // misconfiguration, complete a login.
        let webauthn = WebauthnBuilder::new(&rp_id, &rp_origin)
            .map_err(|e| Error::Config(format!("webauthn configuration: {e}")))?
            .rp_name("marcusdunn.ca reading trainer")
            .build()
            .map_err(|e| Error::Config(format!("webauthn configuration: {e}")))?;

        let credentials = load_credentials()?;

        Ok(Self {
            store,
            s3,
            docs_bucket,
            jwt_key,
            webauthn,
            credentials,
            origin,
            max_upload_bytes,
        })
    }

    pub fn presigning(&self) -> Result<PresigningConfig> {
        PresigningConfig::expires_in(PRESIGN_TTL)
            .map_err(|e| Error::Config(format!("presigning config: {e}")))
    }
}

/// Read the JWT signing key from SSM Parameter Store.
///
/// `with_decryption` is required — the parameter is a `SecureString`, and
/// without the flag SSM returns the ciphertext rather than failing, which would
/// produce a function that signs tokens with a base64 blob and verifies them
/// consistently. It would *work*, and the key would be public.
///
/// The value never appears in a log line, an error, or a `Debug` impl. That is
/// why the error below reports only the parameter name.
async fn fetch_signing_key(ssm: &aws_sdk_ssm::Client, name: &str) -> Result<String> {
    let out = ssm
        .get_parameter()
        .name(name)
        .with_decryption(true)
        .send()
        .await
        .map_err(aws)?;

    let value = out
        .parameter
        .and_then(|p| p.value)
        .ok_or_else(|| Error::Config(format!("{name} has no value")))?;

    // A short key makes HMAC-SHA256 weak in a way nothing else here would
    // catch. The documented provisioning command uses `openssl rand -base64 48`,
    // which is 64 characters.
    if value.len() < 32 {
        return Err(Error::Config(format!(
            "{name} is too short to be a signing key"
        )));
    }

    Ok(value)
}

/// Parse `WEBAUTHN_CREDENTIALS`.
///
/// The value is a JSON array of serialized webauthn-rs `Passkey`s, i.e.
/// `[{"cred": {...}}, ...]`. There is no registration endpoint in this app by
/// design — a personal tool with exactly one user does not need a permanently
/// reachable "enrol a new credential" route, and having one is strictly a
/// liability — so credentials are produced out of band and pasted into
/// configuration.
///
/// **The stored `counter` should be left at whatever registration produced,
/// and is never updated by this app.** webauthn-rs only performs the signature
/// counter check when either the stored or the presented counter is non-zero;
/// synced passkeys (Android/Google Password Manager, iCloud Keychain) always
/// report zero because there is no single device to count on. Since nothing
/// here ever writes a counter back, the check is never armed, which is the
/// intended behaviour: enforcing it would lock out the actual authenticator
/// this app is used with, and it protects against cloning of *hardware* keys
/// that synced passkeys are not.
///
/// An empty list is refused. A deployment with no credentials cannot
/// authenticate anyone, and failing at cold start says so, whereas starting
/// successfully produces a login page that rejects every attempt for reasons
/// that look like a passkey problem.
fn load_credentials() -> Result<Vec<Passkey>> {
    let raw = config::require("WEBAUTHN_CREDENTIALS")?;

    let creds: Vec<Passkey> = serde_json::from_str(&raw).map_err(|e| {
        // The error deliberately does not echo `raw`. Credentials are public,
        // but echoing configuration into logs is a habit that eventually
        // echoes something that is not.
        Error::Config(format!(
            "WEBAUTHN_CREDENTIALS is not a JSON array of webauthn-rs Passkeys: {e}"
        ))
    })?;

    if creds.is_empty() {
        return Err(Error::Config(
            "WEBAUTHN_CREDENTIALS is empty; no one could log in".into(),
        ));
    }

    Ok(creds)
}
