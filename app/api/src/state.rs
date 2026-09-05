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
    /// Whether this deployment can log anyone in yet, and what it serves if it
    /// cannot. See [`Access`].
    pub access: Access,

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

/// The two states this function can come up in.
///
/// An enum rather than a `Vec<Passkey>` plus an `Option<String>`, because the
/// property that matters is that they are **mutually exclusive**: enrolment must
/// be impossible the moment a real credential exists. Expressed as two fields,
/// that invariant would live in whichever `if` happened to check it, and the
/// failure mode of getting it wrong is a permanently reachable "add a passkey"
/// endpoint on an unauthenticated Function URL. Expressed as this enum, it is
/// established once in [`load_access`] and cannot be violated downstream.
pub enum Access {
    /// The normal state: at least one credential, no way to add another.
    Live { credentials: Vec<Passkey> },

    /// The bootstrap state, and the answer to a genuine deadlock: the only way
    /// to produce a `WEBAUTHN_CREDENTIALS` value is to run a registration
    /// ceremony, the ceremony is bound to origin `APP_ORIGIN` so it cannot be
    /// run from a laptop, and the deployed function refuses to start without
    /// credentials. So a deployment with none — and only a deployment with
    /// none — serves the two `/auth/register/*` routes and nothing else.
    ///
    /// The token is what stops that window from being open enrolment for
    /// whoever finds the URL first. It is a bearer secret: whoever presents it
    /// owns the app.
    Registration { token: String },
}

impl AppState {
    pub async fn load() -> Result<Self> {
        let sdk = aws_config::load_from_env().await;

        let table = config::require("TABLE_NAME")?;
        let auth_table = config::require("AUTH_TABLE_NAME")?;
        let docs_bucket = config::require("DOCS_BUCKET")?;
        let origin = config::require("APP_ORIGIN")?;
        let rp_id = config::require("WEBAUTHN_RP_ID")?;
        let jwt_param = config::require("JWT_SIGNING_KEY_PARAM")?;
        let max_upload_bytes = config::parse_or("MAX_UPLOAD_BYTES", DEFAULT_MAX_UPLOAD_BYTES)?;

        // Ceremony state goes in its own table. See `Store::with_auth_table`
        // for why the routes that need no session must not write where the
        // routes that do go on to scan.
        let store =
            Store::new(aws_sdk_dynamodb::Client::new(&sdk), table).with_auth_table(auth_table);
        let s3 = aws_sdk_s3::Client::new(&sdk);
        let ssm = aws_sdk_ssm::Client::new(&sdk);

        let secret = fetch_secret(&ssm, &jwt_param).await?;
        // A short key makes HMAC-SHA256 weak in a way nothing else here would
        // catch. The documented provisioning command uses `openssl rand -base64 48`,
        // which is 64 characters.
        if secret.len() < MIN_SIGNING_KEY_LEN {
            return Err(Error::Config(format!(
                "{jwt_param} is too short to be a signing key"
            )));
        }
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

        let access = load_access(&ssm).await?;

        Ok(Self {
            store,
            s3,
            docs_bucket,
            jwt_key,
            webauthn,
            access,
            origin,
            max_upload_bytes,
        })
    }

    pub fn presigning(&self) -> Result<PresigningConfig> {
        PresigningConfig::expires_in(PRESIGN_TTL)
            .map_err(|e| Error::Config(format!("presigning config: {e}")))
    }

    /// The passkeys a login may be attempted against.
    ///
    /// Empty in registration mode, which the router makes unreachable from the
    /// login routes — and which would fail closed anyway: an assertion against
    /// an empty credential set matches nothing.
    pub fn credentials(&self) -> &[Passkey] {
        match &self.access {
            Access::Live { credentials } => credentials,
            Access::Registration { .. } => &[],
        }
    }

    /// `Some` only in registration mode. This is the single test the router uses
    /// to decide whether to serve the app or the enrolment ceremony, so there is
    /// exactly one place where "is this deployment enrolled?" is answered.
    pub fn registration_token(&self) -> Option<&str> {
        match &self.access {
            Access::Live { .. } => None,
            Access::Registration { token } => Some(token),
        }
    }
}

