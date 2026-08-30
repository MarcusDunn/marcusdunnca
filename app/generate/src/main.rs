//! S3 `ObjectCreated` → questions.
//!
//! Triggered by the upload landing in the documents bucket, not by the API, so
//! that a browser that uploads and then closes the tab still gets its questions
//! and so the API never has to hold a request open across a model call.
//!
//! **This function is the only thing in the system that spends money per
//! invocation, and nothing upstream of it is throttled.** S3 event notification
//! has no rate limit of its own, this account's Lambda concurrency limit is 10
//! with no reservation possible, and an upload is one tap. Every guard below
//! exists because the default behaviour — process whatever arrives — turns an
//! idle evening of tapping into forty Bedrock invocations.

mod bedrock;
mod pdf;

use aws_lambda_events::event::s3::S3Event;
use lambda_runtime::{service_fn, LambdaEvent};
use trainer_core::clock;
use trainer_core::config;
use trainer_core::error::{aws, Error, Result};
use trainer_core::keys;
use trainer_core::model::DocStatus;
use trainer_core::store::Store;
use trainer_core::tags::TAG_VERSION;

/// Nova Lite in the Canadian inference profile. The `ca.` prefix is a genuine
/// in-region profile rather than a globally-routed one, which is why this model
/// was chosen over the alternatives at similar cost.
const DEFAULT_MODEL_ID: &str = "ca.amazon.nova-lite-v1:0";

/// Page ceiling. A hundred pages is already a long read; beyond that the
/// generation is expensive and the resulting ten questions cover so little of
/// the document that they are not a useful test of having read it.
const DEFAULT_MAX_PAGES: usize = 100;

/// Documents per UTC day.
const DEFAULT_DAILY_CAP: u32 = 20;

/// Ceiling on what is handed to Bedrock.
///
/// The Converse API caps a single document block at roughly 4.5 MB, and exceeds
/// it with a `ValidationException` that costs a round trip and reads like an
/// internal error. Checking locally turns that into a message naming the actual
/// problem. Deliberately below the 50 MB the presigned upload permits — the
/// upload limit bounds storage, this bounds what can be processed.
const DEFAULT_MAX_DOCUMENT_BYTES: i64 = 4_500_000;

struct Config {
    store: Store,
    s3: aws_sdk_s3::Client,
    bedrock: aws_sdk_bedrockruntime::Client,
    /// Read from configuration, never from the event. The event's bucket name
    /// is attacker-influenced in the general case — any bucket can be
    /// configured to notify any function it has permission to — and trusting it
    /// would let a foreign bucket direct this function's reads. Pinning it here
    /// means the function reads from exactly one place.
    docs_bucket: String,
    model_id: String,
    max_pages: usize,
    daily_cap: u32,
    max_document_bytes: i64,
}

impl Config {
    async fn load() -> Result<Self> {
        let sdk = aws_config::load_from_env().await;

        Ok(Self {
            store: Store::new(
                aws_sdk_dynamodb::Client::new(&sdk),
                config::require("TABLE_NAME")?,
            ),
            s3: aws_sdk_s3::Client::new(&sdk),
            bedrock: aws_sdk_bedrockruntime::Client::new(&sdk),
            docs_bucket: config::require("DOCS_BUCKET")?,
            model_id: config::parse_or("MODEL_ID", DEFAULT_MODEL_ID.to_string())?,
            max_pages: config::parse_or("MAX_PAGES", DEFAULT_MAX_PAGES)?,
            daily_cap: config::parse_or("DAILY_DOCUMENT_CAP", DEFAULT_DAILY_CAP)?,
            max_document_bytes: config::parse_or("MAX_DOCUMENT_BYTES", DEFAULT_MAX_DOCUMENT_BYTES)?,
        })
    }
}

#[tokio::main]
async fn main() -> std::result::Result<(), lambda_runtime::Error> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_ansi(false)
        .without_time()
        .with_target(false)
        .init();

    let config = Config::load().await.map_err(|e| {
        tracing::error!(error = %e, "failed to initialise");
        lambda_runtime::Error::from(e.to_string())
    })?;
    let config: &'static Config = Box::leak(Box::new(config));

    lambda_runtime::run(service_fn(move |event: LambdaEvent<S3Event>| async move {
        handle(config, event.payload).await
    }))
    .await
}

