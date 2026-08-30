//! DynamoDB access for the single table.
//!
//! Two things worth knowing before reading:
//!
//! **List operations are `Scan`s, deliberately.** There is no GSI on this
//! table and none is wanted. A GSI is a second copy of the data with its own
//! provisioned capacity, and the table runs at 5 RCU / 5 WCU precisely because
//! that is what the perpetual free tier covers. With one reader and a few
//! hundred rows, a projected scan costs a fraction of one RCU-second and is
//! cheaper *and* simpler than maintaining an index. The comment to leave for
//! the future is the trigger condition: if this table ever holds another
//! user's rows, the scans below become both a cost problem and a data-isolation
//! problem, and that is when the GSI earns its keep — not before.
//!
//! **Everything paginates by hand.** `Scan` returns at most 1 MB per call
//! regardless of how few items match a filter, so a filtered scan that stops
//! at the first response silently returns a prefix of the answer. That failure
//! is invisible in testing (small table, one page) and wrong in production.

use aws_sdk_dynamodb::error::SdkError;
use aws_sdk_dynamodb::operation::transact_write_items::TransactWriteItemsError;
use aws_sdk_dynamodb::operation::update_item::UpdateItemError;
use aws_sdk_dynamodb::types::{AttributeValue, Put, ReturnValue, TransactWriteItem};
use aws_sdk_dynamodb::Client;
use std::collections::HashMap;

use crate::error::{aws, Error, Result};
use crate::keys;
use crate::model::{Attempt, ChallengeItem, DocMeta, DocStatus, DocSummary, Question};
use crate::tags::Topic;

type Item = HashMap<String, AttributeValue>;

/// Outcome of an idempotent attempt write.
///
/// `AlreadyRecorded` carries the original attempt rather than just a flag,
/// because the caller must reproduce the *original* response. Returning a
/// freshly graded one would be almost right — same score, different
/// `submittedAt` — and "almost right" is how a duplicate ends up looking like
/// two sittings in the history view.
///
/// The inner `Option` is the pathological case: a marker exists but the attempt
/// it names does not. That should be unreachable, since both are written in one
/// transaction, but it is representable in the data so it is representable in
/// the type.
#[derive(Debug)]
pub enum AttemptWrite {
    Created,
    AlreadyRecorded(Box<Option<Attempt>>),
}

#[derive(Clone)]
pub struct Store {
    client: Client,
    table: String,
}

impl Store {
    pub fn new(client: Client, table: impl Into<String>) -> Self {
        Self {
            client,
            table: table.into(),
        }
    }

    // ---- documents --------------------------------------------------------

    /// Create the `pending` metadata row.
    ///
    /// Conditional on the partition not already existing. Document ids are
    /// UUIDv4 so a collision is not a real concern; the condition is here so
    /// that a retried request cannot reset a document that has already been
    /// processed back to `pending` and re-run generation against it.
    pub async fn create_doc(&self, doc: &DocMeta) -> Result<()> {
        let item: Item = serde_dynamo::to_item(doc).map_err(|e| Error::Aws(e.to_string()))?;

        self.client
            .put_item()
            .table_name(&self.table)
            .set_item(Some(item))
            .condition_expression("attribute_not_exists(pk)")
            .send()
            .await
            .map_err(aws)?;

        Ok(())
    }

    pub async fn get_doc(&self, doc_id: &str) -> Result<Option<DocMeta>> {
        let out = self
            .client
            .get_item()
            .table_name(&self.table)
            .key("pk", AttributeValue::S(keys::doc_pk(doc_id)))
            .key("sk", AttributeValue::S(keys::META_SK.to_string()))
            .send()
            .await
            .map_err(aws)?;

        match out.item {
            None => Ok(None),
            Some(item) => Ok(Some(
                serde_dynamo::from_item(item).map_err(|e| Error::Aws(e.to_string()))?,
            )),
        }
    }

