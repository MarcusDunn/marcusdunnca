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

/// Claude Sonnet 4.6 on the US cross-region inference profile.
///
/// This replaced Nova Lite, and the tradeoff was made knowingly: Nova Lite was
/// the only model with a genuine in-region (`ca.`) profile, so document text
/// stayed in ca-central-1. A `us.` profile routes outside Canada. The reason it
/// is worth it is question quality — measured on the same document, Nova
/// produced questions answerable from general knowledge, no reasoning mode is
/// available on the Nova family at any price, and structural failures persisted
/// even under a JSON Schema. Sonnet with a thinking budget produced ten
/// schema-clean questions anchored to the document's own tables.
///
/// Any cross-region profile — `us.` or `global.` — depends on the Bedrock
/// exemption in the permissions boundary's region lock, because each routing
/// target is authorized with `aws:RequestedRegion` set to that target's region
/// rather than the caller's. See `global_service_actions` in bootstrap/iam.tf.
///
/// Roughly 7c per document against Nova's 0.1c. At a handful of documents a
/// month that is inside the noise of the budget.
const DEFAULT_MODEL_ID: &str = "us.anthropic.claude-sonnet-4-6";

/// Thinking budget, in tokens. Zero disables it.
///
/// Reasoning is what moves questions from "a well-read person could answer
/// this" to "you had to have opened the document" — on the reference document
/// it was the difference between defining `bond yields` in the abstract and
/// asking what this report says pent-up demand did in Ontario. It costs output
/// tokens, hence configurable rather than hardcoded.
const DEFAULT_THINKING_BUDGET_TOKENS: u32 = 3000;

/// Page ceiling. A hundred pages is already a long read; beyond that the
/// generation is expensive and the resulting ten questions cover so little of
/// the document that they are not a useful test of having read it.
const DEFAULT_MAX_PAGES: usize = 100;

/// Documents per UTC day.
const DEFAULT_DAILY_CAP: u32 = 20;

/// Consecutive infrastructure failures before a document is marked `failed`.
///
/// **This must be at least Lambda's total attempt count for the function**
/// (`maximum_retry_attempts + 1`), or documents strand again: the handler would
/// still be returning "put it back to pending, something will retry" on the
/// very invocation that is the last one. Terraform derives the environment
/// variable from the same input it configures Lambda with, precisely so the two
/// cannot drift — see `generate_retry_attempts` in infra/variables.tf.
///
/// The default matches the default there (one retry, so two attempts).
const DEFAULT_MAX_GENERATION_ATTEMPTS: u32 = 2;

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
    thinking_budget_tokens: u32,
    max_pages: usize,
    daily_cap: u32,
    max_document_bytes: i64,
    max_generation_attempts: u32,
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
            thinking_budget_tokens: config::parse_or(
                "THINKING_BUDGET_TOKENS",
                DEFAULT_THINKING_BUDGET_TOKENS,
            )?,
            max_pages: config::parse_or("MAX_PAGES", DEFAULT_MAX_PAGES)?,
            daily_cap: config::parse_or("DAILY_DOCUMENT_CAP", DEFAULT_DAILY_CAP)?,
            max_document_bytes: config::parse_or("MAX_DOCUMENT_BYTES", DEFAULT_MAX_DOCUMENT_BYTES)?,
            max_generation_attempts: config::parse_or(
                "MAX_GENERATION_ATTEMPTS",
                DEFAULT_MAX_GENERATION_ATTEMPTS,
            )?
            // Zero or one would mean "give up before Lambda stops retrying",
            // which reintroduces the stranding this exists to prevent.
            .max(1),
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
///   error so the invocation fails — *until the attempts run out*, at which
///   point it becomes terminal too. See below.
///
/// Getting this backwards is expensive in one direction and invisible in the
/// other: retrying an unparseable PDF burns nothing but noise, while *not*
/// retrying a transient DynamoDB throttle strands a document forever.
///
/// # Why retryable failures must eventually become terminal
///
/// "Retryable" was originally implemented as "always put it back to `pending`
/// and fail the invocation", which is right exactly as long as something is
/// still going to retry. Lambda's retries are finite, and when they are gone
/// the event goes to the dead-letter queue — and there is no mechanism that
/// delivers another `ObjectCreated` for an object that already exists. The
/// document rests at `pending`, with no error text and no Retry button, and is
/// unrecoverable without touching S3 by hand.
///
/// This is not a theoretical edge: an IAM misconfiguration produced it, and the
/// document showed "Queued" indefinitely while nothing was queued.
///
/// So the last attempt writes `failed` instead of `pending`. The document then
/// carries a message and a Retry button, which is the whole difference between
/// a bad afternoon and a lost document.
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

            let attempts = match config.store.record_generation_failure(doc_id).await {
                Ok(n) => n,
                Err(e) => {
                    // The counter could not be advanced, so this attempt cannot
                    // be told apart from the last one. Fail the invocation and
                    // let Lambda retry: over-retrying is recoverable, and
                    // treating an unknown count as "give up" would fail
                    // documents over a transient DynamoDB error.
                    tracing::error!(doc_id, error = %e, "could not record the failed attempt");
                    return Some(err);
                }
            };

            if another_attempt_follows(attempts, config.max_generation_attempts) {
                tracing::warn!(
                    doc_id,
                    attempts,
                    max = config.max_generation_attempts,
                    "returned to pending for another attempt"
                );
                return Some(err);
            }

            // Out of attempts. The message carries the underlying error rather
            // than a generic apology, which deviates from the usual rule that
            // `error` holds only `Invalid`-grade text: this field is the only
            // place a reader sees *why*, the app has exactly one user, and the
            // identifiers in an AWS error chain are their own account's. It is
            // bounded — `describe` truncates each link and `set_doc_failed`
            // truncates the whole string.
            let message = format!(
                "Generation failed {attempts} times and was given up on. Last error — {err}"
            );
            tracing::error!(doc_id, attempts, "giving up on document");

            if let Err(e) = config.store.set_doc_failed(doc_id, &message).await {
                tracing::error!(doc_id, error = %e, "could not record final failure");
                return Some(e);
            }

            // Deliberately `None`. The document is now visibly failed with a
            // Retry button, so failing the invocation as well would buy a
            // dead-letter message and a red metric for a situation already
            // recorded where the reader can act on it.
            None
        }
    }
}

