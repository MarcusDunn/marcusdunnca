//! The stored shapes, and the wire shapes derived from them.
//!
//! **Two vocabularies live in this file and they are deliberately different.**
//! Everything persisted to DynamoDB is `snake_case`; everything serialized to
//! the browser is `camelCase`, because `web/src/lib/schemas.ts` Zod-parses every
//! response and a mismatched key does not degrade — it throws and the screen is
//! blank. Rather than renaming the stored attributes (which would orphan every
//! existing row) the wire types carry `#[serde(rename_all = "camelCase")]` and
//! are built from the stored ones.
//!
//! The important thing in this file is [`PublicQuestion`]. Everything else is
//! bookkeeping.

use serde::{Deserialize, Serialize};

use crate::tags::{Choice, QuestionFormat, Skill, Topic};

/// One selectable answer.
///
/// The id is a [`Choice`], so it is `"a"` through `"d"` and nothing else. The
/// frontend keys its radio inputs and its `selectedOptionId` on this string, and
/// making it a closed enum rather than a free string means a submitted option id
/// that does not exist is a deserialization failure rather than a silently
/// ungraded question.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionOption {
    pub id: Choice,
    pub text: String,
}

/// A question as stored — including the answer key.
///
/// This type must never be serialized onto a response body. See
/// [`PublicQuestion`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Question {
    pub id: String,
    /// Defaulted, because the model is not asked for it — there is exactly one
    /// legal value, so the field can only ever be filled in wrongly. See
    /// [`QuestionFormat`]'s `Default` impl for the generation that was thrown
    /// away proving that.
    ///
    /// The field itself stays: grading branches on it, and a second format will
    /// not be graded by equality.
    #[serde(default)]
    pub format: QuestionFormat,
    /// Singular. One question tests one skill: the history view builds a
    /// skill × topic matrix, and a question belonging to three skills would
    /// either be counted three times or need a weighting rule nobody has
    /// specified.
    pub skill: Skill,
    pub prompt: String,
    /// Exactly four, validated at generation time so [`Choice::index`] is total.
    pub options: Vec<String>,
    pub answer: Choice,
    pub explanation: String,
}

impl Question {
    /// Pair each option with its letter.
    pub fn options(&self) -> Vec<QuestionOption> {
        self.options
            .iter()
            .enumerate()
            .filter_map(|(i, text)| {
                // `Choice::ALL` has four entries and generation validates that
                // `options` does too, so this never drops anything. `get`
                // rather than an index because "never" is a claim about
                // validated data, and a panic here would be an invocation
                // error with no failed-status row to explain it.
                Choice::ALL.get(i).map(|id| QuestionOption {
                    id: *id,
                    text: text.clone(),
                })
            })
            .collect()
    }
}

/// A question as sent to the browser for answering.
///
/// **This is the one bug that quietly makes the whole app pointless.** A quiz
/// whose payload contains the answer key is not a quiz; and because the app
/// still *works* — questions render, submissions grade, history accumulates —
/// nothing ever surfaces the mistake except opening devtools.
///
/// So the key is not stripped, it is *unrepresentable*. This struct has no
/// `answer` field and no `explanation` field, which means:
///
///   - `GET /docs/:id/quiz` cannot leak them by forgetting a `remove()`;
///   - adding a field to `Question` later does not silently widen the public
///     shape, because the `From` impl below names every field it copies;
///   - the only way to leak the key is to change *this* type, which is a
///     visible, reviewable diff rather than a forgotten line.
///
/// The alternative — `#[serde(skip_serializing)]` on the fields of `Question` —
/// was rejected because those same fields must serialize when the document is
/// written to DynamoDB. One type cannot be both the storage shape and the
/// public shape without a flag, and a flag is exactly the thing that gets
/// forgotten.
///
/// The frontend agrees, independently: `QuizQuestion` in `schemas.ts` has no
/// optional `answer` to fall back on, so a leak here would also be a parse
/// error there. Two locks on the same door, which is the right number for this
/// particular door.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicQuestion {
    pub id: String,
    pub format: QuestionFormat,
    pub skill: Skill,
    /// The document's topics, copied onto every question. The frontend's
    /// skill × topic matrix reads them per question so that a future mixed
    /// document — or per-question topics — needs no schema change.
    pub topics: Vec<Topic>,
    pub prompt: String,
    pub options: Vec<QuestionOption>,
}

impl PublicQuestion {
    pub fn new(q: &Question, topics: &[Topic]) -> Self {
        Self {
            id: q.id.clone(),
            format: q.format,
            skill: q.skill,
            topics: topics.to_vec(),
            prompt: q.prompt.clone(),
            options: q.options(),
        }
    }
}