    /// Claim a document for generation.
    ///
    /// Conditional on the current status being `pending`. S3 event delivery is
    /// at-least-once, so the same object notification can arrive twice; without
    /// this condition the second delivery would re-invoke Bedrock and bill for
    /// a document that is already `ready`. The caller treats a failed condition
    /// as "someone else has it" and returns successfully rather than erroring,
    /// so the duplicate delivery is acknowledged instead of retried forever.
    pub async fn claim_doc_for_processing(&self, doc_id: &str, now: &str) -> Result<bool> {
        let res = self
            .client
            .update_item()
            .table_name(&self.table)
            .key("pk", AttributeValue::S(keys::doc_pk(doc_id)))
            .key("sk", AttributeValue::S(keys::META_SK.to_string()))
            .update_expression("SET #status = :processing, processed_at = :now REMOVE #err")
            .condition_expression("attribute_exists(pk) AND #status = :pending")
            .expression_attribute_names("#status", "status")
            .expression_attribute_names("#err", "error")
            .expression_attribute_values(":processing", AttributeValue::S("processing".into()))
            .expression_attribute_values(":pending", AttributeValue::S("pending".into()))
            .expression_attribute_values(":now", AttributeValue::S(now.to_string()))
            .send()
            .await;

        match res {
            Ok(_) => Ok(true),
            Err(e) if is_conditional_check_failure(&e) => Ok(false),
            Err(e) => Err(aws(e)),
        }
    }

    /// Publish the generated questions.
    ///
    /// `page_count` is written here, not at creation, because the value written
    /// at creation came from the browser. This one was derived from the bytes
    /// by the process that also enforced `MAX_PAGES` against it, so the number
    /// the list view shows and the number the guard checked are the same number.
    ///
    /// `topics` is deliberately absent: topics are chosen by the reader at
    /// upload time and generation has no business overwriting them.
    /// Publish a finished generation.
    ///
    /// `title` and `topics` are written here rather than at creation because
    /// the model chooses both, and it cannot do so until it has read the PDF —
    /// which happens after the row already exists. Until this runs the row
    /// carries the provisional title `POST /docs` derived from the filename.
    #[allow(clippy::too_many_arguments)]
    pub async fn set_doc_ready(
        &self,
        doc_id: &str,
        title: &str,
        topics: &[Topic],
        questions: &[Question],
        page_count: usize,
        tag_version: u32,
    ) -> Result<()> {
        let count = questions.len();
        let questions: AttributeValue =
            serde_dynamo::to_attribute_value(questions).map_err(|e| Error::Aws(e.to_string()))?;
        let topics: AttributeValue =
            serde_dynamo::to_attribute_value(topics).map_err(|e| Error::Aws(e.to_string()))?;

        self.client
            .update_item()
            .table_name(&self.table)
            .key("pk", AttributeValue::S(keys::doc_pk(doc_id)))
            .key("sk", AttributeValue::S(keys::META_SK.to_string()))
            // `generation_attempts` is cleared, not left to accumulate: it
            // counts *consecutive* failures, so a document that failed twice
            // and then succeeded must not carry two attempts into whatever
            // happens next.
            .update_expression(
                "SET #status = :ready, title = :t, topics = :topics, questions = :q, \
                 question_count = :n, page_count = :p, tag_version = :v \
                 REMOVE #err, generation_attempts",
            )
            .expression_attribute_names("#status", "status")
            .expression_attribute_names("#err", "error")
            .expression_attribute_values(":ready", AttributeValue::S("ready".into()))
            .expression_attribute_values(":t", AttributeValue::S(title.to_string()))
            .expression_attribute_values(":topics", topics)
            .expression_attribute_values(":q", questions)
            .expression_attribute_values(":n", AttributeValue::N(count.to_string()))
            .expression_attribute_values(":p", AttributeValue::N(page_count.to_string()))
            .expression_attribute_values(":v", AttributeValue::N(tag_version.to_string()))
            .send()
            .await
            .map_err(aws)?;

        Ok(())
    }

