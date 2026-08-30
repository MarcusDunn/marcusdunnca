//! One-shot passkey enrolment, for the deploy-then-enrol window only.
//!
//! **Why this exists.** `WEBAUTHN_CREDENTIALS` can only be produced by a
//! registration ceremony; a registration ceremony is bound to the relying
//! party's origin, which is `https://study.aws.marcusdunn.ca` and not
//! `localhost`; and the deployed function refuses to start with no credentials
//! (correctly — see `state::load_access`). That is a deadlock, and this module
//! is the smallest thing that breaks it: a deployment with *no* credentials
//! serves these two routes and nothing else, until the operator pastes the
//! result into configuration and applies.
//!
//! **Why it is not a standing registration endpoint.** Two independent gates,
//! both of which must hold:
//!
//! 1. `state::Access` is an enum, so "has credentials" and "serves registration"
//!    cannot both be true. Once a credential exists these routes are not
//!    disabled, they are *absent* — the router 404s them along with anything
//!    else it does not know.
//! 2. `REGISTRATION_TOKEN` must be presented in the `x-registration-token`
//!    header. Without it, the interval between `tofu apply` and the paste is an
//!    open enrolment endpoint on an unauthenticated Function URL, and whoever
//!    reaches it first owns the app — the ceremony does not care *who* is
//!    holding the phone.
//!
//! The token is therefore a bearer secret of the highest privilege this system
//! has, for as long as the window is open. It is never logged, never echoed and
//! never included in an error, and the comparison below does not short-circuit.

use serde::{Deserialize, Serialize};
use trainer_core::clock;
use trainer_core::error::{Error, Result};
use trainer_core::keys;
use trainer_core::model::ChallengeItem;
use webauthn_rs::prelude::{
    CreationChallengeResponse, Passkey, PasskeyRegistration, RegisterPublicKeyCredential, Uuid,
};

use crate::state::AppState;

/// The header carrying `REGISTRATION_TOKEN`.
///
/// A header rather than a query parameter, because `handle` logs the path of
/// every request and a query string is the classic way a secret ends up in a log
/// group. A header rather than a body field, because the check then runs before
/// anything parses attacker-supplied JSON.
pub const TOKEN_HEADER: &str = "x-registration-token";

/// How long a registration ceremony may take.
///
/// Five minutes, against sixty seconds for a login challenge. Enrolment is the
/// slower ceremony — choosing where to store the passkey, a system prompt, and
/// on a phone possibly a QR hop from another device — and nothing is queued
/// behind it. It is still short enough that a state row is worthless long before
/// anyone could find it, and the row is deleted on use regardless.
const REGISTRATION_TTL_SECS: i64 = 300;

/// What the passkey is called in the authenticator's list. Matches the JWT
/// `sub`, which is the only other place this app names its single user.
const USER_NAME: &str = "owner";
const USER_DISPLAY_NAME: &str = "marcusdunn.ca reading trainer";

/// `POST /auth/register/begin` response.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BeginResponse {
    /// Names the stored state row, and is handed straight back on finish.
    ///
    /// Not a security control. Every check that matters — challenge, origin, RP
    /// ID hash, user verification — happens inside webauthn-rs against the state
    /// this id loads, exactly as on the login path; a caller who supplies
    /// somebody else's id simply loads a state their attestation will not verify
    /// against. It is explicit here rather than recovered from `clientDataJSON`
    /// (as `auth::extract_challenge` does) because the client is fifty lines of
    /// hand-written JavaScript and does not need the archaeology.
    pub registration_id: String,

    /// webauthn-rs's own `CreationChallengeResponse`, flattened, so the body is
    /// `{"registrationId": ..., "publicKey": {...}}` and `publicKey` is verbatim
    /// what `navigator.credentials.create()` takes once its base64url fields are
    /// decoded to `ArrayBuffer`s.
    ///
    /// Passed through rather than translated into a DTO the way
    /// `auth::ChallengeResponse` is. That DTO exists to match a zod schema in the
    /// SPA; this response has exactly one client, `web/public/register.html`,
    /// written against this shape, and a hand-rolled copy of the creation
    /// options would be a second place for the algorithm list and the
    /// authenticator selection criteria to drift.
    #[serde(flatten)]
    pub options: CreationChallengeResponse,
}

/// `POST /auth/register/finish` request.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FinishRequest {
    pub registration_id: String,
    /// The raw `navigator.credentials.create()` result with its buffers
    /// base64url-encoded — webauthn-rs's `RegisterPublicKeyCredential` serde
    /// shape, including its `clientExtensionResults` alias. The same
    /// passthrough as `/auth/verify`, for the same reason: no translation means
    /// nothing to get wrong.
    pub credential: RegisterPublicKeyCredential,
}

