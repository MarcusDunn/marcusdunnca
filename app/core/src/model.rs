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

use crate::numeric::NumericAnswer;
use crate::tags::{Choice, Confidence, QuestionFormat, Skill, Topic};

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
///
/// # Why this is one flat struct rather than an enum
///
/// There are two kinds of question and they share no answer key, which is
/// exactly the shape a Rust enum is for. It is stored flat anyway, with the
/// key fields optional, for one reason: these rows go through `serde_dynamo`,
/// and every enum representation that would express the invariant properly —
/// `#[serde(tag)]`, `#[serde(untagged)]`, `#[serde(flatten)]` — either breaks
/// rows written before the second format existed (those carry no `format` at
/// all, so there is no tag to dispatch on) or leans on serde's content
/// buffering, which is where `serde_dynamo`'s sharp edges live.
///
/// The invariant is not abandoned, it is moved: [`Question::body`] is the only
/// way anything reads an answer key, and it returns `None` for a row whose
/// `format` and payload disagree. Grading calls it and refuses to score a
/// document it cannot grade, rather than marking the question wrong — a
/// question nobody can answer scoring zero is a confidently false statement
/// about the reader.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Question {
    pub id: String,
    /// Defaulted, because the model is still not asked for it.
    ///
    /// It has two legal values now, but choosing between them was never a
    /// judgement call: the generator requests multiple-choice and numeric
    /// questions in two separate arrays, so the handler already knows which is
    /// which and fills this in. Asking would only reintroduce the failure that
    /// removed the field in the first place — Sonnet answering
    /// `"format": "definitional"`, putting a skill in the format slot, and
    /// costing ten good questions.
    ///
    /// The default is `multiple_choice` because every row written before
    /// `numeric` existed was one. See [`QuestionFormat`]'s `Default` impl.
    #[serde(default)]
    pub format: QuestionFormat,
    /// Singular. One question tests one skill: the history view builds a
    /// skill × topic matrix, and a question belonging to three skills would
    /// either be counted three times or need a weighting rule nobody has
    /// specified.
    pub skill: Skill,
    pub prompt: String,
    /// Exactly four on a multiple-choice question, validated at generation time
    /// so [`Choice::index`] is total. Empty on a numeric one.
    #[serde(default)]
    pub options: Vec<String>,
    /// The keyed letter. `None` on a numeric question, which has no letters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<Choice>,
    /// The keyed figure and the precision it demands. `None` on a
    /// multiple-choice question.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub numeric: Option<NumericAnswer>,
    pub explanation: String,
}

/// The answer key, in the shape its format actually has.
///
/// Borrowed rather than owned because it is produced per submission, per
/// question, and copying four option strings to compare one letter is work for
/// nothing.
#[derive(Debug)]
pub enum QuestionBody<'a> {
    MultipleChoice {
        options: &'a [String],
        answer: Choice,
    },
    Numeric(&'a NumericAnswer),
}

impl Question {
    /// Pair each option with its letter. Empty for a numeric question.
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

    /// The answer key, or `None` if this row's `format` and payload disagree.
    ///
    /// **This is the single place the flat storage shape is turned back into
    /// the sum type it really is**, and the single place a malformed row is
    /// detectable. Generation validates that it never writes one; this exists
    /// because "generation validates it" is a statement about code that can
    /// change, and the cost of being wrong is a silently unscoreable quiz.
    pub fn body(&self) -> Option<QuestionBody<'_>> {
        match self.format {
            QuestionFormat::MultipleChoice => {
                let answer = self.answer?;
                // A keyed letter that indexes past the options is the same
                // corruption as a missing one, and `Choice::index` is only
                // total because this is checked.
                if answer.index() >= self.options.len() {
                    return None;
                }
                Some(QuestionBody::MultipleChoice {
                    options: &self.options,
                    answer,
                })
            }
            QuestionFormat::Numeric => self.numeric.as_ref().map(QuestionBody::Numeric),
        }
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
    /// Four on a multiple-choice question, empty on a numeric one.
    pub options: Vec<QuestionOption>,
    /// How close a typed answer has to be. Numeric questions only.
    ///
    /// **This is not the key and does not narrow it.** It says the answer is
    /// wanted to within a point, which is a statement about precision — the
    /// same statement the question would make in prose if the field did not
    /// exist. Withholding it would not protect anything; it would just mean
    /// guessing at how exact to be, which is the one thing this format is
    /// designed to take out of the reader's hands.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tolerance: Option<f64>,
    /// `"%"`, `"$B"`. Numeric questions only; rendered beside the input so the
    /// reader is not left inferring the denomination from the prompt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
}