    /// Every topic ever used, for offering back to the model.
    ///
    /// Seeded rather than empty on first read: a model given an empty list
    /// coins a vocabulary from scratch on document one, and the second document
    /// then coins synonyms for it. Seeding costs nothing and gives the reuse
    /// instruction something to bite on immediately.
    ///
    /// Missing and malformed both degrade to the seed rather than erroring. A
    /// failure to read the registry must not fail a generation — the tags would
    /// merely be less consistent, which is not worth spending a Bedrock call to
    /// avoid.
    pub async fn known_topics(&self) -> Result<Vec<Topic>> {
        let out = self
            .client
            .get_item()
            .table_name(&self.table)
            .key("pk", AttributeValue::S(keys::TOPICS_PK.to_string()))
            .key("sk", AttributeValue::S(keys::TOPICS_SK.to_string()))
            .send()
            .await
            .map_err(aws)?;

        let stored = out
            .item
            .as_ref()
            .and_then(|item| item.get("topics"))
            .and_then(|v| v.as_ss().ok())
            .cloned()
            .unwrap_or_default();

        let mut topics: Vec<Topic> = crate::tags::SEED_TOPICS
            .iter()
            .filter_map(|w| Topic::parse(w))
            .collect();

        // Stored tags are filtered through `parse` too. The set may contain
        // values written before the one-word rule, and offering those back to
        // the model would teach it to reproduce exactly the shape now refused.
        for word in stored {
            if let Some(topic) = Topic::parse(&word) {
                if !topics.contains(&topic) {
                    topics.push(topic);
                }
            }
        }

        topics.sort();
        Ok(topics)
    }

    /// Record topics as used, so later generations can reuse them.
    ///
    /// `ADD` on a string set is an atomic union: concurrent generations cannot
    /// clobber each other's tags, and re-registering an existing tag is a no-op
    /// rather than a duplicate.
    ///
    /// Best-effort by contract — the caller logs and continues on failure. The
    /// document is already `ready` by this point, and failing it because a
    /// convenience index did not update would throw away a paid-for generation.
    pub async fn register_topics(&self, topics: &[Topic]) -> Result<()> {
        // DynamoDB rejects an empty string set outright, so this is a real
        // guard rather than an optimisation.
        if topics.is_empty() {
            return Ok(());
        }

        let words: Vec<String> = topics.iter().map(|t| t.as_str().to_string()).collect();

        self.client
            .update_item()
            .table_name(&self.table)
            .key("pk", AttributeValue::S(keys::TOPICS_PK.to_string()))
            .key("sk", AttributeValue::S(keys::TOPICS_SK.to_string()))
            .update_expression("ADD topics :t")
            .expression_attribute_values(":t", AttributeValue::Ss(words))
            .send()
            .await
            .map_err(aws)?;

        Ok(())
    }

    /// Record a failure the UI can render next to a retry button.
    ///
    /// `message` is expected to be an `Error::Invalid` / `Error::QuotaExceeded`
    /// string, not a flattened AWS error chain — those can contain request ids
    /// and ARNs, and this field is rendered in a browser.
    pub async fn set_doc_failed(&self, doc_id: &str, message: &str) -> Result<()> {
        // Truncated because DynamoDB items cap at 400 KB and because a
        // multi-kilobyte error is not something anyone reads off a phone.
        let message: String = message.chars().take(500).collect();

        self.client
            .update_item()
            .table_name(&self.table)
            .key("pk", AttributeValue::S(keys::doc_pk(doc_id)))
            .key("sk", AttributeValue::S(keys::META_SK.to_string()))
            .update_expression("SET #status = :failed, #err = :msg")
            .expression_attribute_names("#status", "status")
            .expression_attribute_names("#err", "error")
            .expression_attribute_values(":failed", AttributeValue::S("failed".into()))
            .expression_attribute_values(":msg", AttributeValue::S(message))
            .send()
            .await
            .map_err(aws)?;

        Ok(())
    }

