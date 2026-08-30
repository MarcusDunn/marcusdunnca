//! Response construction, CORS, and error mapping.
//!
//! CORS is applied here rather than in the Function URL's own `cors` block. A
//! Function URL's CORS configuration and the WebAuthn relying-party origin must
//! be the same string — if they drift, the symptom is a browser that completes
//! a passkey prompt and then fails the fetch, which looks like a passkey
//! problem and is not. Reading both from `APP_ORIGIN` in one process makes that
//! drift impossible.
//!
//! Note `Access-Control-Allow-Origin` is the exact origin, never `*`. It is
//! paired with `Allow-Credentials: false` and a bearer token in a header
//! (rather than a cookie), so the app is not vulnerable to CSRF by
//! construction — but a wildcard would still let any page read responses, and
//! `GET /docs/:id/quiz` is not something to hand out.

use lambda_http::{Body, Response};
use serde::Serialize;
use trainer_core::error::Error;

pub fn json<T: Serialize>(origin: &str, status: u16, body: &T) -> Response<Body> {
    let payload = match serde_json::to_string(body) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "failed to serialize response");
            return base(origin, 500)
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"message":"internal error","error":"internal error"}"#,
                ))
                .unwrap_or_default();
        }
    };

    base(origin, status)
        .header("content-type", "application/json")
        .body(Body::from(payload))
        .unwrap_or_default()
}

pub fn no_content(origin: &str) -> Response<Body> {
    base(origin, 204).body(Body::Empty).unwrap_or_default()
}

/// Map a domain error onto a status code and a body that is safe to show.
///
/// `Aws` and `Json` deliberately collapse to a generic 500 message. Their
/// payloads can contain request ids, ARNs and account numbers; those belong in
/// CloudWatch, not in a browser. The *log* keeps the detail.
pub fn error_response(origin: &str, err: &Error) -> Response<Body> {
    let (status, message) = match err {
        Error::Unauthorized => (401, "unauthorized".to_string()),
        Error::NotFound => (404, "not found".to_string()),
        Error::Invalid(m) => (400, m.clone()),
        Error::QuotaExceeded(m) => (429, m.clone()),
        Error::Config(m) => {
            tracing::error!(error = %m, "configuration error at request time");
            (500, "internal error".to_string())
        }
        Error::Aws(m) => {
            tracing::error!(error = %m, "aws call failed");
            (500, "internal error".to_string())
        }
        Error::Json(e) => {
            tracing::error!(error = %e, "json error");
            (500, "internal error".to_string())
        }
    };

    // `message` is the key `api.ts` reads first (`data.message ?? data.error`).
    // Both are emitted because `ApiErrorBody` accepts either and a second key
    // costs nothing next to the cost of a client that shows "Request failed
    // with status 400" instead of the reason.
    json(
        origin,
        status,
        &serde_json::json!({ "message": message, "error": message }),
    )
}

fn base(origin: &str, status: u16) -> lambda_http::http::response::Builder {
    Response::builder()
        .status(status)
        .header("access-control-allow-origin", origin)
        // The token lives in a header, not a cookie, so credentialed CORS is
        // not needed. Leaving it off means a malicious page cannot make the
        // browser attach anything on the user's behalf.
        .header("access-control-allow-credentials", "false")
        // Responses vary by Origin because the header above is computed. Absent
        // this, a shared cache could serve one origin's response to another.
        .header("vary", "Origin")
        // The API returns only JSON, but a response that a browser can be
        // convinced to interpret as HTML is an XSS vector on the API's own
        // origin, which is exactly where the Function URL lives.
        .header("x-content-type-options", "nosniff")
        .header("cache-control", "no-store")
}

/// Preflight. `max-age` is generous because the answer never changes and every
/// preflight is a cold-start-eligible invocation against a pool of ten.
pub fn preflight(origin: &str) -> Response<Body> {
    base(origin, 204)
        .header("access-control-allow-methods", "GET, POST, OPTIONS")
        .header(
            "access-control-allow-headers",
            "authorization, content-type",
        )
        .header("access-control-max-age", "600")
        .body(Body::Empty)
        .unwrap_or_default()
}