/// `POST /auth/register/finish` response — the whole point of the exercise.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FinishResponse {
    /// One element of the `WEBAUTHN_CREDENTIALS` array: the serialized
    /// `Passkey`, `{"cred": {...}}`, byte-for-byte what `load_access` parses.
    pub credential: Passkey,

    /// The complete variable value for a single-device deployment, so the
    /// common case is a copy and a paste with no bracket-counting. A second
    /// device's element has to be merged into this array by hand — there is
    /// nowhere for this function to accumulate them, since the whole premise is
    /// that it has no writable credential store.
    pub webauthn_credentials: String,
}

/// Begin enrolment.
///
/// A fresh random user handle per ceremony, *not* a fixed one. A synced
/// authenticator (iCloud Keychain, Google Password Manager) treats a
/// discoverable credential as keyed by `(rp_id, user.id)` and will silently
/// replace an existing one on a repeat registration — so enrolling a phone with
/// the same handle the laptop used would invalidate the laptop's credential
/// while this function, which never writes credentials back, went on serving the
/// old public key. Distinct handles cost nothing here: nothing in this app reads
/// `userHandle`, and `verify_assertion` matches on credential id alone.
///
/// `exclude_credentials` is `None` because in this mode there are, by
/// construction, no credentials to exclude.
pub async fn begin(state: &AppState) -> Result<BeginResponse> {
    let (ccr, reg_state) = state
        .webauthn
        .start_passkey_registration(Uuid::new_v4(), USER_NAME, USER_DISPLAY_NAME, None)
        .map_err(|e| {
            tracing::error!(error = ?e, "failed to start passkey registration");
            // `Config` rather than `Unauthorized`: the only ways this call fails
            // are an RP ID or origin the library will not accept, and during a
            // bootstrap a 401 would send the operator hunting for a token
            // problem that is not there. `http::error_response` logs it and
            // returns a generic 500.
            Error::Config("could not start passkey registration".into())
        })?;

    let registration_id = Uuid::new_v4().to_string();

    let item = ChallengeItem {
        pk: keys::AUTH_PK.to_string(),
        sk: keys::registration_sk(&registration_id),
        state: serde_json::to_string(&reg_state)?,
        expires_at: clock::unix_now() + REGISTRATION_TTL_SECS,
    };

    state.store.put_registration(&item).await?;

    Ok(BeginResponse {
        registration_id,
        options: ccr,
    })
}

/// Complete enrolment and hand the credential back.
///
/// The ordering mirrors `auth::verify_assertion` deliberately: consume the state
/// row first, check expiry in this process rather than trusting a TTL sweep,
/// then let webauthn-rs check the attestation, the origin and the RP ID hash.
/// Nothing here is written to the table — the output is configuration, and it
/// leaves via the response body only.
///
/// The result is round-tripped through the exact parse `load_access` performs
/// before it is returned. That check is the difference between an operator
/// discovering a serialization mismatch now, with the ceremony still on screen,
/// and discovering it after an apply, from a function that will not start.
pub async fn finish(state: &AppState, request: FinishRequest) -> Result<FinishResponse> {
    let Some(stored) = state
        .store
        .take_registration(&request.registration_id)
        .await?
    else {
        // Never issued, already used, or reaped.
        return Err(Error::Invalid(
            "no such registration; start a new one".into(),
        ));
    };

    if stored.expires_at < clock::unix_now() {
        return Err(Error::Invalid(
            "registration expired; start a new one".into(),
        ));
    }

    let reg_state: PasskeyRegistration = serde_json::from_str(&stored.state)?;

    let passkey = state
        .webauthn
        .finish_passkey_registration(&request.credential, &reg_state)
        .map_err(|e| {
            // The library's error names which check failed and contains no key
            // material. Unlike the login path this is returned to the caller as
            // well as logged: the caller here is the operator, holding the
            // token, and "registration rejected" with no reason is a bad way to
            // spend a ceremony.
            tracing::debug!(error = ?e, "registration rejected");
            Error::Invalid(format!("registration rejected: {e}"))
        })?;

    let webauthn_credentials = serde_json::to_string(&vec![&passkey])?;
    let reparsed: Vec<Passkey> = serde_json::from_str(&webauthn_credentials).map_err(|e| {
        tracing::error!(error = %e, "registered passkey does not round-trip");
        Error::Config("registered passkey does not round-trip as WEBAUTHN_CREDENTIALS".into())
    })?;
    debug_assert_eq!(reparsed.len(), 1);

    tracing::info!("passkey registered; paste the credential into WEBAUTHN_CREDENTIALS");

    Ok(FinishResponse {
        credential: passkey,
        webauthn_credentials,
    })
}