    /// Put a document back to `pending` after an infrastructure failure, so the
    /// invocation's retry can claim it again.
    ///
    /// Unconditional on status by design: this is called from the generate
    /// handler's error path, where the document is known to be `processing`
    /// because this process put it there.
    /// Put a document back to `pending` after a retryable failure, and report
    /// how many consecutive failures it has now had.
    ///
    /// The count is returned rather than read separately because the caller's
    /// decision — retry again, or give up and mark the document `failed` —
    /// depends on it, and a read-then-write would race with a concurrent
    /// delivery of the same event. `ADD` increments atomically and
    /// `UPDATED_NEW` hands back the post-increment value, so every caller sees
    /// a distinct number and exactly one of them sees the last one.
    ///
    /// A missing attribute starts at zero, so documents written before this
    /// field existed need no migration.
    pub async fn record_generation_failure(&self, doc_id: &str) -> Result<u32> {
        let out = self
            .client
            .update_item()
            .table_name(&self.table)
            .key("pk", AttributeValue::S(keys::doc_pk(doc_id)))
            .key("sk", AttributeValue::S(keys::META_SK.to_string()))
            .update_expression("SET #status = :pending REMOVE #err ADD generation_attempts :one")
            .condition_expression("attribute_exists(pk)")
            .expression_attribute_names("#status", "status")
            .expression_attribute_names("#err", "error")
            .expression_attribute_values(":pending", AttributeValue::S("pending".into()))
            .expression_attribute_values(":one", AttributeValue::N("1".into()))
            .return_values(ReturnValue::UpdatedNew)
            .send()
            .await
            .map_err(aws)?;

        // If the attribute somehow comes back unreadable, report 1 rather than
        // 0. Zero would mean "no failures yet" to the caller, which is the one
        // answer that is certainly wrong here — this function only runs because
        // a generation just failed, and reporting zero forever is how a
        // document gets stranded.
        Ok(out
            .attributes()
            .and_then(|a| a.get("generation_attempts"))
            .and_then(|v| v.as_n().ok())
            .and_then(|n| n.parse().ok())
            .unwrap_or(1))
    }

    /// Arm a user-initiated retry: `failed` (or a stuck `processing`) → `pending`.
    ///
    /// Conditional on the status, and that condition is the whole safety of the
    /// retry button. Without it, a retry on a `ready` document would discard a
    /// perfectly good question set and spend another Bedrock call to regenerate
    /// it; a retry on a document already `pending` would arm a second generation
    /// for an object whose first one has not finished.
    ///
    /// `processing` is included because a document can be left there by an
    /// invocation that died mid-flight — a timeout, or the process being killed
    /// after the claim but before any outcome was written. Excluding it would
    /// make exactly the zombie state the retry button exists to clear the one
    /// state it cannot clear.
    ///
    /// `pending` is deliberately *not* included, and that is safe only because
    /// `record_generation_failure` and `MAX_GENERATION_ATTEMPTS` guarantee a
    /// document stops at `failed` rather than resting at `pending`. Before that
    /// existed, an exhausted retry left the document `pending` — a state this
    /// condition rejects and the UI offers no button for — and the document was
    /// unrecoverable. Widening this to `pending` would be the wrong fix: it
    /// would also arm a retry for a document whose first generation is simply
    /// still queued.
    ///
    /// The attempt counter is cleared, because a user-initiated retry is a
    /// fresh start — often *because* the underlying cause has just been fixed,
    /// which is precisely when the document deserves the full budget of
    /// attempts again rather than the zero it has left.
    pub async fn arm_retry(&self, doc_id: &str) -> Result<bool> {
        let res = self
            .client
            .update_item()
            .table_name(&self.table)
            .key("pk", AttributeValue::S(keys::doc_pk(doc_id)))
            .key("sk", AttributeValue::S(keys::META_SK.to_string()))
            .update_expression("SET #status = :pending REMOVE #err, generation_attempts")
            .condition_expression(
                "attribute_exists(pk) AND (#status = :failed OR #status = :processing)",
            )
            .expression_attribute_names("#status", "status")
            .expression_attribute_names("#err", "error")
            .expression_attribute_values(":pending", AttributeValue::S("pending".into()))
            .expression_attribute_values(":failed", AttributeValue::S("failed".into()))
            .expression_attribute_values(":processing", AttributeValue::S("processing".into()))
            .send()
            .await;

        match res {
            Ok(_) => Ok(true),
            Err(e) if is_conditional_check_failure(&e) => Ok(false),
            Err(e) => Err(aws(e)),
        }
    }