/// Shortest signing key this will accept. See the check in [`AppState::load`].
const MIN_SIGNING_KEY_LEN: usize = 32;

/// Read a `SecureString` from SSM Parameter Store.
///
/// `with_decryption` is required — without the flag SSM returns the ciphertext
/// rather than failing, which for the signing key would produce a function that
/// signs tokens with a base64 blob and verifies them consistently. It would
/// *work*, and the key would be public.
///
/// The value never appears in a log line, an error, or a `Debug` impl. That is
/// why the error below reports only the parameter name. Both secrets this
/// function reads grant everything — one mints sessions, the other enrols the
/// passkey that mints them — so there is no "safe to show" tier.
async fn fetch_secret(ssm: &aws_sdk_ssm::Client, name: &str) -> Result<String> {
    let out = ssm
        .get_parameter()
        .name(name)
        .with_decryption(true)
        .send()
        .await
        .map_err(aws)?;

    out.parameter
        .and_then(|p| p.value)
        .ok_or_else(|| Error::Config(format!("{name} has no value")))
}

/// Shortest registration token this will accept.
///
/// The registration routes need no session, and the gateway throttles them to
/// one request a second, so the token is the only thing between an unenrolled
/// deployment and whoever guesses it. Thirty-two characters is what
/// `openssl rand -base64 24` produces, and refusing anything shorter at cold
/// start is the only moment this code gets to have an opinion — after that the
/// value is just a string being compared.
const MIN_REGISTRATION_TOKEN_LEN: usize = 32;

/// Decide between serving the app and serving the enrolment ceremony.
///
/// `WEBAUTHN_CREDENTIALS` is a JSON array of serialized webauthn-rs `Passkey`s,
/// i.e. `[{"cred": {...}}, ...]`. There is no registration endpoint in this app
/// by design — a personal tool with exactly one user does not need a
/// permanently reachable "enrol a new credential" route, and having one is
/// strictly a liability — so credentials are produced out of band and pasted
/// into configuration.
///
/// The exception is the bootstrap, and it is narrow: a *non-empty* list is
/// checked first and wins unconditionally, so the registration token is dead
/// configuration the instant a credential exists — it is not even fetched.
/// The reverse — clearing `WEBAUTHN_CREDENTIALS` — reopens enrolment, which is
/// the correct behaviour for a deployment that can no longer authenticate
/// anyone but is worth knowing.
///
/// The token is an SSM `SecureString` named by `REGISTRATION_TOKEN_PARAM`,
/// read here rather than taken from an environment variable. An environment
/// variable is Lambda configuration and Terraform state, both of which the
/// plan role can read from any pull request, and state keeps its version
/// history for ninety days. A parameter under `/secret/` is readable by this
/// function's role and by nothing CI holds. It is resolved only on this
/// branch, so a deployment holding credentials never needs the parameter, or
/// the variable naming it, to exist.
///
/// A *malformed* list is an error in both modes. Falling through to registration
/// mode on a parse failure would turn a typo in configuration into an open
/// enrolment window on a running, previously-enrolled app.
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
/// An empty list with no readable registration token is still refused. A
/// deployment with no credentials cannot authenticate anyone, and failing at
/// cold start says so, whereas starting successfully produces a login page that
/// rejects every attempt for reasons that look like a passkey problem.
async fn load_access(ssm: &aws_sdk_ssm::Client) -> Result<Access> {
    // Not `config::require`: absent and `"[]"` are the same state — no
    // credentials — and Terraform's default for this variable is `"[]"`, so
    // both spellings reach here on a first deploy.
    let raw = std::env::var("WEBAUTHN_CREDENTIALS").unwrap_or_default();
    let credentials = parse_credentials(&raw)?;

    if !credentials.is_empty() {
        return Ok(Access::Live { credentials });
    }

    // Only now. Everything about registration mode is resolved lazily so that
    // a deployment holding credentials depends on none of it.
    let param = config::require("REGISTRATION_TOKEN_PARAM").map_err(|e| {
        Error::Config(format!(
            "WEBAUTHN_CREDENTIALS is empty; no one could log in, and registration mode \
             is unavailable because {e}"
        ))
    })?;

    let token = fetch_secret(ssm, &param).await.map_err(|e| {
        // Names the parameter, never its value. Put the parameter to start in
        // registration mode; the message says which one.
        Error::Config(format!(
            "WEBAUTHN_CREDENTIALS is empty; no one could log in, and the registration \
             token could not be read: {e}"
        ))
    })?;

    registration_access(&token)
}

