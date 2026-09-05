//! Key construction for the single table.
//!
//! Centralised because key strings are the one thing in this system that cannot
//! be refactored after the fact — a typo'd prefix does not fail, it writes a
//! row nobody will ever read again. Every `format!("DOC#...")` in a handler is
//! a chance to write `DOC:` instead.

/// Partition key for a document and everything hanging off it.
pub fn doc_pk(doc_id: &str) -> String {
    format!("DOC#{doc_id}")
}

/// Sort key of the document's metadata row.
pub const META_SK: &str = "META";

/// Sort key of one attempt. RFC 3339 in UTC, so lexicographic order is
/// chronological order and a `begins_with`/range query over attempts works
/// without a secondary index.
pub fn attempt_sk(submitted_at: &str) -> String {
    format!("{ATTEMPT_PREFIX}{submitted_at}")
}

pub const ATTEMPT_PREFIX: &str = "ATTEMPT#";

/// Sort key of the idempotency marker for one submission.
///
/// Shares the document's partition so it can be written in the same
/// transaction as the attempt — DynamoDB transactions are cross-partition
/// capable, but keeping them together also means a document's rows can be
/// deleted as one range.
pub fn idempotency_sk(attempt_id: &str) -> String {
    format!("IDEMPOTENCY#{attempt_id}")
}

/// Partition holding the spaced-review schedule, one row per question.
///
/// **One partition for every document's questions, deliberately.** The review
/// queue's only question is "what is due now", which spans documents — putting
/// each document's schedule in its own partition would turn that into a scan.
/// One `Query` here returns the whole schedule and the handler picks the due
/// ones, which is the same trade the document list already makes and for the
/// same reason: one reader, a few hundred rows, 5 RCU.
///
/// The trigger for changing it: if this grows past a few thousand rows the
/// Query starts paginating for no benefit, and the fix is to move the due date
/// into the sort key so the range itself selects. That is a migration, not a
/// rewrite, and it is not worth doing early — a due date in the sort key means
/// every review is a delete plus a put rather than an update.
pub const REVIEW_PK: &str = "REVIEW";

/// Sort key of one question's schedule.
///
/// Document first so `begins_with(doc_pk_fragment)` selects a single document's
/// questions — which is what the submit path needs when it advances every
/// question in an attempt at once.
pub fn review_sk(doc_id: &str, qid: &str) -> String {
    format!("{doc_id}#{qid}")
}

/// The `begins_with` prefix matching every review row for one document.
pub fn review_sk_prefix(doc_id: &str) -> String {
    format!("{doc_id}#")
}

/// All auth challenges share one partition. That is fine — they live 60
/// seconds and there is one user, so this is not a hot partition, and putting
/// them together means TTL sweeps touch one place.
///
/// The partition lives in the ceremony table, not the application table, so
/// that anonymous callers cannot write where the authenticated routes scan.
/// See `Store::with_auth_table`.
pub const AUTH_PK: &str = "AUTH";

/// The challenge itself is the sort key. Making the challenge the key is what
/// makes single-use enforcement a `DeleteItem` rather than a read-modify-write.
pub fn challenge_sk(challenge_b64: &str) -> String {
    format!("CHALLENGE#{challenge_b64}")
}

/// Sort key of an in-flight passkey registration, in the same partition and
/// with the same TTL treatment as the challenges above.
///
/// A separate prefix rather than reusing `CHALLENGE#`, because the two hold
/// different serialized types — a `PasskeyRegistration` and a
/// `PasskeyAuthentication` — and a row read as the wrong one is a
/// deserialization failure at the least helpful moment. Rows under this prefix
/// can only be written by a deployment that has no credentials at all; see the
/// `api` crate's `register` module.
pub fn registration_sk(id: &str) -> String {
    format!("REGISTRATION#{id}")
}

/// Partition holding the daily generation counter.
pub const QUOTA_PK: &str = "QUOTA";

/// The single row listing every topic ever used.
///
/// One row, not one row per topic, because the only question ever asked of it
/// is "what is the whole set" — that is what gets handed to the model so it can
/// reuse an existing tag instead of coining a synonym. A `GetItem` answers it;
/// a row-per-topic layout would need a query and would buy nothing.
///
/// Stored as a DynamoDB string set so registration is `ADD`, which is an atomic
/// union. The read-modify-write alternative loses tags when two documents
/// finish generating at once, and with ten concurrent executions available that
/// is not hypothetical.
pub const TOPICS_PK: &str = "TOPICS";
pub const TOPICS_SK: &str = "REGISTRY";

/// One row per UTC day.
pub fn day_sk(date: &str) -> String {
    format!("DAY#{date}")
}

/// Recover the document id from an S3 object key.
///
/// Accepts exactly `docs/<id>.pdf` and nothing else. Anything laxer — a
/// `strip_prefix` plus `strip_suffix` with no check on what is between them —
/// would happily derive an id containing `/`, which produces a partition key
/// that no `POST /docs` could ever have created and a row that is invisible to
/// the rest of the app.
pub fn doc_id_from_s3_key(key: &str) -> Option<&str> {
    let id = key.strip_prefix("docs/")?.strip_suffix(".pdf")?;

    let ok = !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');

    ok.then_some(id)
}

/// The only key a document is ever stored at.
pub fn s3_key(doc_id: &str) -> String {
    format!("docs/{doc_id}.pdf")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn s3_key_and_doc_id_round_trip() {
        let id = "0191f0c8-2a1e-7c3b-9d44-6f2b1c4a5e77";
        assert_eq!(doc_id_from_s3_key(&s3_key(id)), Some(id));
    }

    #[test]
    fn traversal_and_nesting_are_refused() {
        // A presigned PUT pins the key, but the S3 trigger fires for every
        // object in the bucket including any written by a future feature or by
        // hand. Deriving a partition key from an attacker-influenced string is
        // the thing to be careful about here.
        assert_eq!(doc_id_from_s3_key("docs/a/b.pdf"), None);
        assert_eq!(doc_id_from_s3_key("docs/../secret.pdf"), None);
        assert_eq!(doc_id_from_s3_key("docs/.pdf"), None);
        assert_eq!(doc_id_from_s3_key("other/x.pdf"), None);
        assert_eq!(doc_id_from_s3_key("docs/x.txt"), None);
        assert_eq!(
            doc_id_from_s3_key(&format!("docs/{}.pdf", "a".repeat(65))),
            None
        );
    }
}