/// Will anything invoke this function again for this document?
///
/// Extracted from [`record_outcome`] because it is a one-line comparison whose
/// off-by-one has no safe direction, and because the surrounding function needs
/// DynamoDB to run. Too eager and the document is failed while Lambda is still
/// willing to retry; too lax and the last attempt says "put it back to pending,
/// something will retry" when nothing will — which strands it exactly as
/// before.
///
/// `attempts` is the post-increment count, so the first failure arrives as 1.
fn another_attempt_follows(attempts: u32, max_attempts: u32) -> bool {
    attempts < max_attempts
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

    // Offered to the model so it reuses an existing tag rather than coining a
    // synonym for one. A failure to read the registry degrades the tags but
    // must not cost a generation, so it falls back to the seed set.
    let known_topics = config.store.known_topics().await.unwrap_or_else(|e| {
        tracing::warn!(doc_id, error = %e, "could not read the topic registry; using seeds");
        trainer_core::tags::SEED_TOPICS
            .iter()
            .filter_map(|w| trainer_core::tags::Topic::parse(w))
            .collect()
    });

    tracing::info!(doc_id, pages, size_bytes = size, "invoking model");

    let generated = bedrock::generate(
        &config.bedrock,
        bedrock::Request {
            model_id: &config.model_id,
            thinking_budget_tokens: config.thinking_budget_tokens,
            known_topics: &known_topics,
            // The document id, so the option shuffle is reproducible per
            // document rather than per invocation.
            seed: doc_id,
            pdf: bytes,
        },
    )
    .await?;

    // `pages` rather than the client-reported count the row was created with.
    // The browser's number is a UX optimisation; this one was derived from the
    // bytes by the same process that enforced `MAX_PAGES` against it, so the
    // number the list shows is the number the guard checked.
    //
    // The title and topics land here too: both are chosen by the model, and
    // until this write the row carries the provisional title `POST /docs`
    // derived from the filename.
    config
        .store
        .set_doc_ready(
            doc_id,
            &generated.title,
            &generated.topics,
            &generated.questions,
            pages,
            TAG_VERSION,
        )
        .await?;

    // After the document is ready, and deliberately not fatal. The generation
    // is already paid for and stored; failing it because a convenience index
    // did not update would throw that away to keep a cache tidy.
    if let Err(e) = config.store.register_topics(&generated.topics).await {
        tracing::warn!(doc_id, error = %e, "could not register topics");
    }

    // Counts and tags only. The questions contain the answer key and never
    // reach a log line, here or anywhere else.
    tracing::info!(
        doc_id,
        questions = generated.questions.len(),
        topics = %generated.topics.iter().map(|t| t.as_str()).collect::<Vec<_>>().join(","),
        "document ready"
    );

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

    /// The boundary that decides whether a document is recoverable.
    ///
    /// With Lambda's default of one retry there are two invocations. The first
    /// failure must leave the document `pending` so the second can claim it;
    /// the second must mark it `failed`, because nothing follows and a
    /// `pending` document has no Retry button.
    #[test]
    fn the_last_attempt_knows_it_is_the_last() {
        let max = super::DEFAULT_MAX_GENERATION_ATTEMPTS;
        assert!(
            super::another_attempt_follows(1, max),
            "first failure: Lambda will retry, so keep the document retryable"
        );
        assert!(
            !super::another_attempt_follows(2, max),
            "second failure: nothing follows, so the document must be failed"
        );
    }

    /// Over-counting must fail closed. If a duplicate delivery pushes the count
    /// past the maximum, the document still ends up `failed` rather than
    /// wrapping back into "retry forever".
    #[test]
    fn exceeding_the_maximum_still_gives_up() {
        assert!(!super::another_attempt_follows(
            99,
            super::DEFAULT_MAX_GENERATION_ATTEMPTS
        ));
    }

    /// The handler's default and Terraform's default describe the same
    /// arrangement: `maximum_retry_attempts = 1`, so two attempts in total.
    /// Terraform derives the environment variable from that same variable, but
    /// the compiled-in fallback is what runs if the variable is ever missing —
    /// and a fallback that disagrees would strand documents silently.
    #[test]
    fn the_compiled_default_matches_one_lambda_retry() {
        assert_eq!(
            super::DEFAULT_MAX_GENERATION_ATTEMPTS,
            1 + 1,
            "infra/variables.tf defaults generate_retry_attempts to 1"
        );
    }
}
