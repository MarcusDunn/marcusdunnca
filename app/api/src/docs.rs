//! Document routes: create, retry, list, download URL, quiz, submit.
//!
//! Every response type here is shaped to `web/src/lib/schemas.ts`, which
//! Zod-parses at the boundary. A missing or misspelled key does not degrade
//! gracefully — it throws a `SchemaError` and the screen goes blank — so the
//! wire structs are separate from the stored ones and carry
//! `rename_all = "camelCase"` explicitly rather than relying on field names
//! happening to match.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use trainer_core::clock;
use trainer_core::error::{aws, Error, Result};
use trainer_core::keys;
use trainer_core::model::{
    Attempt, AttemptResponse, DocMeta, DocStatus, PublicQuestion, Question, QuestionOption,
};
use trainer_core::store::AttemptWrite;
use trainer_core::tags::{Choice, Topic, TAG_VERSION};

use crate::state::AppState;

/// `POST /docs` serves two jobs, distinguished by which variant parses.
///
/// Folding retry into the create route rather than adding `POST
/// /docs/:id/retry` keeps the endpoint list as specified. The alternative the
/// frontend rejected — re-uploading the same PDF as a fresh document — is
/// precisely the zombie accumulation the retry button exists to prevent.
///
/// `untagged` because the frontend sends one shape or the other with no
/// discriminant field. Variant order matters: `Retry` is tried first because
/// `Upload`'s fields are all required, so a retry body cannot accidentally
/// parse as an upload, but a serde `untagged` error message is much clearer
/// when the narrower variant is first.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum CreateDocRequest {
    Retry(RetryRequest),
    Upload(UploadRequest),
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetryRequest {
    pub retry_of: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadRequest {
    /// The name of the file being uploaded, used as a **provisional** title.
    ///
    /// Neither the title nor the topics are asked of the uploader any more —
    /// the model derives both from the document itself, which is strictly
    /// better information than a filename and removes the two fields that made
    /// uploading a form rather than a file picker. But the row exists before
    /// generation runs, and a document listed as `pending` with no title at all
    /// is unidentifiable while it is the only thing the reader is waiting on.
    ///
    /// So this is a placeholder with a lifetime of about a minute. `generate`
    /// overwrites it. It is never sent to the model — see the note on the
    /// document block's `name` in `bedrock.rs`, which is a prompt injection
    /// vector by AWS's own warning.
    pub filename: String,
    /// Counted in the browser with pdf-lib. Recorded, but **not trusted**: the
    /// authoritative count is derived in `generate` and overwrites this.
    pub page_count: usize,
    pub content_type: String,
    /// The exact byte length of the file about to be uploaded.
    ///
    /// **Required, and the reason `schemas.ts` needs a `sizeBytes` field.** See
    /// [`create_upload`] — without it the presigned PUT cannot bound the size of
    /// what is written, and a pinned-key presigned URL still accepts a 5 GB
    /// object.
    pub size_bytes: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateDocResponse {
    pub id: String,
    /// `null` on a retry: the PDF is already in S3 and must not be uploaded
    /// again.
    pub upload_url: Option<String>,
}

pub async fn create(state: &AppState, req: CreateDocRequest) -> Result<CreateDocResponse> {
    match req {
        CreateDocRequest::Retry(r) => retry(state, &r.retry_of).await,
        CreateDocRequest::Upload(u) => create_upload(state, u).await,
    }
}

/// Reserve an id, write the `pending` row, hand back an upload URL.
///
/// The presigned URL is the only thing standing between an authenticated
/// session and arbitrary writes to the documents bucket, so all three of these
/// are pinned into the signature:
///
/// **Key.** `docs/<uuid>.pdf`, generated here. Never taken from the request. A
/// caller-supplied key — even one the server prefixes — lets an upload land
/// somewhere the S3 trigger interprets differently, and the trigger derives a
/// DynamoDB partition key from that path.
///
/// **Content-Type.** `application/pdf`. The bucket serves nothing directly, so
/// this is not about browser sniffing; it is about `generate` being able to
/// state, without checking, what it is handing to Bedrock's document block.
///
/// **Content-Length.** The exact declared size, checked against the cap here
/// *before* signing.
///
/// That last one deserves its own note, because the obvious approach does not
/// work. A `content-length-range` condition is a feature of browser POST
/// policies (`createPresignedPost`), which the Rust SDK does not implement, and
/// SigV4 query presigning has no equivalent. Without *something*, a presigned
/// PUT with a pinned key still accepts an object of any size — a 5 GB upload to
/// a pinned key is still a 5 GB bill.
///
/// Setting `.content_length()` puts `content-length` into `X-Amz-SignedHeaders`
/// (verified against the SDK: the query string carries
/// `X-Amz-SignedHeaders=content-length;content-type;host`), so S3 rejects any
/// PUT whose actual length differs from the signed one. That is strictly
/// stronger than a range: the size is not bounded, it is fixed at the value the
/// server approved.
///
/// It costs the client nothing. `uploadToS3` sends a `File` body, so the browser
/// sets `Content-Length` to `file.size` automatically and the signed set is
/// reproduced exactly — the client already hardcodes the matching content-type.
async fn create_upload(state: &AppState, req: UploadRequest) -> Result<CreateDocResponse> {
    let title = provisional_title(&req.filename);

    // The signature pins this value, so a mismatch here is not a style check —
    // it would produce an upload URL the browser cannot use.
    if req.content_type != "application/pdf" {
        return Err(Error::Invalid("only application/pdf is accepted".into()));
    }

    if req.size_bytes <= 0 {
        return Err(Error::Invalid(
            "sizeBytes must be a positive byte count".into(),
        ));
    }
    if req.size_bytes > state.max_upload_bytes {
        return Err(Error::Invalid(format!(
            "that file is {:.1} MB; the limit is {:.0} MB",
            req.size_bytes as f64 / 1_000_000.0,
            state.max_upload_bytes as f64 / 1_000_000.0,
        )));
    }

    let doc_id = uuid::Uuid::new_v4().to_string();
    let s3_key = keys::s3_key(&doc_id);

    let meta = DocMeta {
        pk: keys::doc_pk(&doc_id),
        sk: keys::META_SK.to_string(),
        doc_id: doc_id.clone(),
        title,
        status: DocStatus::Pending,
        error: None,
        s3_key: s3_key.clone(),
        // Empty until the model chooses them. The list view renders a document
        // with no topics without complaint; the alternative — asking the
        // uploader for tags the model is about to overwrite — is the thing
        // this change removes.
        topics: Vec::new(),
        tag_version: TAG_VERSION,
        page_count: req.page_count,
        questions: Vec::new(),
        question_count: 0,
        created_at: clock::now_iso8601(),
        processed_at: None,
        generation_attempts: 0,
    };

    // Written before the URL is handed out. If this fails there is no upload
    // URL in existence, so there is no way to create an object the trigger will
    // find no metadata row for — which is the state that would otherwise
    // produce a permanently invisible, permanently billed object.
    state.store.create_doc(&meta).await?;

    let presigned = state
        .s3
        .put_object()
        .bucket(&state.docs_bucket)
        .key(&s3_key)
        .content_type("application/pdf")
        .content_length(req.size_bytes)
        .presigned(state.presigning()?)
        .await
        .map_err(aws)?;

    Ok(CreateDocResponse {
        id: doc_id,
        upload_url: Some(presigned.uri().to_string()),
    })
}

/// A readable stand-in title, derived from the uploaded filename.
///
/// This is shown for roughly the minute between the row being created and
/// `generate` replacing it with the model's title, and it only has to be good
/// enough to tell two pending uploads apart.
///
/// Never fails. A filename is not something to reject an upload over — the PDF
/// is the point, the name is incidental, and "your file name is invalid" is an
/// absurd thing to say to someone who picked a file from their phone. Anything
/// unusable degrades to a constant.
fn provisional_title(filename: &str) -> String {
    let stem = filename
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(filename)
        .trim_end_matches(".pdf")
        .trim_end_matches(".PDF");

    let cleaned: String = stem
        .chars()
        // Separators become spaces; control characters are dropped outright
        // rather than rendered. React escapes its output, so this is tidiness
        // rather than a defence, but a title containing a newline breaks the
        // list layout for no reason.
        .map(|c| if c == '_' || c == '-' { ' ' } else { c })
        .filter(|c| !c.is_control())
        .collect();

    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");

    // Bounded well under DynamoDB's item limit and under what the list can
    // show. The model's title, which replaces this, is bounded by the schema.
    let truncated: String = collapsed.chars().take(200).collect();

    if truncated.is_empty() {
        "Untitled document".to_string()
    } else {
        truncated
    }
}

/// Re-run generation for a document whose PDF is already in S3.
///
/// The trigger is S3 `ObjectCreated`, and there is deliberately no way for the
/// client to invoke `generate` directly — so re-running it means producing a
/// new `ObjectCreated` event for an object that already exists. A same-key
/// `CopyObject` does that: the bucket is versioned, so the copy writes a new
/// version and fires the notification, and no bytes cross the API.
///
/// **This requires the bucket notification to be configured for
/// `s3:ObjectCreated:*` rather than `s3:ObjectCreated:Put`** — a self-copy
/// raises `ObjectCreated:Copy`, and a Put-only filter would silently do
/// nothing, leaving the document stuck on `pending` with no error to show.
///
/// `HeadObject` runs first so that a document which failed *before* its upload
/// ever landed gets a message saying so, rather than being reset to `pending`
/// and waiting forever for an event that cannot happen. That stuck-pending row
/// is the zombie this route exists to avoid creating.
async fn retry(state: &AppState, doc_id: &str) -> Result<CreateDocResponse> {
    let doc = state.store.get_doc(doc_id).await?.ok_or(Error::NotFound)?;

    if doc.status == DocStatus::Ready {
        return Err(Error::Invalid("this document already has questions".into()));
    }

    state
        .s3
        .head_object()
        .bucket(&state.docs_bucket)
        .key(&doc.s3_key)
        .send()
        .await
        .map_err(|e| {
            tracing::warn!(doc_id, error = %trainer_core::error::describe(&e), "retry: object missing");
            Error::Invalid(
                "the PDF was never uploaded for this document; upload it again instead".into(),
            )
        })?;

    // Armed before the copy, not after. If the status write succeeds and the
    // copy fails the document sits on `pending` and the user can press retry
    // again; if the copy fired first and the status write failed, the generate
    // handler would find a non-pending document and skip it, consuming the
    // retry silently.
    if !state.store.arm_retry(doc_id).await? {
        return Err(Error::Invalid(
            "this document is already being processed".into(),
        ));
    }

    let source = format!("{}/{}", state.docs_bucket, doc.s3_key);
    state
        .s3
        .copy_object()
        .bucket(&state.docs_bucket)
        .key(&doc.s3_key)
        .copy_source(urlencoding::encode(&source).into_owned())
        // Without REPLACE, S3 refuses a copy onto the same key ("This copy
        // request is illegal because it is trying to copy an object to itself
        // without changing the object's metadata"). Restating the content type
        // is the smallest legal change.
        .metadata_directive(aws_sdk_s3::types::MetadataDirective::Replace)
        .content_type("application/pdf")
        .send()
        .await
        .map_err(aws)?;

    Ok(CreateDocResponse {
        id: doc.doc_id,
        // Null: the PDF is already there. The client skips the PUT entirely.
        upload_url: None,
    })
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentSummaryDto {
    pub id: String,
    pub title: String,
    pub topics: Vec<Topic>,
    pub status: DocStatus,
    pub page_count: usize,
    pub created_at: String,
    pub error: Option<String>,
    pub attempt_count: usize,
}

#[derive(Debug, Serialize)]
pub struct DocumentListResponse {
    pub documents: Vec<DocumentSummaryDto>,
}

/// `GET /docs`
///
/// Polled every five seconds by the client while any document is unsettled,
/// which is why the underlying scan projects away the question payload.
pub async fn list(state: &AppState) -> Result<DocumentListResponse> {
    let documents = state
        .store
        .list_docs()
        .await?
        .into_iter()
        .map(|d| DocumentSummaryDto {
            id: d.doc_id,
            title: d.title,
            topics: d.topics,
            status: d.status,
            page_count: d.page_count,
            created_at: d.created_at,
            error: d.error,
            attempt_count: d.attempt_count,
        })
        .collect();

    Ok(DocumentListResponse { documents })
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadUrlResponse {
    pub url: String,
    pub expires_at: String,
}

/// `GET /docs/:id/url` — a short-lived presigned GET so the reader can see the
/// PDF it is being quizzed on.
///
/// The document row is looked up first even though the S3 key is derivable from
/// the id. That turns "no such document" into a 404 instead of a signed URL
/// that 404s at S3 — and, more usefully, means this route cannot be used to
/// probe which object keys exist in the bucket.
pub async fn download_url(state: &AppState, doc_id: &str) -> Result<DownloadUrlResponse> {
    let doc = state.store.get_doc(doc_id).await?.ok_or(Error::NotFound)?;

    let presigned = state
        .s3
        .get_object()
        .bucket(&state.docs_bucket)
        .key(&doc.s3_key)
        .presigned(state.presigning()?)
        .await
        .map_err(aws)?;

    Ok(DownloadUrlResponse {
        url: presigned.uri().to_string(),
        // Computed from the same TTL the signature uses. The client caches this
        // query with `staleTime: Infinity` and offers a manual reload, so the
        // expiry is what tells it the reload is needed.
        expires_at: clock::iso_at(clock::unix_now() + crate::state::PRESIGN_TTL.as_secs() as i64),
    })
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuizResponse {
    pub document_id: String,
    /// [`PublicQuestion`], not [`Question`]. The type has no `answer` and no
    /// `explanation` field, so this response *cannot* carry the key — see the
    /// doc comment on `PublicQuestion` for why that is a type-level property
    /// rather than a line of code that strips fields.
    pub questions: Vec<PublicQuestion>,
    /// Not in the Zod schema (which strips unknown keys), kept because the read
    /// screen showing a title it did not have to look up separately is worth one
    /// string.
    pub title: String,
}

/// `GET /docs/:id/quiz`
pub async fn quiz(state: &AppState, doc_id: &str) -> Result<QuizResponse> {
    let doc = state.store.get_doc(doc_id).await?.ok_or(Error::NotFound)?;

    // A document that is not ready has no questions, and returning an empty
    // quiz would fail the client's `min(1)` and blank the screen. Report the
    // actual state so the UI can show a spinner or the failure message.
    match doc.status {
        DocStatus::Ready => {}
        DocStatus::Failed => {
            return Err(Error::Invalid(
                doc.error
                    .unwrap_or_else(|| "question generation failed".into()),
            ))
        }
        DocStatus::Pending | DocStatus::Processing => {
            return Err(Error::Invalid("questions are not ready yet".into()))
        }
    }

    Ok(QuizResponse {
        document_id: doc.doc_id,
        questions: doc
            .questions
            .iter()
            .map(|q| PublicQuestion::new(q, &doc.topics))
            .collect(),
        title: doc.title,
    })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitRequest {
    pub answers: Vec<SubmittedAnswer>,
    /// Client-generated, stable across retries of the same submission.
    ///
    /// **Not currently in `schemas.ts` — it needs adding.** Optional here so the
    /// route works before the frontend sends it, but while it is absent every
    /// resubmission writes a second attempt, and a double-tap on a flaky phone
    /// connection permanently skews every rate on the history screen.
    #[serde(default)]
    pub attempt_id: Option<String>,
    /// Also not in `schemas.ts`. The data model has a place for it; without a
    /// client value it stores zero, which is honest.
    #[serde(default)]
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmittedAnswer {
    pub question_id: String,
    /// `"a"` through `"d"`. A value outside that set is a deserialization
    /// error, which is a 400 rather than a silently ungraded question.
    pub option_id: Choice,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GradedQuestionDto {
    pub question_id: String,
    pub format: trainer_core::tags::QuestionFormat,
    pub skill: trainer_core::tags::Skill,
    pub topics: Vec<Topic>,
    pub prompt: String,
    pub options: Vec<QuestionOption>,
    /// `null` when the question was skipped. Distinguished from wrong because
    /// "I did not answer eight of ten" and "I got eight of ten wrong" are
    /// different signals about the same score.
    pub selected_option_id: Option<Choice>,
    pub correct_option_id: Choice,
    pub correct: bool,
    pub explanation: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmitResponse {
    pub attempt_id: String,
    pub document_id: String,
    pub submitted_at: String,
    pub correct: usize,
    pub total: usize,
    /// Carries the key, correctly: the attempt has already been written by the
    /// time this is built, so it is no longer secret for this attempt. This is
    /// the *only* response type that includes it.
    pub questions: Vec<GradedQuestionDto>,
}

/// Upper bound on a stored duration: 24 hours.
///
/// Client-reported and there is no incentive to lie to yourself, so this is not
/// validation so much as sanitisation. The bound exists because a phone that
/// sleeps mid-quiz, or a clock that steps, produces values that are not wrong
/// so much as meaningless, and one of them in the data makes every average
/// useless afterwards.
const MAX_DURATION_MS: u64 = 24 * 60 * 60 * 1000;

/// `POST /docs/:id/submit` — grade, record, explain.
///
/// Grading is `submitted == stored answer`. No model call: the key was decided
/// once, at generation time, and re-deriving it per submission would be slower,
/// cost money, and — worst — be non-deterministic, so the same answers could
/// score differently on two attempts at the same document.
///
/// Every question in the document produces a response row, including ones the
/// client did not mention. A client that omits a question must not thereby
/// shrink the denominator.
pub async fn submit(state: &AppState, doc_id: &str, req: SubmitRequest) -> Result<SubmitResponse> {
    let doc = state.store.get_doc(doc_id).await?.ok_or(Error::NotFound)?;

    if doc.status != DocStatus::Ready {
        return Err(Error::Invalid("this document has no questions".into()));
    }

    // Last write wins on a duplicated question id. Arbitrary, but it has to be
    // something, and a duplicate is a client bug rather than an attack — the
    // key is not in the client's hands, so there is nothing to gain by sending
    // a question twice.
    let submitted: HashMap<&str, Choice> = req
        .answers
        .iter()
        .map(|a| (a.question_id.as_str(), a.option_id))
        .collect();

    // Reject answers for questions that do not exist rather than ignoring them.
    // Silently dropping them hides a client/server question-set mismatch, which
    // is the bug that would make scores wrong without making them look wrong.
    if let Some(unknown) = submitted
        .keys()
        .find(|qid| !doc.questions.iter().any(|q| q.id == **qid))
    {
        return Err(Error::Invalid(format!("no such question: {unknown}")));
    }

    let mut responses = Vec::with_capacity(doc.questions.len());
    let mut score = 0usize;

    for question in &doc.questions {
        let chosen = submitted.get(question.id.as_str()).copied();
        let correct = chosen == Some(question.answer);
        if correct {
            score += 1;
        }

        responses.push(AttemptResponse {
            qid: question.id.clone(),
            format: question.format,
            skill: question.skill,
            // Snapshots, both of them. Re-tagging a document later must not
            // rewrite what past attempts mean.
            topics: doc.topics.clone(),
            answer: chosen,
            correct,
        });
    }

    let submitted_at = clock::now_iso8601();
    let total = doc.questions.len();

    // Server-generated when the client sends none. That gives the response a
    // stable id to return but provides no deduplication — nothing links two
    // submissions of the same answers. See the field comment.
    let attempt_id = req
        .attempt_id
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    let attempt = Attempt {
        pk: keys::doc_pk(doc_id),
        sk: keys::attempt_sk(&submitted_at),
        attempt_id: attempt_id.clone(),
        doc_id: doc_id.to_string(),
        doc_title: doc.title.clone(),
        submitted_at: submitted_at.clone(),
        responses,
        topics: doc.topics.clone(),
        tag_version: TAG_VERSION,
        duration_ms: req.duration_ms.unwrap_or(0).min(MAX_DURATION_MS),
        score,
        total,
    };

    // Written before the response is built. If the write fails the caller gets
    // an error and can retry, rather than seeing their score and losing it.
    let stored = match state.store.put_attempt_once(&attempt).await? {
        AttemptWrite::Created => attempt,
        // A duplicate submission replays the original, so a lost response
        // followed by a retry shows the same result rather than a second
        // sitting with a later timestamp.
        AttemptWrite::AlreadyRecorded(existing) => match *existing {
            Some(original) => {
                tracing::info!(doc_id, "duplicate submission; replaying original attempt");
                original
            }
            // Marker without an attempt. Unreachable via the transaction, but
            // if it ever happens, grading what we have beats a 500.
            None => attempt,
        },
    };

    Ok(SubmitResponse {
        attempt_id: stored.attempt_id.clone(),
        document_id: stored.doc_id.clone(),
        submitted_at: stored.submitted_at.clone(),
        correct: stored.score,
        total: stored.total,
        questions: grade_all(&doc.questions, &doc.topics, &stored),
    })
}

/// Build the graded view from the *stored* attempt, not from the request.
///
/// So a replayed duplicate reports what was recorded rather than what was just
/// sent — those are the same thing on the happy path, and the difference is
/// exactly what makes the replay correct when they are not.
fn grade_all(
    questions: &[Question],
    topics: &[Topic],
    attempt: &Attempt,
) -> Vec<GradedQuestionDto> {
    let recorded: HashMap<&str, &AttemptResponse> = attempt
        .responses
        .iter()
        .map(|r| (r.qid.as_str(), r))
        .collect();

    questions
        .iter()
        .map(|q| {
            let response = recorded.get(q.id.as_str());
            GradedQuestionDto {
                question_id: q.id.clone(),
                format: q.format,
                skill: q.skill,
                topics: topics.to_vec(),
                prompt: q.prompt.clone(),
                options: q.options(),
                selected_option_id: response.and_then(|r| r.answer),
                correct_option_id: q.answer,
                correct: response.is_some_and(|r| r.correct),
                explanation: q.explanation.clone(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use trainer_core::tags::{QuestionFormat, Skill};

    use super::*;

    fn sample_question() -> Question {
        Question {
            id: "q3".into(),
            format: QuestionFormat::MultipleChoice,
            skill: Skill::FigureRecall,
            prompt: "What was the deficit?".into(),
            options: vec!["one".into(), "two".into(), "three".into(), "four".into()],
            answer: Choice::B,
            explanation: "Table 2, line 4.".into(),
        }
    }

    /// The regression test for the one bug that would quietly make the app
    /// pointless. It asserts on the *serialized bytes*, not on the struct,
    /// because the failure mode is a field appearing on the wire.
    #[test]
    fn quiz_payload_cannot_contain_the_answer_key() {
        let q = sample_question();
        let public = PublicQuestion::new(&q, &[Topic::parse("fiscal").expect("a valid topic")]);
        let json = serde_json::to_string(&public).expect("serializes");

        assert!(
            !json.contains("answer"),
            "quiz payload leaked the key: {json}"
        );
        assert!(
            !json.contains("explanation"),
            "quiz payload leaked the explanation: {json}"
        );
        assert!(
            !json.contains("Table 2"),
            "quiz payload leaked rationale: {json}"
        );

        // And still contains everything `QuizQuestion` in schemas.ts requires.
        for required in ["id", "format", "skill", "topics", "prompt", "options"] {
            assert!(json.contains(required), "quiz payload missing {required}");
        }
    }

    /// Option ids are what the client posts back as `optionId`, so they have to
    /// be exactly the four letters the grader compares against.
    #[test]
    fn options_are_lettered_a_to_d() {
        let json = serde_json::to_string(&sample_question().options()).expect("serializes");
        assert_eq!(
            json,
            r#"[{"id":"a","text":"one"},{"id":"b","text":"two"},{"id":"c","text":"three"},{"id":"d","text":"four"}]"#
        );
    }

    /// A retry body must not be mistaken for an upload body, and vice versa.
    #[test]
    fn create_request_variants_are_unambiguous() {
        let retry: CreateDocRequest =
            serde_json::from_str(r#"{"retryOf":"abc"}"#).expect("retry parses");
        assert!(matches!(retry, CreateDocRequest::Retry(_)));

        let upload: CreateDocRequest = serde_json::from_str(
            r#"{"filename":"report.pdf","pageCount":4,
                "contentType":"application/pdf","sizeBytes":1024}"#,
        )
        .expect("upload parses");
        assert!(matches!(upload, CreateDocRequest::Upload(_)));
    }

    #[test]
    fn a_provisional_title_is_readable_and_never_fails() {
        assert_eq!(
            provisional_title("Provincial_Housing-Outlook.pdf"),
            "Provincial Housing Outlook"
        );
        // Phones and some browsers send a full path.
        assert_eq!(
            provisional_title("/private/var/tmp/td report.PDF"),
            "td report"
        );
        // Every one of these would be a 400 if this validated instead of
        // degrading, and none of them is a reason to refuse someone's PDF.
        assert_eq!(provisional_title(""), "Untitled document");
        assert_eq!(provisional_title(".pdf"), "Untitled document");
        assert_eq!(provisional_title("   "), "Untitled document");
        assert_eq!(provisional_title("a\nb.pdf"), "ab");
        assert_eq!(provisional_title(&"x".repeat(500)).chars().count(), 200);
    }

    #[test]
    fn an_option_id_outside_a_to_d_is_a_deserialization_error() {
        assert!(
            serde_json::from_str::<SubmittedAnswer>(r#"{"questionId":"q1","optionId":"e"}"#)
                .is_err()
        );
    }
}