    /// Every document, with its attempt count.
    ///
    /// One scan, not one scan plus a query per document. The attempt rows share
    /// a partition with their document's metadata row, so a single pass over
    /// the table can count them while collecting the summaries — an N+1 of
    /// `Query` calls would cost more request units *and* more latency on a
    /// function that shares a 10-execution concurrency pool with the generator.
    ///
    /// Note the projection deliberately omits `questions`; see [`DocSummary`].
    /// The attempt rows are projected down to nothing but their keys.
    pub async fn list_docs(&self) -> Result<Vec<DocSummary>> {
        let mut summaries: HashMap<String, DocSummary> = HashMap::new();
        let mut attempt_counts: HashMap<String, usize> = HashMap::new();

        let mut start_key: Option<Item> = None;
        loop {
            let out = self
                .client
                .scan()
                .table_name(&self.table)
                // `size` is a reserved word; `status` and `error` are too.
                .projection_expression(
                    "pk, sk, doc_id, title, #status, #err, topics, tag_version, created_at, \
                     page_count, question_count",
                )
                .expression_attribute_names("#status", "status")
                .expression_attribute_names("#err", "error")
                .set_exclusive_start_key(start_key.clone())
                .send()
                .await
                .map_err(aws)?;

            for item in out.items() {
                let Some(AttributeValue::S(pk)) = item.get("pk") else {
                    continue;
                };
                let Some(AttributeValue::S(sk)) = item.get("sk") else {
                    continue;
                };
                if !pk.starts_with("DOC#") {
                    continue;
                }

                if sk == keys::META_SK {
                    // A row that fails to deserialize is skipped rather than
                    // failing the whole list. The realistic cause is a row
                    // written before a schema change, and one unreadable
                    // document must not make the app unusable.
                    match summary_from_item(item) {
                        Ok(s) => {
                            summaries.insert(s.doc_id.clone(), s);
                        }
                        Err(e) => {
                            tracing::warn!(pk = %pk, error = %e, "skipping unreadable document row");
                        }
                    }
                } else if sk.starts_with(keys::ATTEMPT_PREFIX) {
                    *attempt_counts
                        .entry(pk.trim_start_matches("DOC#").to_string())
                        .or_default() += 1;
                }
            }

            start_key = out.last_evaluated_key().cloned();
            if start_key.is_none() {
                break;
            }
        }

        let mut docs: Vec<DocSummary> = summaries
            .into_values()
            .map(|mut d| {
                d.attempt_count = attempt_counts.get(&d.doc_id).copied().unwrap_or(0);
                d
            })
            .collect();

        // Newest first. `created_at` is RFC 3339 UTC so string order is time
        // order — the reason the format is pinned rather than left to whatever
        // `Debug` prints.
        docs.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(docs)
    }

    // ---- attempts ---------------------------------------------------------