/// Where a document is in the pipeline.
///
/// `Failed` carries no data itself — the reason lives in `DocMeta::error` — so
/// that a failed document is still a complete, queryable row rather than a
/// tagged union DynamoDB would have to model awkwardly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocStatus {
    /// Row written by `POST /docs`; the presigned URL has been handed out but
    /// the object may never arrive. These are the rows that accumulate when
    /// someone taps upload and then closes the tab.
    Pending,
    /// The S3 trigger has picked it up. Set *before* the guards run, so a
    /// document that trips the daily cap still shows movement in the UI rather
    /// than sitting on `pending` forever.
    Processing,
    Ready,
    Failed,
}

/// `DOC#<id>` / `META`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocMeta {
    pub pk: String,
    pub sk: String,

    pub doc_id: String,
    pub title: String,
    pub status: DocStatus,

    /// Present only when `status == Failed`. Written from `Error::Invalid` and
    /// friends, never from a raw AWS error chain — see the note on
    /// `error::describe`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,

    /// Always exactly `docs/<doc_id>.pdf`. Stored anyway rather than recomputed
    /// so that a future change to the key layout does not orphan old rows.
    pub s3_key: String,

    /// Chosen by the model from the document, not supplied by the reader.
    ///
    /// **Empty until `status == Ready`.** The row exists from the moment the
    /// upload URL is handed out, which is before anything has read the PDF, so
    /// a `pending` document genuinely has no topics and the list view renders
    /// it without them.
    ///
    /// This used to be the reader's job, on the reasoning that a model cannot
    /// hallucinate a topic it is never asked for. True, and beside the point:
    /// the closed vocabulary it had to choose from contained no tag for a
    /// housing report, so the reader picked a wrong one instead. The vocabulary
    /// is now open — see `tags::Topic`.
    #[serde(default)]
    pub topics: Vec<Topic>,
    pub tag_version: u32,

    /// Client-reported at creation, **overwritten with the authoritative count**
    /// when `generate` parses the file.
    ///
    /// The browser counts pages with pdf-lib so an over-long PDF costs zero API
    /// calls, which is a good optimisation and a worthless guard: it is a number
    /// from the client. `MAX_PAGES` is enforced in `generate` against a count
    /// this process derived, and that count replaces this field.
    #[serde(default)]
    pub page_count: usize,

    /// Empty until `status == Ready`. Includes the answer key; this field is
    /// the reason `DocMeta` is never returned to the browser as-is.
    #[serde(default)]
    pub questions: Vec<Question>,

    /// `questions.len()`, denormalized.
    ///
    /// Redundant everywhere except `GET /docs`, whose projection excludes
    /// `questions` entirely — and DynamoDB has no way to project the *length*
    /// of a list attribute. Without this the list view would have to read the
    /// full question payload for every document just to render a count.
    #[serde(default)]
    pub question_count: usize,

    pub created_at: String,

    /// Set when the S3 trigger claims the document. Distinguishes "created and
    /// abandoned" from "actually cost money".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub processed_at: Option<String>,

    /// How many times generation has failed for infrastructure reasons.
    ///
    /// **This field is what stops a document being stranded.** An
    /// infrastructure failure is retryable, so the handler puts the document
    /// back to `pending` and fails the invocation so Lambda retries it. But
    /// Lambda's retries are finite: once they are exhausted the event goes to
    /// the dead-letter queue and *nothing* will ever deliver another S3 event
    /// for an object that already exists. The document then sits at `pending`
    /// for good — with no error text and, because the UI only offers Retry on
    /// `failed`, no way for the reader to do anything about it.
    ///
    /// That is not hypothetical: it is exactly what an IAM misconfiguration
    /// produced, and the symptom was a document that said "Queued" forever.
    ///
    /// Counting the failures here lets the handler recognise its own last
    /// attempt and write `failed` instead of `pending`, which puts the document
    /// back in reach of the Retry button. Reset on success and on a
    /// user-initiated retry — it counts consecutive failures, not lifetime ones.
    #[serde(default)]
    pub generation_attempts: u32,
}

/// The projection used by `GET /docs`.
///
/// Deliberately a separate struct from `DocMeta` with a matching
/// `ProjectionExpression`: the list view has no use for `questions`, and
/// `questions` is by far the largest attribute on the row. Reading it back for
/// every document on every list would multiply the scan's consumed capacity by
/// roughly twenty for no benefit, on a table provisioned at 5 RCU — and the
/// frontend polls this endpoint every five seconds while anything is unsettled.
#[derive(Debug, Clone)]
pub struct DocSummary {
    pub doc_id: String,
    pub title: String,
    pub status: DocStatus,
    pub error: Option<String>,
    pub topics: Vec<Topic>,
    pub tag_version: u32,
    pub page_count: usize,
    pub created_at: String,
    /// Filled in by the store from the same scan that produced the row, not by
    /// a follow-up query per document.
    pub attempt_count: usize,
    pub question_count: usize,
}