/// Check the `x-registration-token` header against `REGISTRATION_TOKEN`.
///
/// A missing header is treated as an empty token rather than as its own error,
/// so there is exactly one rejection path and one response. Nothing about the
/// presented value — not its content, not its length, not whether it was present
/// at all — reaches a log line or the response body.
pub fn require_token(expected: &str, presented: Option<&str>) -> Result<()> {
    if constant_time_eq(
        presented.unwrap_or_default().as_bytes(),
        expected.as_bytes(),
    ) {
        return Ok(());
    }

    tracing::info!("registration attempt rejected");
    Err(Error::Unauthorized)
}

/// Compare two byte strings without an early exit on the first differing byte.
///
/// `a == b` on slices is a `memcmp`, which returns as soon as it finds a
/// difference. Over a network that timing signal is usually unmeasurable, but
/// this token guards enrolment on an endpoint anyone can reach as often as they
/// like, and the fix costs four lines. `black_box` keeps the accumulator from
/// being optimised back into a short-circuit.
///
/// The length is compared first and therefore leaks, which is the standard
/// trade: the token is a fixed-length random string, so its length is not the
/// secret.
///
/// Hand-rolled rather than pulled from `subtle`: this is the only constant-time
/// comparison in the codebase and it is four lines against a dependency in a
/// binary whose size is a cold-start cost.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }

    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }

    std::hint::black_box(diff) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_matching_token_is_accepted_and_nothing_else_is() {
        let token = "0123456789abcdef0123456789abcdef";

        assert!(require_token(token, Some(token)).is_ok());

        // Absent, empty, a prefix, an extension, and a one-byte difference at
        // each end — the last two being what a comparison that stopped early
        // would tell an attacker apart.
        for presented in [
            None,
            Some(""),
            Some("0123456789abcdef0123456789abcde"),
            Some("0123456789abcdef0123456789abcdef0"),
            Some("1123456789abcdef0123456789abcdef"),
            Some("0123456789abcdef0123456789abcdeg"),
        ] {
            assert!(
                require_token(token, presented).is_err(),
                "accepted {presented:?}"
            );
        }
    }

    /// Pins the request contract against `web/public/register.html`. That page
    /// has no build step, no schema and no type checking, so this test is the
    /// only thing that fails if the shape it posts and the shape this module
    /// parses ever drift — and the alternative to failing here is failing during
    /// a ceremony on a deployment nobody can log into.
    ///
    /// The base64url payloads are deliberately nonsense: `Base64UrlSafeData`
    /// only decodes bytes, and everything that inspects those bytes lives past
    /// this boundary.
    #[test]
    fn the_finish_request_matches_what_the_page_posts() {
        let body = serde_json::json!({
            "registrationId": "8f14e45f-ceea-467a-9c3b-4d5f6a7b8c9d",
            "credential": {
                "id": "AAECAw",
                "rawId": "AAECAw",
                "type": "public-key",
                "clientExtensionResults": { "credProps": { "rk": true } },
                "response": {
                    "attestationObject": "BAUGBw",
                    "clientDataJSON": "CAkKCw",
                    "transports": ["internal", "hybrid"]
                }
            }
        });

        let parsed: FinishRequest = serde_json::from_value(body).expect("page body must parse");
        assert_eq!(parsed.credential.type_, "public-key");
        // `transports` is what makes a login on a phone offer the phone. It
        // arrives here or it is lost for good — nothing re-registers.
        assert!(parsed.credential.response.transports.is_some());
    }

    /// Pins the response contract in the other direction: `#[serde(flatten)]` on
    /// `CreationChallengeResponse` must produce a sibling `publicKey` object, not
    /// a nested `options` one, because the page passes `begin.publicKey`
    /// straight to `navigator.credentials.create()`.
    #[test]
    fn the_begin_response_puts_public_key_at_the_top_level() {
        let options: CreationChallengeResponse = serde_json::from_value(serde_json::json!({
            "publicKey": {
                "rp": { "name": "marcusdunn.ca reading trainer", "id": "marcusdunn.ca" },
                "user": { "id": "AAECAw", "name": USER_NAME, "displayName": USER_DISPLAY_NAME },
                "challenge": "BAUGBw",
                "pubKeyCredParams": [{ "type": "public-key", "alg": -7 }]
            }
        }))
        .expect("fixture must parse");

        let body = serde_json::to_value(BeginResponse {
            registration_id: "8f14e45f-ceea-467a-9c3b-4d5f6a7b8c9d".to_string(),
            options,
        })
        .expect("must serialize");

        assert!(body.get("registrationId").is_some());
        assert!(body.get("publicKey").is_some(), "flatten regressed: {body}");
        // base64url strings, not arrays of numbers: the page's
        // `base64UrlToBytes` is written against strings.
        assert!(body["publicKey"]["challenge"].is_string());
        assert!(body["publicKey"]["user"]["id"].is_string());
    }

    #[test]
    fn constant_time_eq_agrees_with_equality() {
        assert!(constant_time_eq(b"", b""));
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert!(!constant_time_eq(b"", b"a"));
    }
}