impl PublicQuestion {
    pub fn new(q: &Question, topics: &[Topic]) -> Self {
        // Read through `body()` rather than off the struct, so a numeric
        // question can only ever contribute the two fields that are safe to
        // send. Reaching for `q.numeric` directly here is how `value` would
        // one day arrive on the wire.
        let numeric = match q.body() {
            Some(QuestionBody::Numeric(answer)) => Some(answer),
            _ => None,
        };

        Self {
            id: q.id.clone(),
            format: q.format,
            skill: q.skill,
            topics: topics.to_vec(),
            prompt: q.prompt.clone(),
            options: q.options(),
            tolerance: numeric.map(|n| n.tolerance),
            unit: numeric.map(|n| n.unit.clone()),
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
    /// The chosen letter on a multiple-choice question.
    ///
    /// `None` means the question was left blank, which is not the same as
    /// wrong for the purpose of reading it back later — and, on a numeric
    /// question, means nothing at all. Check `answer_text` there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<Choice>,
    /// What the reader typed on a numeric question, **verbatim**.
    ///
    /// Stored as the string rather than the parsed float on purpose. `"about
    /// 4"` does not parse, and the interesting thing about that answer is not
    /// that it scored zero — it is that the reader hedged. A float cannot
    /// record that; the string can.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer_text: Option<String>,
    /// How sure the reader said they were.
    ///
    /// `None` on attempts recorded before confidence was asked for. **That is
    /// not the same as `Guessing`** and must never be coerced to it: those
    /// answers were given without the question being put, so they carry no
    /// information about calibration and have to be excluded from any
    /// reliability estimate rather than pooled in at the bottom band.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<Confidence>,
    /// Points earned, under the scoring rule in force when this was submitted.
    ///
    /// Denormalized deliberately. If the points table is ever retuned, a stored
    /// value keeps this attempt reporting the score it was actually given,
    /// which is the same reasoning that copies `skill` and `topics` onto the
    /// row instead of joining back. Zero on a response with no confidence.
    #[serde(default)]
    pub points: i32,
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

    /// Sum of `responses[].points`. Can be negative — that is the point of it.
    ///
    /// **Reported alongside `score`, never instead of it.** They answer
    /// different questions: `score` is how much you knew, `points` is how well
    /// you knew what you knew. Collapsing the two into one number destroys the
    /// only diagnostic the confidence bands produce, because six confident
    /// rights and four confident wrongs scores the same as ten hedged rights
    /// and looks nothing like it.
    #[serde(default)]
    pub points: i32,
    /// `total * MAX_POINTS_PER_QUESTION`, so a points total can be read against
    /// its ceiling. Stored rather than computed for the same reason as
    /// `points`: a change to the rule must not restate old attempts.
    #[serde(default)]
    pub max_points: i32,
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

    fn numeric_question() -> Question {
        Question {
            id: "n1".into(),
            format: QuestionFormat::Numeric,
            skill: Skill::FigureRecall,
            prompt: "What was the 2026 forecast change in Ontario home prices?".into(),
            options: Vec::new(),
            answer: None,
            numeric: Some(NumericAnswer {
                value: -4.0,
                tolerance: 1.0,
                unit: "%".into(),
            }),
            explanation: "Table 1, Ontario row.".into(),
        }
    }

    /// The numeric equivalent of the multiple-choice leak test. The key here is
    /// a *number*, so the substring that must not appear is the number.
    #[test]
    fn a_numeric_quiz_payload_cannot_contain_the_figure() {
        let topic = Topic::parse("housing").expect("a valid topic");
        let public = PublicQuestion::new(&numeric_question(), &[topic]);
        let json = serde_json::to_string(&public).expect("serializes");

        assert!(!json.contains("value"), "leaked the key's field: {json}");
        assert!(!json.contains("-4"), "leaked the figure itself: {json}");
        assert!(!json.contains("explanation"), "leaked rationale: {json}");

        // The precision hint and the denomination are supposed to be there —
        // withholding them would only make the reader guess how exact to be.
        assert!(json.contains("tolerance"));
        assert!(json.contains("unit"));
        // And no empty option list masquerading as a four-option question.
        assert!(json.contains(r#""options":[]"#));
    }

    /// A multiple-choice question must not sprout the numeric hint fields, or
    /// the client's discriminated union stops discriminating.
    #[test]
    fn a_multiple_choice_payload_carries_no_numeric_fields() {
        let q: Question = serde_json::from_str(
            r#"{"id":"q1","skill":"causal","prompt":"why?",
                "options":["w","x","y","z"],"answer":"a","explanation":"page 1"}"#,
        )
        .expect("parses");
        let topic = Topic::parse("fiscal").expect("a valid topic");
        let json = serde_json::to_string(&PublicQuestion::new(&q, &[topic])).expect("serializes");

        assert!(!json.contains("tolerance"));
        assert!(!json.contains("unit"));
    }

    #[test]
    fn body_returns_the_key_in_the_shape_the_format_promises() {
        let mc: Question = serde_json::from_str(
            r#"{"id":"q1","skill":"causal","prompt":"why?",
                "options":["w","x","y","z"],"answer":"b","explanation":"page 1"}"#,
        )
        .expect("parses");

        assert!(matches!(
            mc.body(),
            Some(QuestionBody::MultipleChoice {
                answer: Choice::B,
                ..
            })
        ));
        assert!(matches!(
            numeric_question().body(),
            Some(QuestionBody::Numeric(_))
        ));
    }

    /// The reason `body` returns an `Option` at all. Each of these is a row
    /// that deserializes perfectly and cannot be graded, and each must be
    /// detectable in one place rather than producing a question that scores
    /// zero however it is answered.
    #[test]
    fn a_row_whose_format_and_payload_disagree_has_no_body() {
        let mut orphan = numeric_question();
        orphan.numeric = None;
        assert!(orphan.body().is_none(), "numeric with no figure");

        let mut mislabelled = numeric_question();
        mislabelled.format = QuestionFormat::MultipleChoice;
        assert!(mislabelled.body().is_none(), "no options and no letter");

        let mut short: Question = serde_json::from_str(
            r#"{"id":"q1","skill":"causal","prompt":"why?",
                "options":["w","x"],"answer":"d","explanation":"page 1"}"#,
        )
        .expect("parses");
        assert!(
            short.body().is_none(),
            "the letter indexes past the options"
        );
        short.answer = None;
        assert!(short.body().is_none(), "multiple choice with no letter");
    }

    /// Legacy rows predate both new fields and must keep reading.
    #[test]
    fn an_attempt_response_without_confidence_still_deserializes() {
        let r: AttemptResponse = serde_json::from_str(
            r#"{"qid":"q1","format":"multiple_choice","skill":"causal",
                "topics":["fiscal"],"answer":"a","correct":true}"#,
        )
        .expect("legacy response still reads");

        assert_eq!(r.confidence, None, "absent is not `guessing`");
        assert_eq!(r.points, 0);
        assert_eq!(r.answer_text, None);
    }
}