/// One graded answer, as stored.
///
/// Storing the per-question breakdown rather than a total is the difference
/// between tags being useful and being decorative. A stored total can never be
/// re-segmented: the history screen's skill × topic matrix is computed from
/// these rows, and if in six months you want to know how `causal` questions
/// about `energy` documents went last spring, either the answer is in here or
/// it does not exist.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttemptResponse {
    pub qid: String,
    pub format: QuestionFormat,
    /// Copied from the question at submit time, for the same reason `topics` is
    /// copied: an attempt is a record of what was true when it was taken.
    pub skill: Skill,
    /// Copied from the document at submit time. Re-tagging a document later
    /// must not rewrite what past attempts mean.
    #[serde(default)]
    pub topics: Vec<Topic>,
    /// `None` means the question was left blank, which is not the same as
    /// wrong for the purpose of reading it back later.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<Choice>,
    pub correct: bool,
}

/// `DOC#<id>` / `ATTEMPT#<iso8601>`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attempt {
    pub pk: String,
    pub sk: String,

    /// Stable identifier, supplied by the client so a resubmission of the same
    /// attempt is recognisable. See `Store::put_attempt_once`.
    pub attempt_id: String,

    pub doc_id: String,
    /// Denormalized so `GET /history` is one scan rather than a scan plus a
    /// lookup per distinct document.
    pub doc_title: String,
    pub submitted_at: String,

    pub responses: Vec<AttemptResponse>,
    #[serde(default)]
    pub topics: Vec<Topic>,
    pub tag_version: u32,

    /// Client-reported wall clock for the attempt. Untrusted — it is a
    /// self-measurement on a personal tool, and there is nothing to gain by
    /// lying to yourself — but clamped in the handler so a bad clock cannot
    /// write a nonsense value that breaks every average afterwards.
    #[serde(default)]
    pub duration_ms: u64,

    /// Derived from `responses`, stored anyway so a future summary view does
    /// not have to re-reduce every attempt.
    pub score: usize,
    pub total: usize,
}

/// `AUTH` / `CHALLENGE#<b64url>`, and — during the one-shot enrolment described
/// in the `api` crate's `register` module — `AUTH` / `REGISTRATION#<uuid>`.
///
/// One type for both because the row *is* the same thing in both cases: an
/// opaque serialized ceremony state, addressed by the id the client will hand
/// back, that expires. Only `state`'s inner type differs, and neither side of
/// this crate ever looks inside it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChallengeItem {
    pub pk: String,
    pub sk: String,
    /// The serialized `PasskeyAuthentication` (or, for a registration row,
    /// `PasskeyRegistration`) state. webauthn-rs requires this to be held
    /// server-side between the two round trips; it binds the challenge to the
    /// credential set and the user-verification policy that were in force when
    /// it was issued.
    pub state: String,
    /// Unix seconds. Doubles as the DynamoDB TTL attribute and as an explicit
    /// check in the handler — TTL deletion is best-effort and can lag by up to
    /// 48 hours, so a 60-second expiry that is only enforced by TTL is a
    /// 48-hour expiry.
    pub expires_at: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The model is no longer asked for `format`, so every generated question
    /// arrives without one. If this default ever stops applying, ten good
    /// questions get discarded at deserialization — which is exactly what
    /// happened when the model *was* asked, and answered `"definitional"`.
    #[test]
    fn a_question_without_a_format_defaults_to_multiple_choice() {
        let q: Question = serde_json::from_str(
            r#"{"id":"q1","skill":"causal","prompt":"why?",
                "options":["a","b","c","d"],"answer":"a","explanation":"page 1"}"#,
        )
        .expect("format is optional");

        assert_eq!(q.format, QuestionFormat::MultipleChoice);
    }

    /// Stored rows written before the field was defaulted still carry it, and
    /// must keep round-tripping.
    #[test]
    fn an_explicit_format_still_deserializes() {
        let q: Question = serde_json::from_str(
            r#"{"id":"q1","format":"multiple_choice","skill":"causal","prompt":"why?",
                "options":["a","b","c","d"],"answer":"a","explanation":"page 1"}"#,
        )
        .expect("explicit format still accepted");

        assert_eq!(q.format, QuestionFormat::MultipleChoice);
    }

    /// A wrong value must still be refused rather than silently defaulted. The
    /// defaulting is for an *absent* field only — if serde ever started
    /// swallowing bad values, the closed vocabulary would be decorative.
    #[test]
    fn a_bogus_format_is_still_rejected() {
        let bogus: Result<Question, _> = serde_json::from_str(
            r#"{"id":"q1","format":"definitional","skill":"causal","prompt":"why?",
                "options":["a","b","c","d"],"answer":"a","explanation":"page 1"}"#,
        );
        assert!(
            bogus.is_err(),
            "an unknown format must not default silently"
        );
    }
}
