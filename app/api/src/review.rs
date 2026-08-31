//! `GET /review` and `POST /review/submit` — the spaced queue.
//!
//! # What a review is, and what it is not
//!
//! It is the same question, from a document read some time ago, graded by the
//! same arithmetic. It is **not** a new question about that document: writing
//! variants would transfer better to new phrasings, but it costs a model call
//! per review, and a schedule accumulated over months would be re-based every
//! time the model changed. The durable-retention result this feature is built
//! on — successive relearning — repeats the item you already have.
//!
//! The document is deliberately not shown. A review with the PDF on screen is a
//! reading comprehension test, not a retrieval one, and retrieval is the part
//! that produces retention.
//!
//! # The permutation
//!
//! Options are re-shuffled every repetition, because a question whose answer
//! stays at `c` stops testing the fact after the second sitting. The order is
//! stored on the schedule row rather than derived here, so what the grader
//! compares against is provably what was on screen. See `ReviewItem`.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use trainer_core::clock;
use trainer_core::error::{Error, Result};
use trainer_core::model::{DocMeta, DocStatus, Question, QuestionBody, QuestionOption};
use trainer_core::numeric;
use trainer_core::review::ReviewItem;
use trainer_core::shuffle;
use trainer_core::tags::{Choice, Confidence, QuestionFormat, Skill, Topic};

use crate::state::AppState;