/// One S3 event can carry several records. Each is processed independently so
/// that one bad document does not strand the others.
///
/// Returning `Err` from here makes Lambda retry the *whole* event, including
/// records that already succeeded. Since a succeeded record's document is no
/// longer `pending`, the retry's `claim_doc_for_processing` returns false and
/// it is skipped — which is why that claim is conditional rather than a plain
/// status write. Without it, a retry caused by record three would re-invoke
/// Bedrock for records one and two.
async fn handle(config: &Config, event: S3Event) -> std::result::Result<(), lambda_runtime::Error> {
    let mut infrastructure_failure = None;

    for record in event.records {
        let Some(key) = record.s3.object.key.as_deref() else {
            tracing::warn!("s3 record with no object key");
            continue;
        };

        // S3 event keys are URL-encoded — a space arrives as `+` and a `#` as
        // `%23`. Document ids are UUIDs so this never matters in practice, but
        // decoding is one line and the alternative is a bug that only appears
        // once someone changes how ids are generated.
        let decoded = urlencoding::decode(key)
            .map(|c| c.into_owned())
            .unwrap_or_else(|_| key.to_string());

        let Some(doc_id) = keys::doc_id_from_s3_key(&decoded) else {
            // Not an error: the bucket may hold objects this function has no
            // business with, now or later. Silently ignoring them is correct;
            // failing would put an unrelated object into a retry loop.
            tracing::info!(key = %decoded, "ignoring object outside docs/<id>.pdf");
            continue;
        };

        match process(config, doc_id).await {
            Ok(()) => {}
            Err(e) => {
                if let Some(err) = record_outcome(config, doc_id, e).await {
                    infrastructure_failure = Some(err);
                }
            }
        }
    }

    match infrastructure_failure {
        None => Ok(()),
        // Surfaced as an invocation error so it lands in Lambda's error metric
        // and gets Lambda's own retries. A document that failed this way was
        // reset to `pending` by `record_outcome`, so the retry can re-claim it.
        Some(e) => Err(lambda_runtime::Error::from(e.to_string())),
    }
}

/// Decide what a failure means for the document, and whether the invocation
/// should be reported as failed.
///
/// The distinction is the whole of this function:
///
/// - **The document's fault** (`Invalid`) or **a deliberate refusal**
///   (`QuotaExceeded`) is terminal. Write `status: failed` with the message and
///   return `None` — retrying will produce the identical outcome, and three
///   more Bedrock calls if it got that far.
/// - **Infrastructure** (`Aws`, `Json`, `Config`) may succeed on a retry. Put
///   the document back to `pending` so a retry can claim it, and return the
///   error so the invocation fails.
///
/// Getting this backwards is expensive in one direction and invisible in the
/// other: retrying an unparseable PDF burns nothing but noise, while *not*
/// retrying a transient DynamoDB throttle strands a document forever.
async fn record_outcome(config: &Config, doc_id: &str, err: Error) -> Option<Error> {
    match &err {
        Error::Invalid(msg) | Error::QuotaExceeded(msg) => {
            tracing::warn!(doc_id, reason = %msg, "document rejected");
            if let Err(e) = config.store.set_doc_failed(doc_id, msg).await {
                tracing::error!(doc_id, error = %e, "could not record failure");
                return Some(e);
            }
            None
        }
        _ => {
            tracing::error!(doc_id, error = %err, "processing failed");
            if let Err(e) = config.store.reset_doc_to_pending(doc_id).await {
                tracing::error!(doc_id, error = %e, "could not reset document for retry");
            }
            Some(err)
        }
    }
}