    /// Write an attempt exactly once per client-supplied `attempt_id`.
    ///
    /// A double-tap on a flaky phone connection is the normal case, not the
    /// exotic one: the request succeeds, the response is lost, the user taps
    /// again. Without deduplication that writes two attempts, and since every
    /// rate on the history screen is computed over attempts, one lost response
    /// permanently skews the numbers it exists to report.
    ///
    /// The attempt row and an `IDEMPOTENCY#<attempt_id>` marker are written in
    /// **one transaction**, with the condition on the marker. A two-step
    /// version has no safe ordering: writing the attempt first can leave a
    /// duplicate if the marker write fails, and writing the marker first can
    /// leave a marker pointing at an attempt that was never written, which
    /// makes every subsequent retry fail permanently.
    ///
    /// Returns `Ok(None)` when this attempt was already recorded, along with
    /// the sort key of the original so the caller can return the original
    /// result rather than a fresh one — a resubmission must produce the same
    /// answer, not merely avoid producing a second row.
    ///
    /// Cost note: transactional writes are billed at twice the WCU of an
    /// ordinary write, so one submission consumes roughly six WCU against a
    /// table provisioned at five. That is a one-second burst, absorbed by
    /// DynamoDB's burst credit and retried by the SDK if not; submissions are
    /// minutes apart by nature.
    pub async fn put_attempt_once(&self, attempt: &Attempt) -> Result<AttemptWrite> {
        let item: Item = serde_dynamo::to_item(attempt).map_err(|e| Error::Aws(e.to_string()))?;

        let marker = Put::builder()
            .table_name(&self.table)
            .item("pk", AttributeValue::S(attempt.pk.clone()))
            .item(
                "sk",
                AttributeValue::S(keys::idempotency_sk(&attempt.attempt_id)),
            )
            .item("attempt_sk", AttributeValue::S(attempt.sk.clone()))
            // Markers are only useful for as long as a client might retry.
            // Thirty days is far longer than that and costs nothing; the TTL
            // exists so they do not accumulate forever in a table that is
            // scanned on every list.
            .item(
                "expires_at",
                AttributeValue::N((crate::clock::unix_now() + 30 * 24 * 60 * 60).to_string()),
            )
            .condition_expression("attribute_not_exists(pk)")
            .build()
            .map_err(|e| Error::Aws(format!("building idempotency marker: {e}")))?;

        let row = Put::builder()
            .table_name(&self.table)
            .set_item(Some(item))
            .build()
            .map_err(|e| Error::Aws(format!("building attempt: {e}")))?;

        let res = self
            .client
            .transact_write_items()
            .transact_items(TransactWriteItem::builder().put(marker).build())
            .transact_items(TransactWriteItem::builder().put(row).build())
            .send()
            .await;

        match res {
            Ok(_) => Ok(AttemptWrite::Created),
            Err(e) if is_transaction_condition_failure(&e) => {
                let existing = self
                    .find_attempt_by_id(&attempt.doc_id, &attempt.attempt_id)
                    .await?;
                Ok(AttemptWrite::AlreadyRecorded(Box::new(existing)))
            }
            Err(e) => Err(aws(e)),
        }
    }

    /// Follow an idempotency marker back to the attempt it recorded.
    async fn find_attempt_by_id(&self, doc_id: &str, attempt_id: &str) -> Result<Option<Attempt>> {
        let marker = self
            .client
            .get_item()
            .table_name(&self.table)
            .key("pk", AttributeValue::S(keys::doc_pk(doc_id)))
            .key("sk", AttributeValue::S(keys::idempotency_sk(attempt_id)))
            // Strongly consistent: the marker was written moments ago by the
            // request this one is a duplicate of, and an eventually-consistent
            // read that missed it would send back "not found" for an attempt
            // that certainly exists.
            .consistent_read(true)
            .send()
            .await
            .map_err(aws)?;

        let Some(AttributeValue::S(attempt_sk)) =
            marker.item.as_ref().and_then(|i| i.get("attempt_sk"))
        else {
            return Ok(None);
        };

        let out = self
            .client
            .get_item()
            .table_name(&self.table)
            .key("pk", AttributeValue::S(keys::doc_pk(doc_id)))
            .key("sk", AttributeValue::S(attempt_sk.clone()))
            .consistent_read(true)
            .send()
            .await
            .map_err(aws)?;

        match out.item {
            None => Ok(None),
            Some(item) => Ok(Some(
                serde_dynamo::from_item(item).map_err(|e| Error::Aws(e.to_string()))?,
            )),
        }
    }

    /// Every attempt across every document, newest first.
    ///
    /// Unfiltered at the store layer on purpose: the format/skill/topic filters
    /// are predicates over *elements of a list attribute*, which DynamoDB's
    /// filter expressions cannot express (`contains` works on a set or a string,
    /// not on a list of maps). Pushing them down would mean either a materialised
    /// filter attribute per combination or a wrong query, so they are applied in
    /// the handler over a result set that is, for one reader, small.
    pub async fn list_attempts(&self) -> Result<Vec<Attempt>> {
        let mut attempts = Vec::new();
        let mut start_key: Option<Item> = None;

        loop {
            let out = self
                .client
                .scan()
                .table_name(&self.table)
                .filter_expression("begins_with(sk, :prefix)")
                .expression_attribute_values(
                    ":prefix",
                    AttributeValue::S(keys::ATTEMPT_PREFIX.to_string()),
                )
                .set_exclusive_start_key(start_key.clone())
                .send()
                .await
                .map_err(aws)?;

            for item in out.items() {
                match serde_dynamo::from_item::<_, Attempt>(item.clone()) {
                    Ok(a) => attempts.push(a),
                    Err(e) => tracing::warn!(error = %e, "skipping unreadable attempt row"),
                }
            }

            start_key = out.last_evaluated_key().cloned();
            if start_key.is_none() {
                break;
            }
        }

        attempts.sort_by(|a, b| b.submitted_at.cmp(&a.submitted_at));
        Ok(attempts)
    }

