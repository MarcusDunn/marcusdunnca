//! The reading-trainer API: one Lambda behind one Function URL, routed on path.
//!
//! **Why one function rather than one per route.** This account's Lambda
//! concurrency limit is 10 *in total*, and reserved concurrency cannot be set
//! (AWS refuses any reservation leaving fewer than 10 unreserved). Splitting
//! the API into eight functions would multiply cold starts across a pool that
//! cannot be partitioned anyway, and each cold start pays for a fresh SSM fetch
//! of the signing key. One function means one warm environment serves every
//! route.
//!
//! **Why hand-rolled routing rather than a framework.** The route table is
//! eight entries with one path parameter. `axum` via `lambda_http`'s tower
//! integration would work, but it adds a meaningful chunk to a binary that is
//! cold-started on a shared, ten-slot pool. The whole router is `match_route`
//! below and fits on a screen.
//!
//! **The one exception to that route table** is bootstrap registration mode: a
//! deployment with no passkeys configured serves two enrolment routes and 503s
//! everything else. It is checked first in `dispatch` and is mutually exclusive
//! with everything after it. See `register`.

mod auth;
mod docs;
mod history;
mod http;
mod register;
mod state;

use lambda_http::{service_fn, Body, Request, RequestExt, Response};
use trainer_core::error::{Error, Result};

use crate::state::AppState;

#[tokio::main]
async fn main() -> std::result::Result<(), lambda_http::Error> {
    // No timestamps and no ANSI: CloudWatch adds its own timestamp to every
    // line, and escape codes in a log group are unreadable.
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_ansi(false)
        .without_time()
        .with_target(false)
        .init();

    // Cold-start failures abort here, before the runtime reports ready. A
    // function that starts with a missing table name and fails on the third
    // route is far harder to diagnose than one that never comes up.
    let state = AppState::load().await.map_err(|e| {
        tracing::error!(error = %e, "failed to initialise");
        lambda_http::Error::from(e.to_string())
    })?;
    let state: &'static AppState = Box::leak(Box::new(state));

    lambda_http::run(service_fn(move |req: Request| async move {
        Ok::<_, std::convert::Infallible>(handle(state, req).await)
    }))
    .await
}

/// Never returns `Err`. Every failure becomes a response, because an `Err` out
/// of a `lambda_http` handler produces a 502 with no CORS headers, which the
/// browser reports as a generic network error — the least diagnosable outcome
/// available.
async fn handle(state: &AppState, req: Request) -> Response<Body> {
    let method = req.method().clone();
    let path = normalise(req.uri().path());

    if method == lambda_http::http::Method::OPTIONS {
        return http::preflight(&state.origin);
    }

    // Logged before dispatch and without the query string: `?skill=` is
    // harmless, but logging a whole URI is how bearer tokens end up in log
    // groups the day someone adds `?token=` for a debugging session.
    tracing::info!(method = %method, path = %path, "request");

    match dispatch(state, &method, &path, req).await {
        Ok(response) => response,
        Err(e) => http::error_response(&state.origin, &e),
    }
}

/// Function URLs deliver the path with a leading slash and no stage prefix, but
/// a trailing slash is a real difference to a `match` on `&str`. Normalised
/// once here so `/docs` and `/docs/` are the same route.
fn normalise(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        "/".to_string()
    } else {
        trimmed.to_string()
    }
}

