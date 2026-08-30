//! One error type shared by both handlers.
//!
//! The variants are chosen so the API crate can map them to HTTP status codes
//! without inspecting strings, and so the generate crate can decide whether a
//! failure is the document's fault (`Invalid` — write `status: failed` with a
//! message the UI can render) or the infrastructure's (`Aws` — let the Lambda
//! invocation fail so it is retried and shows up in CloudWatch metrics).

/// Anything that can go wrong in either handler.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A required environment variable is missing or unparseable. Only ever
    /// raised at cold start — a misconfigured function should fail loudly and
    /// immediately rather than serve half its routes.
    #[error("configuration: {0}")]
    Config(String),

    /// The caller asked for something that does not exist.
    #[error("not found")]
    NotFound,

    /// The caller (or the model) sent something we refuse. Carries a message
    /// that is safe to return to the browser and safe to store in `error` on a
    /// failed document.
    #[error("{0}")]
    Invalid(String),

    /// Authentication failed. Deliberately opaque: the message is not
    /// propagated to the client, because distinguishing "no such credential"
    /// from "bad signature" from "expired challenge" is free reconnaissance.
    #[error("unauthorized")]
    Unauthorized,

    /// A cap was hit. Separate from `Invalid` because it is not the document's
    /// fault and the UI should say so differently.
    #[error("{0}")]
    QuotaExceeded(String),

    /// An AWS call failed. The string is the flattened source chain — see
    /// [`describe`] for why that matters.
    #[error("aws: {0}")]
    Aws(String),

    /// JSON we produced or consumed did not round-trip.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Flatten an error and its `source()` chain into one line.
///
/// `SdkError`'s own `Display` is almost always the useless string
/// "service error" — the actual cause (`AccessDenied`, `ValidationException`,
/// a TLS failure) lives one to three links down the `source()` chain. Logging
/// `{e}` on an AWS error is the single most common way to end up with a
/// CloudWatch log group full of messages that say nothing.
///
/// Truncated per link because Bedrock validation errors can echo a large chunk
/// of the request back at you, and this string is written to DynamoDB as the
/// user-visible `error` field.
pub fn describe<E: std::error::Error + 'static>(err: &E) -> String {
    let mut out = String::new();
    let mut cause: Option<&(dyn std::error::Error + 'static)> = Some(err);
    let mut depth = 0;

    while let Some(e) = cause {
        if depth > 0 {
            out.push_str(": ");
        }
        let link = e.to_string();
        if link.len() > 200 {
            // Sliced on a char boundary, not a byte index: AWS error messages
            // routinely contain non-ASCII quotation marks, and `&s[..200]`
            // through one of those is a panic in the error-handling path —
            // the worst possible place for a panic, because it replaces a
            // diagnosable failure with an opaque one.
            out.push_str(&link[..link.floor_char_boundary(200)]);
            out.push('…');
        } else {
            out.push_str(&link);
        }
        depth += 1;
        if depth >= 5 {
            break;
        }
        cause = e.source();
    }

    out
}

/// Convenience for the extremely common `.map_err(...)` on an AWS call.
pub fn aws<E: std::error::Error + 'static>(err: E) -> Error {
    Error::Aws(describe(&err))
}