    // ---- auth challenges --------------------------------------------------

    pub async fn put_challenge(&self, challenge: &ChallengeItem) -> Result<()> {
        let item: Item = serde_dynamo::to_item(challenge).map_err(|e| Error::Aws(e.to_string()))?;

        self.client
            .put_item()
            .table_name(&self.table)
            .set_item(Some(item))
            .send()
            .await
            .map_err(aws)?;

        Ok(())
    }

    /// Consume a challenge, atomically.
    ///
    /// This is a `DeleteItem` with `ReturnValues: ALL_OLD`, not a read followed
    /// by a delete. The difference is the entire single-use guarantee: two
    /// concurrent verifications of the same assertion both succeed under
    /// read-then-delete, and exactly one gets a non-empty `Attributes` here.
    ///
    /// Returns `None` if the challenge never existed or has already been used.
    /// Expiry is checked by the caller against `expires_at` — DynamoDB's TTL is
    /// a background sweep with no latency guarantee (documented as typically
    /// within 48 hours), so relying on it to enforce a 60-second window would
    /// give a 60-second window on paper and a two-day one in practice.
    pub async fn take_challenge(&self, challenge_b64: &str) -> Result<Option<ChallengeItem>> {
        self.take_auth_item(keys::challenge_sk(challenge_b64)).await
    }

    // ---- passkey registration ---------------------------------------------
    //
    // Bootstrap only. Nothing writes these rows once `WEBAUTHN_CREDENTIALS` is
    // non-empty — see the `api` crate's `register` module.

    /// Store the state of a registration ceremony.
    ///
    /// The same write as [`Store::put_challenge`], named for what it stores so
    /// the call site is not misleading; the sort key in `registration` is what
    /// actually distinguishes the two.
    pub async fn put_registration(&self, registration: &ChallengeItem) -> Result<()> {
        self.put_challenge(registration).await
    }

    /// Consume a registration state, atomically.
    ///
    /// Single-use for the same reason a challenge is — see
    /// [`Store::take_challenge`] — with the same expiry caveat: `expires_at` is
    /// checked by the caller, because TTL is a sweep and not a deadline.
    pub async fn take_registration(&self, id: &str) -> Result<Option<ChallengeItem>> {
        self.take_auth_item(keys::registration_sk(id)).await
    }

    /// The delete-and-return shared by both ceremonies. `sk` is taken already
    /// built so there is one code path and no chance of a caller passing a raw
    /// id where a prefixed key belongs.
    async fn take_auth_item(&self, sk: String) -> Result<Option<ChallengeItem>> {
        let out = self
            .client
            .delete_item()
            .table_name(&self.table)
            .key("pk", AttributeValue::S(keys::AUTH_PK.to_string()))
            .key("sk", AttributeValue::S(sk))
            .return_values(ReturnValue::AllOld)
            .send()
            .await
            .map_err(aws)?;

        match out.attributes {
            None => Ok(None),
            Some(item) if item.is_empty() => Ok(None),
            Some(item) => Ok(Some(
                serde_dynamo::from_item(item).map_err(|e| Error::Aws(e.to_string()))?,
            )),
        }
    }

    // ---- daily generation quota -------------------------------------------