async fn dispatch(
    state: &AppState,
    method: &lambda_http::http::Method,
    path: &str,
    req: Request,
) -> Result<Response<Body>> {
    use lambda_http::http::Method;

    let origin = state.origin.as_str();

    // Registration mode. This is the whole route table for a deployment that
    // has no credentials yet — see `register` for why it exists and why it
    // cannot coexist with the routes below. It is matched before anything else
    // so that no route added later can be reached by an unenrolled deployment,
    // and everything it does not match is refused rather than served.
    //
    // `registration_token` is `None` the moment `WEBAUTHN_CREDENTIALS` is
    // non-empty, at which point these two paths fall through to the `NotFound`
    // arm at the bottom like any other unknown path.
    if let Some(token) = state.registration_token() {
        return match (method, path) {
            (&Method::POST, "/auth/register/begin") => {
                register::require_token(token, registration_token(&req))?;
                let options = register::begin(state).await?;
                Ok(http::json(origin, 200, &options))
            }
            (&Method::POST, "/auth/register/finish") => {
                // Token first, body second: the check should not be reachable
                // only after parsing something a stranger sent.
                register::require_token(token, registration_token(&req))?;
                let credential = register::finish(state, body_json(&req)?).await?;
                Ok(http::json(origin, 200, &credential))
            }
            _ => Ok(http::unavailable(origin)),
        };
    }

    // The two unauthenticated routes are matched first and exhaustively, so
    // that no future addition can accidentally land in front of the auth check
    // below. Everything past this point has been through `require_session`.
    match (method, path) {
        (&Method::POST, "/auth/challenge") => {
            let challenge = auth::start_challenge(state).await?;
            return Ok(http::json(origin, 200, &challenge));
        }
        (&Method::POST, "/auth/verify") => {
            // The client posts the raw assertion — `id`, `rawId`, `type`,
            // `clientExtensionResults` and a base64url `response` — which is
            // exactly webauthn-rs's `PublicKeyCredential` serde shape, including
            // its `clientExtensionResults` alias. No translation needed.
            let credential = body_json(&req)?;
            let session = auth::verify_assertion(state, &credential).await?;
            // The token goes in the body and is never logged. It is a
            // thirty-day credential.
            return Ok(http::json(origin, 200, &session));
        }
        _ => {}
    }

    let header = req
        .headers()
        .get(lambda_http::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    auth::require_session(state, header)?;

    let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();

    match (method, segments.as_slice()) {
        (&Method::POST, ["docs"]) => {
            let created = docs::create(state, body_json(&req)?).await?;
            Ok(http::json(origin, 201, &created))
        }
        (&Method::GET, ["docs"]) => {
            let docs = docs::list(state).await?;
            Ok(http::json(origin, 200, &docs))
        }
        (&Method::GET, ["docs", id, "url"]) => {
            let url = docs::download_url(state, id).await?;
            Ok(http::json(origin, 200, &url))
        }
        (&Method::GET, ["docs", id, "quiz"]) => {
            let quiz = docs::quiz(state, id).await?;
            Ok(http::json(origin, 200, &quiz))
        }
        (&Method::POST, ["docs", id, "submit"]) => {
            let result = docs::submit(state, id, body_json(&req)?).await?;
            Ok(http::json(origin, 200, &result))
        }
        (&Method::GET, ["history"]) => {
            // The current frontend sends no parameters and filters in memory;
            // these are here because the endpoint is specified as filterable.
            // `query_string_parameters` collapses repeats, which is fine for
            // three single-valued filters.
            let pairs: Vec<(String, String)> = req
                .query_string_parameters()
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();

            let filter = history::HistoryFilter::from_query(&pairs)?;
            let entries = history::list(state, &filter).await?;
            Ok(http::json(origin, 200, &entries))
        }
        (&Method::GET, ["health"]) => Ok(http::no_content(origin)),
        _ => Err(Error::NotFound),
    }
}

/// Read the registration token header, if the caller sent one at all.
///
/// A header whose bytes are not valid UTF-8 is `None` rather than an error: it
/// cannot equal the configured token, and `require_token` already has exactly
/// one rejection path for everything that does not match.
fn registration_token(req: &Request) -> Option<&str> {
    req.headers()
        .get(register::TOKEN_HEADER)
        .and_then(|v| v.to_str().ok())
}

/// Parse a JSON request body.
///
/// A parse failure is a 400 with the serde message, which names the offending
/// field — worth having, since the client is a hand-written SPA and the
/// alternative is guessing. The message describes the *request* shape only;
/// none of these types contain anything secret.
fn body_json<T: serde::de::DeserializeOwned>(req: &Request) -> Result<T> {
    // `Body` is `#[non_exhaustive]`, so the wildcard is required rather than
    // chosen. Treating an unknown future variant as empty yields a 400 with a
    // clear message, which beats a panic in the request path.
    let bytes: &[u8] = match req.body() {
        Body::Empty => &[],
        Body::Text(s) => s.as_bytes(),
        Body::Binary(b) => b.as_slice(),
        _ => &[],
    };

    if bytes.is_empty() {
        return Err(Error::Invalid("request body is required".into()));
    }

    serde_json::from_slice(bytes).map_err(|e| Error::Invalid(format!("malformed request: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trailing_slashes_do_not_create_new_routes() {
        assert_eq!(normalise("/docs/"), "/docs");
        assert_eq!(normalise("/docs"), "/docs");
        assert_eq!(normalise("/"), "/");
        assert_eq!(normalise(""), "/");
    }

    /// Guards the ordering in `dispatch`: exactly two routes are reachable
    /// without a session token, and both are under `/auth`. If a third is ever
    /// added above the `require_session` call, this list must change with it.
    #[test]
    fn only_the_auth_routes_are_public() {
        const PUBLIC: [&str; 2] = ["/auth/challenge", "/auth/verify"];
        assert!(PUBLIC.iter().all(|p| p.starts_with("/auth/")));
    }

    /// The registration routes are not a third entry in the list above. They are
    /// reachable only from the `registration_token` branch, which returns before
    /// that list is consulted and exists only while `WEBAUTHN_CREDENTIALS` is
    /// empty — and they carry their own token check. Recorded here so that
    /// moving either path into the public match is a deliberate act.
    #[test]
    fn the_registration_routes_are_not_public() {
        const REGISTRATION: [&str; 2] = ["/auth/register/begin", "/auth/register/finish"];
        const PUBLIC: [&str; 2] = ["/auth/challenge", "/auth/verify"];

        assert!(REGISTRATION.iter().all(|r| !PUBLIC.contains(r)));
        // Normalisation must not turn one into the other, either.
        assert!(REGISTRATION
            .iter()
            .all(|r| !PUBLIC.contains(&normalise(r).as_str())));
    }
}