/// Parse `WEBAUTHN_CREDENTIALS`. Absent, empty and `[]` are all "none".
fn parse_credentials(raw: &str) -> Result<Vec<Passkey>> {
    if raw.trim().is_empty() {
        return Ok(Vec::new());
    }

    serde_json::from_str(raw).map_err(|e| {
        // The error deliberately does not echo `raw`. Credentials are public,
        // but echoing configuration into logs is a habit that eventually echoes
        // something that is not.
        Error::Config(format!(
            "WEBAUTHN_CREDENTIALS is not a JSON array of webauthn-rs Passkeys: {e}"
        ))
    })
}

/// Registration mode, from the token as read.
///
/// Length only. The value is never logged, never returned and never included
/// in an error, for the same reason the JWT secret is not: this one grants
/// enrolment, which grants everything.
fn registration_access(token: &str) -> Result<Access> {
    let token = token.trim();

    if token.len() < MIN_REGISTRATION_TOKEN_LEN {
        return Err(Error::Config(format!(
            "the registration token is shorter than {MIN_REGISTRATION_TOKEN_LEN} characters"
        )));
    }

    // Deliberately loud, and the one log line that says this is happening. An
    // operator who sees this on a deployment that was working has just wiped
    // their credentials.
    tracing::warn!("no credentials configured; serving passkey registration only");

    Ok(Access::Registration {
        token: token.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One real, public credential record — the shape `register::finish` hands
    /// back and `infra/credentials.auto.tfvars` commits. A public key and a
    /// credential id cannot forge an assertion, which is why it can live here.
    const ONE_CREDENTIAL: &str = r#"[{"cred":{"cred_id":"3_n2sKu6yizY440mVTQ3Zw","cred":{"type_":"ES256","key":{"EC_EC2":{"curve":"SECP256R1","x":"aQtimriv0Re14d4vq_2hkS6hIVCzTeNzGorRw2DzWc0","y":"g-cKQpqKr0XHUtN7PXJsvbXGZEbBJy0AKnS1FcnQIeo"}}},"counter":0,"transports":null,"user_verified":true,"backup_eligible":true,"backup_state":true,"registration_policy":"required","extensions":{"cred_protect":"Ignored","hmac_create_secret":"NotRequested","appid":"NotRequested","cred_props":{"Unsigned":{"rk":true}}},"attestation":{"data":"None","metadata":"None"},"attestation_format":"none"}}]"#;

    #[test]
    fn absent_empty_and_empty_list_are_all_no_credentials() {
        for raw in ["", "   ", "[]", " [] "] {
            assert!(
                parse_credentials(raw).expect("parses").is_empty(),
                "{raw:?}"
            );
        }
    }

    #[test]
    fn a_real_credential_record_parses() {
        assert_eq!(parse_credentials(ONE_CREDENTIAL).expect("parses").len(), 1);
    }

    /// A typo in configuration must not fall through to registration mode on a
    /// deployment that was enrolled a moment ago.
    #[test]
    fn a_malformed_list_is_an_error_not_an_open_enrolment_window() {
        for raw in ["{", "[{}]", "null", "\"[]\""] {
            assert!(
                matches!(parse_credentials(raw), Err(Error::Config(_))),
                "{raw:?} was accepted"
            );
        }
    }

    #[test]
    fn a_short_token_is_refused_and_a_long_one_is_kept_trimmed() {
        assert!(matches!(
            registration_access("0123456789abcdef0123456789abcde"),
            Err(Error::Config(_))
        ));
        // Padding does not count toward the length.
        assert!(matches!(
            registration_access("   0123456789abcdef0123456789abcde   "),
            Err(Error::Config(_))
        ));

        let access = registration_access("  0123456789abcdef0123456789abcdef\n").expect("accepted");
        match access {
            Access::Registration { token } => {
                assert_eq!(token, "0123456789abcdef0123456789abcdef");
            }
            Access::Live { .. } => panic!("no credentials cannot be live"),
        }
    }
}