    /// Reserve one generation against today's budget.
    ///
    /// **This must be atomic and it is the reason this method exists rather
    /// than a `list_docs().filter(today).count()`.** `generate` is driven by S3
    /// `ObjectCreated`, which has no throttle of its own, and this account's
    /// Lambda concurrency limit is 10 — so ten handlers can be inside this
    /// function simultaneously. A read-then-compare implementation has every
    /// one of them read the same pre-burst count and every one of them proceed,
    /// which defeats the cap under precisely the burst it exists to stop.
    ///
    /// So the increment and the check are one conditional `UpdateItem`.
    /// DynamoDB evaluates the condition against the item as it is at the moment
    /// of the write, serialised per item, and returns
    /// `ConditionalCheckFailedException` to the losers. The rejection path is
    /// that exception — not a comparison in this process.
    ///
    /// The counter is *reserved*, not refunded. A generation that then fails
    /// validation still consumed a Bedrock call and still cost money, which is
    /// what the cap is denominated in.
    ///
    /// `expires_at` uses `if_not_exists` so the TTL is stamped by whichever
    /// call creates the day's row and is not pushed forward by later ones —
    /// otherwise a busy day's counter would keep renewing its own lease.
    pub async fn reserve_daily_quota(&self, date: &str, cap: u32) -> Result<()> {
        // Two days rather than one: TTL deletes are approximate, and a row that
        // outlives its day by a few hours is harmless, whereas one deleted
        // early would silently reset the cap mid-day.
        let expires_at = crate::clock::unix_now() + 2 * 24 * 60 * 60;

        let res = self
            .client
            .update_item()
            .table_name(&self.table)
            .key("pk", AttributeValue::S(keys::QUOTA_PK.to_string()))
            .key("sk", AttributeValue::S(keys::day_sk(date)))
            .update_expression("ADD #count :one SET expires_at = if_not_exists(expires_at, :exp)")
            .condition_expression("attribute_not_exists(#count) OR #count < :cap")
            .expression_attribute_names("#count", "count")
            .expression_attribute_values(":one", AttributeValue::N("1".into()))
            .expression_attribute_values(":cap", AttributeValue::N(cap.to_string()))
            .expression_attribute_values(":exp", AttributeValue::N(expires_at.to_string()))
            .send()
            .await;

        match res {
            Ok(_) => Ok(()),
            Err(e) if is_conditional_check_failure(&e) => Err(Error::QuotaExceeded(format!(
                "daily document limit of {cap} reached for {date}; try again tomorrow"
            ))),
            Err(e) => Err(aws(e)),
        }
    }
}

/// `DocSummary` needs `doc_id` and `question_count`, and the projected item may
/// legitimately be missing the latter (documents written before generation
/// completed have no questions at all).
fn summary_from_item(item: &Item) -> Result<DocSummary> {
    #[derive(serde::Deserialize)]
    struct Projected {
        doc_id: String,
        title: String,
        status: DocStatus,
        #[serde(default)]
        error: Option<String>,
        #[serde(default)]
        topics: Vec<Topic>,
        tag_version: u32,
        created_at: String,
        #[serde(default)]
        page_count: usize,
        #[serde(default)]
        question_count: usize,
    }

    let p: Projected =
        serde_dynamo::from_item(item.clone()).map_err(|e| Error::Aws(e.to_string()))?;

    Ok(DocSummary {
        doc_id: p.doc_id,
        title: p.title,
        status: p.status,
        error: p.error,
        topics: p.topics,
        tag_version: p.tag_version,
        page_count: p.page_count,
        created_at: p.created_at,
        attempt_count: 0,
        question_count: p.question_count,
    })
}

/// Distinguishing "the condition said no" from "DynamoDB is unreachable"
/// matters: the first is a normal control-flow outcome (duplicate S3 delivery,
/// quota reached) and the second must surface as an error so the invocation is
/// retried.
fn is_conditional_check_failure(err: &SdkError<UpdateItemError>) -> bool {
    matches!(
        err.as_service_error(),
        Some(UpdateItemError::ConditionalCheckFailedException(_))
    )
}

/// A transaction reports a failed condition as a *cancelled transaction* whose
/// per-item reasons must be inspected — there is no top-level
/// `ConditionalCheckFailedException` to match on. Treating every cancellation as
/// a duplicate would swallow genuine conflicts and capacity failures, so the
/// reason codes are checked explicitly.
fn is_transaction_condition_failure(err: &SdkError<TransactWriteItemsError>) -> bool {
    let Some(TransactWriteItemsError::TransactionCanceledException(e)) = err.as_service_error()
    else {
        return false;
    };

    e.cancellation_reasons()
        .iter()
        .any(|r| r.code() == Some("ConditionalCheckFailed"))
}