/// The pipeline for one document.
///
/// Guard ordering is deliberate and runs cheapest-first, with the two that cost
/// money last:
///
/// 1. **Claim.** A conditional status write. Costs one WCU and stops duplicate
///    S3 deliveries from paying for the same document twice.
/// 2. **Size.** A `HeadObject`. Rejects before transferring the bytes.
/// 3. **Pages.** Local parse. Rejects before the model sees anything.
/// 4. **Quota.** An atomic reservation. Placed *after* the local guards so a
///    day of malformed uploads does not consume a budget denominated in model
///    spend — but before the invocation, which is the thing being budgeted.
/// 5. **Generate.**
async fn process(config: &Config, doc_id: &str) -> Result<()> {
    let now = clock::now_iso8601();

    if !config.store.claim_doc_for_processing(doc_id, &now).await? {
        // Either the document does not exist (an object written outside the
        // API), or it is not `pending` — a duplicate delivery, or a retry of an
        // event whose earlier records already succeeded.
        tracing::info!(doc_id, "document is not pending; skipping");
        return Ok(());
    }

    let doc = config
        .store
        .get_doc(doc_id)
        .await?
        // Impossible: the claim above succeeded, which required the row to
        // exist. Handled rather than unwrapped because "impossible" and
        // "cannot happen at runtime" are different claims.
        .ok_or_else(|| Error::Aws("document vanished between claim and read".into()))?;

    debug_assert_eq!(doc.status, DocStatus::Processing);

    let head = config
        .s3
        .head_object()
        .bucket(&config.docs_bucket)
        .key(&doc.s3_key)
        .send()
        .await
        .map_err(aws)?;

    let size = head.content_length().unwrap_or_default();
    if size > config.max_document_bytes {
        return Err(Error::Invalid(format!(
            "this PDF is {:.1} MB; the model accepts up to {:.1} MB",
            size as f64 / 1_000_000.0,
            config.max_document_bytes as f64 / 1_000_000.0,
        )));
    }

    let body = config
        .s3
        .get_object()
        .bucket(&config.docs_bucket)
        .key(&doc.s3_key)
        .send()
        .await
        .map_err(aws)?;

    let bytes = body
        .body
        .collect()
        .await
        .map_err(|e| Error::Aws(format!("reading object body: {e}")))?
        .into_bytes()
        .to_vec();

    let pages = pdf::page_count(&bytes)?;
    if pages > config.max_pages {
        return Err(Error::Invalid(format!(
            "this PDF has {pages} pages; the limit is {}",
            config.max_pages
        )));
    }

    // Reserved, not counted-then-compared. See `Store::reserve_daily_quota` —
    // with no throttle upstream and ten concurrent executions available, a
    // read-then-check would let a burst straight through.
    config
        .store
        .reserve_daily_quota(&clock::today_utc(), config.daily_cap)
        .await?;

    tracing::info!(doc_id, pages, size_bytes = size, "invoking model");

    let questions = bedrock::generate(&config.bedrock, &config.model_id, &doc.title, bytes).await?;

    // `pages` rather than the client-reported count the row was created with.
    // The browser's number is a UX optimisation; this one was derived from the
    // bytes by the same process that enforced `MAX_PAGES` against it, so the
    // number the list shows is the number the guard checked.
    config
        .store
        .set_doc_ready(doc_id, &questions, pages, TAG_VERSION)
        .await?;

    // Counts only. The questions contain the answer key and never reach a log
    // line, here or anywhere else.
    tracing::info!(doc_id, questions = questions.len(), "document ready");

    Ok(())
}

#[cfg(test)]
mod tests {
    use trainer_core::keys;

    #[test]
    fn url_encoded_keys_still_resolve() {
        let encoded = "docs/0191f0c8-2a1e-7c3b-9d44-6f2b1c4a5e77.pdf";
        let decoded = urlencoding::decode(encoded).expect("decodes");
        assert_eq!(
            keys::doc_id_from_s3_key(&decoded),
            Some("0191f0c8-2a1e-7c3b-9d44-6f2b1c4a5e77")
        );
    }

    #[test]
    fn objects_outside_the_docs_prefix_are_ignored() {
        assert!(keys::doc_id_from_s3_key("uploads/other.pdf").is_none());
    }
}