/// Most questions one review session will offer.
///
/// A queue that says "84 due" is a queue nobody starts. Twenty is roughly ten
/// minutes, the oldest-due first, and finishing one session makes the next one
/// shorter — which is the property that keeps a backlog from becoming
/// permanent.
const MAX_SESSION: usize = 20;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewQuestionDto {
    pub question_id: String,
    pub document_id: String,
    pub document_title: String,
    pub format: QuestionFormat,
    pub skill: Skill,
    pub topics: Vec<Topic>,
    pub prompt: String,
    /// In the order this repetition presents them, which is not the order they
    /// are stored in. Carries no key — same rule as the quiz payload.
    pub options: Vec<QuestionOption>,
    pub tolerance: Option<f64>,
    pub unit: Option<String>,
    /// How many times this has been reviewed before. Shown so a first review
    /// and a fifth are distinguishable, which is most of what makes a queue
    /// feel like progress.
    pub reps: i32,
    pub due_at: String,
    /// When the document was read. Rendered as an age beside the prompt, so a
    /// question from a two-year-old report is visibly one.
    pub source_dated_at: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewQueueResponse {
    pub questions: Vec<ReviewQuestionDto>,
    /// Everything due, including what this session did not fit in.
    pub due_total: usize,
    /// The whole schedule, due or not — the denominator for "how much is being
    /// kept alive".
    pub scheduled_total: usize,
    /// Questions that have aged out of being worth asking.
    ///
    /// Reported rather than silently subtracted: a queue that shrinks for
    /// reasons nothing on screen explains is a queue you stop trusting.
    pub retired_total: usize,
    /// When the earliest not-yet-due item comes back, if nothing is due now.
    pub next_due_at: Option<String>,
}

/// `GET /review`
pub async fn queue(state: &AppState) -> Result<ReviewQueueResponse> {
    let now = clock::now_iso8601();
    let schedule = state.store.list_reviews().await?;
    let scheduled_total = schedule.len();

    let retired_total = schedule.iter().filter(|r| r.is_retired(&now)).count();

    // `is_due` already excludes retired items; the count above is what makes
    // that visible rather than a silent subtraction.
    let mut due: Vec<ReviewItem> = schedule
        .iter()
        .filter(|r| r.is_due(&now))
        .cloned()
        .collect();
    let due_total = due.len();

    // Oldest due first: those are the ones closest to being forgotten, which is
    // where a review is worth the most.
    due.sort_by(|a, b| a.due_at.cmp(&b.due_at));
    due.truncate(MAX_SESSION);

    let next_due_at = if due_total == 0 {
        schedule
            .iter()
            .filter(|r| !r.is_retired(&now))
            .map(|r| r.due_at.clone())
            .min()
    } else {
        None
    };

    // One document read per distinct document in the session, not per question.
    // A twenty-question session over three documents is three reads.
    let mut documents: HashMap<String, DocMeta> = HashMap::new();
    let mut questions = Vec::with_capacity(due.len());

    for item in &due {
        if !documents.contains_key(&item.doc_id) {
            match state.store.get_doc(&item.doc_id).await? {
                Some(doc) => {
                    documents.insert(item.doc_id.clone(), doc);
                }
                // The document was deleted, or never became ready. Skipping
                // rather than erroring: one orphaned schedule row must not make
                // the whole queue unreachable.
                None => {
                    tracing::warn!(doc_id = %item.doc_id, "review row for a missing document");
                    continue;
                }
            }
        }

        let Some(doc) = documents.get(&item.doc_id) else {
            continue;
        };
        if doc.status != DocStatus::Ready {
            continue;
        }
        let Some(question) = doc
            .questions
            .iter()
            .find(|q| q.id == item.qid && !q.is_void())
        else {
            // Regenerated away, or voided between the schedule being written
            // and now. Voiding deletes the row, so this is the race rather than
            // the normal path.
            tracing::warn!(qid = %item.qid, "review row for a question that is gone or void");
            continue;
        };

        questions.push(present(question, item, doc));
    }

    Ok(ReviewQueueResponse {
        questions,
        due_total,
        scheduled_total,
        retired_total,
        next_due_at,
    })
}

/// Build the payload for one question, in this repetition's option order.
fn present(question: &Question, item: &ReviewItem, doc: &DocMeta) -> ReviewQuestionDto {
    let numeric = question.numeric();
    let options = permuted_options(question, &item.presentation);

    ReviewQuestionDto {
        question_id: question.id.clone(),
        document_id: doc.doc_id.clone(),
        document_title: doc.title.clone(),
        format: question.format(),
        skill: question.skill,
        topics: doc.topics.clone(),
        prompt: question.prompt.clone(),
        options,
        tolerance: numeric.map(|n| n.tolerance),
        unit: numeric.map(|n| n.unit.clone()),
        reps: item.reps,
        due_at: item.due_at.clone(),
        source_dated_at: item.source_dated_at.clone(),
    }
}

/// Re-letter the options into the presentation order.
///
/// The returned ids are `a`–`d` **in presentation space**: the option shown
/// third is `c`, whatever its stored position. [`resolve`] is the inverse and
/// the two must stay together — a change to one that is not made to the other
/// marks correct answers wrong.
fn permuted_options(question: &Question, presentation: &[usize]) -> Vec<QuestionOption> {
    let stored = question.options();
    if presentation.len() != stored.len() {
        // A presentation that does not match the question — a regenerated
        // document, most likely. Fall back to stored order, which `resolve`
        // agrees with because it applies the same check.
        return stored;
    }

    shuffle::apply(&stored, presentation)
        .into_iter()
        .enumerate()
        .filter_map(|(i, option)| {
            Choice::ALL.get(i).map(|id| QuestionOption {
                id: *id,
                text: option.text,
            })
        })
        .collect()
}

/// Turn a letter the reader picked on screen back into the stored option it
/// refers to.
fn resolve(question: &Question, presentation: &[usize], picked: Choice) -> Option<usize> {
    let stored_len = question.options().len();
    if presentation.len() != stored_len {
        return Some(picked.index());
    }
    presentation.get(picked.index()).copied()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewSubmitRequest {
    pub answers: Vec<SubmittedReview>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmittedReview {
    pub document_id: String,
    pub question_id: String,
    /// The letter **as shown**, not as stored. See [`resolve`].
    #[serde(default)]
    pub option_id: Option<Choice>,
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub confidence: Option<Confidence>,
    /// The probability stated on the slider. Authoritative when present — the
    /// band is derived from it. See `docs::SubmittedAnswer::band`.
    #[serde(default)]
    pub confidence_percent: Option<u8>,
}

impl SubmittedReview {
    fn band(&self, format: QuestionFormat) -> (Option<Confidence>, Option<u8>) {
        match self.confidence_percent {
            Some(stated) => {
                let percent = stated.clamp(Confidence::chance_floor_percent(format), 100);
                (Some(Confidence::from_percent(percent)), Some(percent))
            }
            None => (self.confidence, None),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewResultDto {
    pub question_id: String,
    pub document_id: String,
    pub prompt: String,
    pub correct: bool,
    pub confidence: Option<Confidence>,
    pub confidence_percent: Option<u8>,
    pub score_bits: f64,
    /// In the order they were shown, so the results screen lines up with the
    /// question the reader just answered.
    pub options: Vec<QuestionOption>,
    pub correct_option_id: Option<Choice>,
    pub selected_option_id: Option<Choice>,
    pub selected_value: Option<String>,
    pub correct_value: Option<f64>,
    pub unit: Option<String>,
    pub explanation: String,
    /// When this question comes back. The interesting number: it is what the
    /// answer just bought.
    pub next_due_at: String,
    pub interval_days: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewSubmitResponse {
    pub correct: usize,
    pub total: usize,
    pub score_bits: f64,
    pub max_score_bits: f64,
    pub results: Vec<ReviewResultDto>,
}

/// `POST /review/submit`
///
/// Grades exactly as `docs::submit` does — letters compared, or a typed figure
/// against its tolerance — then advances each question's schedule.
///
/// **No attempt row is written.** A review is not a sitting of a document, and
/// pooling re-answers of a question you have already seen three times into the
/// history matrix would push every rate upward for reasons that have nothing to
/// do with comprehension. The outcome is kept on the schedule row instead; see
/// `ReviewItem::outcomes` for what a future history view could do with it.
pub async fn submit(state: &AppState, req: ReviewSubmitRequest) -> Result<ReviewSubmitResponse> {
    if req.answers.is_empty() {
        return Err(Error::Invalid("no answers were submitted".into()));
    }

    let now = clock::now_iso8601();

    // Grouped by document so each one is read once, and so the schedule rows
    // for a document are queried once rather than per answer.
    let mut by_document: HashMap<&str, Vec<&SubmittedReview>> = HashMap::new();
    for answer in &req.answers {
        by_document
            .entry(answer.document_id.as_str())
            .or_default()
            .push(answer);
    }

    let mut results = Vec::with_capacity(req.answers.len());
    let mut updated: Vec<ReviewItem> = Vec::new();
    let mut correct_count = 0usize;
    let mut score_bits = 0.0f64;
    let mut max_score_bits = 0.0f64;

    for (doc_id, answers) in by_document {
        let doc = state.store.get_doc(doc_id).await?.ok_or(Error::NotFound)?;
        let schedule = state.store.list_reviews_for_doc(doc_id).await?;

        for answer in answers {
            let Some(question) = doc
                .questions
                .iter()
                .find(|q| q.id == answer.question_id && !q.is_void())
            else {
                return Err(Error::Invalid(format!(
                    "no such question: {}",
                    answer.question_id
                )));
            };

            // The schedule row is what the payload was built from, so a missing
            // one means the client is answering something it was never served.
            let mut item = schedule
                .iter()
                .find(|r| r.qid == answer.question_id)
                .cloned()
                .ok_or_else(|| {
                    Error::Invalid(format!(
                        "question {} is not scheduled for review",
                        answer.question_id
                    ))
                })?;

            let graded = grade(question, &item, answer)?;
            let (confidence, confidence_percent) = answer.band(question.format());

            if graded.correct {
                correct_count += 1;
            }
            let awarded = match confidence_percent {
                Some(percent) if graded.answered => {
                    trainer_core::tags::score_bits(percent, graded.correct, question.format())
                }
                _ => 0.0,
            };
            score_bits += awarded;
            max_score_bits += trainer_core::tags::max_score_bits(question.format());

            let shown = permuted_options(question, &item.presentation);
            let key_in_presentation = question
                .answer()
                .and_then(|stored| position_of(&item.presentation, stored.index()))
                .and_then(|i| Choice::ALL.get(i).copied());

            item.record(graded.correct, confidence, &now);

            results.push(ReviewResultDto {
                question_id: question.id.clone(),
                document_id: doc.doc_id.clone(),
                prompt: question.prompt.clone(),
                correct: graded.correct,
                confidence,
                confidence_percent,
                score_bits: awarded,
                options: shown,
                correct_option_id: key_in_presentation,
                selected_option_id: answer.option_id,
                selected_value: graded.answer_text,
                correct_value: question.numeric().map(|n| n.value),
                unit: question.numeric().map(|n| n.unit.clone()),
                explanation: question.explanation.clone(),
                next_due_at: item.due_at.clone(),
                interval_days: item.scheduled_days,
            });

            updated.push(item);
        }
    }

    // Written after every answer is graded, so a failure here loses the
    // schedule advance rather than the reader's session.
    state.store.put_reviews(&updated).await?;

    let total = results.len();
    Ok(ReviewSubmitResponse {
        correct: correct_count,
        total,
        score_bits,
        max_score_bits,
        results,
    })
}

struct Graded {
    answered: bool,
    correct: bool,
    answer_text: Option<String>,
}

/// The same two grading rules as `docs::submit`, against a permuted screen.
fn grade(question: &Question, item: &ReviewItem, answer: &SubmittedReview) -> Result<Graded> {
    match &question.body {
        QuestionBody::MultipleChoice { answer: key, .. } => {
            let Some(picked) = answer.option_id else {
                return Ok(Graded {
                    answered: false,
                    correct: false,
                    answer_text: None,
                });
            };
            let stored_index = resolve(question, &item.presentation, picked);
            Ok(Graded {
                answered: true,
                correct: stored_index == Some(key.index()),
                answer_text: None,
            })
        }
        QuestionBody::Numeric { numeric: spec } => {
            let typed = answer
                .value
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty());
            Ok(Graded {
                answered: typed.is_some(),
                correct: typed
                    .and_then(numeric::parse_reader_value)
                    .is_some_and(|v| spec.accepts(v)),
                answer_text: typed.map(str::to_string),
            })
        }
    }
}

/// Where `stored_index` ended up in the presentation order.
fn position_of(presentation: &[usize], stored_index: usize) -> Option<usize> {
    if presentation.is_empty() {
        return Some(stored_index);
    }
    presentation.iter().position(|&i| i == stored_index)
}

#[cfg(test)]
mod tests {
    use trainer_core::numeric::NumericAnswer;

    use super::*;

    fn question() -> Question {
        Question {
            id: "c1".into(),
            skill: Skill::Causal,
            shelf: trainer_core::tags::Shelf::Slow,
            prompt: "Why did the forecast change?".into(),
            explanation: "Section 2.".into(),
            void: None,
            body: QuestionBody::MultipleChoice {
                options: vec![
                    "stored-a".into(),
                    "stored-b".into(),
                    "stored-c".into(),
                    "stored-d".into(),
                ],
                answer: Choice::B,
            },
        }
    }

    fn item_with(presentation: Vec<usize>) -> ReviewItem {
        let mut item = ReviewItem::new(
            "doc-1",
            "c1",
            "Doc",
            4,
            trainer_core::tags::Shelf::Slow,
            &clock::now_iso8601(),
        );
        item.presentation = presentation;
        item
    }

    fn picked(option_id: Option<Choice>) -> SubmittedReview {
        SubmittedReview {
            document_id: "doc-1".into(),
            question_id: "c1".into(),
            option_id,
            value: None,
            confidence: Some(Confidence::FairlySure),
            confidence_percent: Some(65),
        }
    }

    /// **The property the permutation rests on.** The reader picks a letter off
    /// a re-ordered screen; the grader must map it back to the stored option
    /// before comparing, or every review is graded against the wrong thing.
    #[test]
    fn a_letter_is_graded_against_the_option_it_was_shown_beside() {
        // Reversed: screen position 0 shows stored option 3, and so on. The
        // stored key is `b` (index 1), which now appears at screen position 2 —
        // the letter `c`.
        let item = item_with(vec![3, 2, 1, 0]);
        let q = question();

        let shown = permuted_options(&q, &item.presentation);
        assert_eq!(shown[2].text, "stored-b", "fixture assumption");

        assert!(
            grade(&q, &item, &picked(Some(Choice::C)))
                .expect("grades")
                .correct,
            "the letter the key was shown under must be correct"
        );
        assert!(
            !grade(&q, &item, &picked(Some(Choice::B)))
                .expect("grades")
                .correct,
            "the stored letter must NOT be correct once the screen moved"
        );
    }

    /// The results screen reveals the key, and must reveal it in the same space
    /// the reader was answering in.
    #[test]
    fn the_revealed_key_is_a_screen_letter_not_a_stored_one() {
        let item = item_with(vec![3, 2, 1, 0]);
        let q = question();
        let stored_key = q.answer().expect("multiple choice").index();

        let revealed = position_of(&item.presentation, stored_key)
            .and_then(|i| Choice::ALL.get(i).copied())
            .expect("the key is somewhere on screen");

        assert_eq!(revealed, Choice::C);
        let shown = permuted_options(&q, &item.presentation);
        assert_eq!(shown[revealed.index()].text, "stored-b");
    }

    /// The identity permutation must behave exactly as the original quiz does,
    /// or a first review would disagree with the sitting that created it.
    #[test]
    fn an_unpermuted_question_grades_like_the_original_quiz() {
        let item = item_with(vec![0, 1, 2, 3]);
        let q = question();
        assert!(
            grade(&q, &item, &picked(Some(Choice::B)))
                .expect("grades")
                .correct
        );
        assert!(
            !grade(&q, &item, &picked(Some(Choice::A)))
                .expect("grades")
                .correct
        );
    }

    /// A regenerated document can leave a schedule row whose presentation no
    /// longer fits the question. Falling back to stored order is what keeps
    /// `permuted_options` and `resolve` agreeing in that case — if they
    /// disagreed, the reader would be marked wrong for a right answer.
    #[test]
    fn a_stale_presentation_falls_back_consistently() {
        let item = item_with(vec![0, 1]);
        let q = question();

        let shown = permuted_options(&q, &item.presentation);
        assert_eq!(shown.len(), 4, "all four options are still offered");
        assert_eq!(shown[1].text, "stored-b");
        assert!(
            grade(&q, &item, &picked(Some(Choice::B)))
                .expect("grades")
                .correct
        );
    }

    #[test]
    fn an_unanswered_review_is_wrong_and_scores_nothing() {
        let item = item_with(vec![0, 1, 2, 3]);
        let graded = grade(&question(), &item, &picked(None)).expect("grades");
        assert!(!graded.answered);
        assert!(!graded.correct);
    }

    #[test]
    fn a_typed_review_is_graded_against_its_tolerance() {
        let q = Question {
            id: "n1".into(),
            skill: Skill::FigureRecall,
            shelf: trainer_core::tags::Shelf::Dated,
            prompt: "How much?".into(),
            explanation: "Table 1.".into(),
            void: None,
            body: QuestionBody::Numeric {
                numeric: NumericAnswer {
                    value: -4.0,
                    tolerance: 1.0,
                    unit: "%".into(),
                },
            },
        };
        let item = ReviewItem::new(
            "doc-1",
            "n1",
            "Doc",
            0,
            trainer_core::tags::Shelf::Dated,
            &clock::now_iso8601(),
        );

        let mut answer = picked(None);
        answer.question_id = "n1".into();
        answer.value = Some("-3.5".into());
        assert!(grade(&q, &item, &answer).expect("grades").correct);

        answer.value = Some("2".into());
        assert!(!grade(&q, &item, &answer).expect("grades").correct);
    }
}
