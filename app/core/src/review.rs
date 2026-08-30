//! Spaced review: what to see again, and when.
//!
//! # Why this exists
//!
//! A quiz taken once measures a reading. It does not produce retention, and the
//! gap between those two is most of the point. The strongest result in this
//! literature is *successive relearning* — retrieving the same material
//! correctly across several spaced sessions rather than several times in one —
//! which took cued recall of course concepts from about 11% at a one-month
//! delay to around 68%, and about 49% at four months.
//!
//! Note what that study repeated: **the same items**. Rephrasing each
//! repetition transfers better to new phrasings, but the durable-retention
//! result is built on replaying the item you already have. That is convenient
//! here for a reason worth stating plainly — replay needs no model call, so a
//! review is graded by exactly the arithmetic that graded the original sitting,
//! and a schedule accumulated over months is not quietly re-based every time
//! the model changes.
//!
//! # The one thing that would make this measure nothing
//!
//! A multiple-choice question whose answer stays at `c` stops testing the fact
//! after the second sitting. You recall the letter, or the shape of the right
//! option, and the scheduler reads that as knowledge and stretches the
//! interval. Every repetition after that is evidence about nothing.
//!
//! So the options are re-permuted on every repetition and the permutation is
//! stored, because the grader has to agree with what was on screen. See
//! [`ReviewItem::presentation`].
//!
//! Typed-figure questions are immune to all of this and are the better review
//! items for exactly that reason: there is nothing to recognise.

use serde::{Deserialize, Serialize};

use crate::clock;
use crate::fsrs::{self, Memory, Rating};
use crate::shuffle;
use crate::tags::{Confidence, Shelf};

/// One question's schedule. `REVIEW` / `<doc_id>#<qid>`.
///
/// # Why the FSRS state is stored field by field
///
/// These seven numbers *are* the algorithm's state — see [`crate::fsrs`], which
/// takes and returns exactly them. Keeping them as columns on this row rather
/// than as a nested blob means the schedule is readable in the console, and a
/// future change to the scheduler is a change to fields with names rather than
/// to an opaque struct nobody can inspect.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewItem {
    pub pk: String,
    pub sk: String,

    pub doc_id: String,
    pub qid: String,
    /// Denormalized so the review queue can name the document a question came
    /// from without a lookup per item, exactly as `Attempt` carries it.
    pub doc_title: String,

    /// RFC 3339, UTC. Mirrored out of the FSRS state so the handler can select
    /// on it without reconstructing a card per row.
    pub due_at: String,

    /// How long this question stays worth asking, snapshotted from the question
    /// when the schedule was created.
    ///
    /// A snapshot for the same reason `skill` and `topics` are snapshotted onto
    /// an attempt: regenerating the document must not silently retire — or
    /// un-retire — a schedule that has been accumulating for a year.
    #[serde(default)]
    pub shelf: Shelf,
    /// When the document this came from was read.
    ///
    /// The clock retirement runs against. Deliberately the *document's* date and
    /// not the schedule's: a question added by a second sitting six months later
    /// is no fresher than the report it came from.
    ///
    /// Empty on rows written before shelf life existed, which read as never
    /// retiring — the same forgiving direction as `Shelf::default`.
    #[serde(default)]
    pub source_dated_at: String,
    #[serde(default)]
    pub last_reviewed_at: Option<String>,

    /// FSRS state. Days-to-90%-recall, and how hard this item has proved.
    pub stability: f64,
    pub difficulty: f64,
    /// Mirrors the scheduler's own idea of the interval it last set, which it
    /// needs back on the next call.
    #[serde(default)]
    pub elapsed_days: i64,
    #[serde(default)]
    pub scheduled_days: i64,
    #[serde(default)]
    pub reps: i32,
    #[serde(default)]
    pub lapses: i32,
    /// 0 new, 2 review. See `fsrs::STATE_NEW`.
    #[serde(default)]
    pub state: u8,

    /// The order the four options are shown in *next* time, as indices into the
    /// question's stored `options`.
    ///
    /// **Stored rather than derived, because the grader has to agree with the
    /// screen.** A permutation recomputed at grade time from a counter would be
    /// right until anything advanced that counter in between, and the failure
    /// would be a correct answer marked wrong with no way to tell from the row.
    ///
    /// Empty for a typed-figure question, which has no options to permute.
    #[serde(default)]
    pub presentation: Vec<usize>,

    /// The last few outcomes, newest last.
    ///
    /// Capped, and kept for a reason the scheduler does not care about: a
    /// review's confidence is the only calibration data this app collects
    /// outside a first sitting, and throwing it away would mean the reliability
    /// table permanently describes first readings only. Nothing reads this yet.
    #[serde(default)]
    pub outcomes: Vec<ReviewOutcome>,
}

/// What happened on one repetition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewOutcome {
    pub at: String,
    pub correct: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<Confidence>,
    /// The grade handed to the scheduler, as its own 1–4 encoding. Stored so a
    /// schedule can be explained after the fact without re-deriving it from a
    /// mapping that may since have changed.
    pub rating: u8,
}

/// How many repetitions of outcome history to keep on a row.
const MAX_OUTCOMES: usize = 10;

/// Translate a graded answer into the scheduler's four-point grade.
///
/// # This is better input than a flashcard app gets
///
/// Anki asks the reviewer how hard the card felt, *after* showing them the
/// answer. That is a self-report, made once the answer is known, with nothing
/// riding on it. Here both halves are already available and neither is a
/// self-report about difficulty: whether the answer was right is objective, and
/// how sure the reader was is a claim they made *before* seeing the answer,
/// under a scoring rule that pays for honesty.
///
/// So the mapping is not a convenience — it is the reason the confidence bands
/// and the scheduler belong in the same app:
///
/// | outcome              | grade | reading                                |
/// |----------------------|-------|----------------------------------------|
/// | wrong                | Again | forgotten, whatever they thought       |
/// | right, guessing      | Hard  | retrieved, but not held                |
/// | right, fairly sure   | Good  | held                                   |
/// | right, certain       | Easy  | held firmly; stretch the interval      |
///
/// A wrong answer is `Again` regardless of confidence. The scheduler has one
/// failure grade, and the extra information in a *confident* error is not
/// scheduling information — it is the "sure, and wrong" list, which surfaces
/// separately because that is where it is useful.
///
/// An answer with no confidence — a stale client — grades `Good`. Not `Hard`:
/// treating an unstated confidence as a weak one would shorten intervals on
/// evidence nobody gave.
pub fn rating_for(correct: bool, confidence: Option<Confidence>) -> Rating {
    if !correct {
        return Rating::Again;
    }
    match confidence {
        Some(Confidence::Guessing) => Rating::Hard,
        Some(Confidence::Certain) => Rating::Easy,
        Some(Confidence::FairlySure) | None => Rating::Good,
    }
}

impl ReviewItem {
    /// A question's first schedule, from its first sitting.
    ///
    /// The first attempt at a document *is* the first repetition — there is no
    /// separate "learn" step, because reading the document was it.
    pub fn new(
        doc_id: &str,
        qid: &str,
        doc_title: &str,
        option_count: usize,
        shelf: Shelf,
        source_dated_at: &str,
    ) -> Self {
        Self {
            pk: crate::keys::REVIEW_PK.to_string(),
            sk: crate::keys::review_sk(doc_id, qid),
            doc_id: doc_id.to_string(),
            qid: qid.to_string(),
            doc_title: doc_title.to_string(),
            shelf,
            source_dated_at: source_dated_at.to_string(),
            due_at: clock::now_iso8601(),
            last_reviewed_at: None,
            stability: 0.0,
            difficulty: 0.0,
            elapsed_days: 0,
            scheduled_days: 0,
            reps: 0,
            lapses: 0,
            state: 0,
            presentation: (0..option_count).collect(),
            outcomes: Vec::new(),
        }
    }

    /// Is this item due at `now`?
    ///
    /// String comparison, which is total on RFC 3339 in UTC — the same property
    /// `attempt_sk` relies on. A row whose `due_at` is unparseable garbage
    /// compares as due, which is the safe direction: it surfaces rather than
    /// disappearing.
    pub fn is_due(&self, now: &str) -> bool {
        self.due_at.as_str() <= now && !self.is_retired(now)
    }

    /// Has this question aged out of being worth asking?
    ///
    /// **Retired, not deleted.** The row stays, so the count is reportable and
    /// a question can come back if the document is re-read or the judgement is
    /// revised. Silently dropping rows would make the queue shrink for reasons
    /// nothing on screen explains.
    ///
    /// A row with no source date — written before shelf life existed — never
    /// retires. That is the forgiving direction, and it matches
    /// `Shelf::default`: the cost of keeping a stale question is a few wasted
    /// reviews, and the cost of wrongly retiring a good one is that it quietly
    /// stops existing.
    pub fn is_retired(&self, now: &str) -> bool {
        let Some(horizon) = self.shelf.horizon_days() else {
            return false;
        };
        let (Some(source), Some(now_unix)) = (
            clock::unix_from_iso8601(&self.source_dated_at),
            clock::unix_from_iso8601(now),
        ) else {
            return false;
        };

        now_unix - source > horizon * fsrs::SECONDS_PER_DAY
    }

    /// Advance the schedule by one repetition.
    ///
    /// The only place timestamps become numbers and back. [`crate::fsrs`] works
    /// in Unix seconds and knows nothing about how they are stored; this row
    /// stores RFC 3339 strings because everything else in the table does, and
    /// because a due date you can read in the console is worth the conversion.
    pub fn record(&mut self, correct: bool, confidence: Option<Confidence>, now: &str) {
        let rating = rating_for(correct, confidence);
        let at = clock::unix_from_iso8601(now).unwrap_or_else(clock::unix_now);

        let scheduled = fsrs::next(&self.memory(at), rating, at);

        self.stability = scheduled.stability;
        self.difficulty = scheduled.difficulty;
        self.elapsed_days = scheduled.elapsed_days;
        self.scheduled_days = scheduled.scheduled_days;
        self.reps = scheduled.reps;
        self.lapses = scheduled.lapses;
        self.state = scheduled.state;
        self.due_at = clock::iso_at(scheduled.due_unix);
        self.last_reviewed_at = Some(now.to_string());

        // Re-permuted *after* grading, so the order this repetition was graded
        // against is the one that was on screen, and the next one differs. The
        // seed includes `reps`, which has just advanced.
        if !self.presentation.is_empty() {
            self.presentation = shuffle::permutation(
                self.presentation.len(),
                &format!("review:{}:{}:{}", self.doc_id, self.qid, self.reps),
            );
        }

        self.outcomes.push(ReviewOutcome {
            at: now.to_string(),
            correct,
            confidence,
            rating: rating as u8,
        });
        if self.outcomes.len() > MAX_OUTCOMES {
            let excess = self.outcomes.len() - MAX_OUTCOMES;
            self.outcomes.drain(0..excess);
        }
    }

    /// This row's stored fields, as the scheduler's state.
    ///
    /// `last_reviewed_at` absent means nothing has been recorded, which is the
    /// same thing `state == STATE_NEW` says — the fallback exists only so a row
    /// with an unreadable timestamp behaves like a fresh one rather than
    /// computing a decades-long elapsed time from a zero.
    fn memory(&self, now_unix: i64) -> Memory {
        Memory {
            stability: self.stability,
            difficulty: self.difficulty,
            elapsed_days: self.elapsed_days,
            scheduled_days: self.scheduled_days,
            reps: self.reps,
            lapses: self.lapses,
            state: self.state,
            last_review_unix: self
                .last_reviewed_at
                .as_deref()
                .and_then(clock::unix_from_iso8601)
                .unwrap_or(now_unix),
            due_unix: clock::unix_from_iso8601(&self.due_at).unwrap_or(now_unix),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item() -> ReviewItem {
        ReviewItem::new(
            "doc-1",
            "c1",
            "A Reference Document",
            4,
            Shelf::Slow,
            &clock::now_iso8601(),
        )
    }

    #[test]
    fn a_new_item_is_due_immediately() {
        assert!(item().is_due(&clock::now_iso8601()));
    }

    /// The mapping that makes the confidence bands do double duty.
    #[test]
    fn confidence_and_correctness_become_a_grade() {
        assert_eq!(rating_for(true, Some(Confidence::Certain)), Rating::Easy);
        assert_eq!(rating_for(true, Some(Confidence::FairlySure)), Rating::Good);
        assert_eq!(rating_for(true, Some(Confidence::Guessing)), Rating::Hard);
        assert_eq!(rating_for(true, None), Rating::Good);

        // Wrong is wrong. The extra information in a confident error is not
        // scheduling information.
        for band in [
            Some(Confidence::Certain),
            Some(Confidence::FairlySure),
            Some(Confidence::Guessing),
            None,
        ] {
            assert_eq!(rating_for(false, band), Rating::Again);
        }
    }

    /// **The property the whole schedule rests on.** If a correct answer did
    /// not push the item further out, every review would come back tomorrow
    /// forever and the feature would be a to-do list.
    #[test]
    fn a_correct_answer_schedules_further_out_than_a_wrong_one() {
        let now = clock::now_iso8601();

        let mut right = item();
        right.record(true, Some(Confidence::FairlySure), &now);

        let mut wrong = item();
        wrong.record(false, Some(Confidence::FairlySure), &now);

        assert!(
            right.due_at > wrong.due_at,
            "correct {} should be later than wrong {}",
            right.due_at,
            wrong.due_at
        );
        assert!(right.due_at > now, "a correct answer must not stay due");
    }

    /// Confidence has to move the interval, or recording it changes nothing and
    /// the mapping above is decorative.
    #[test]
    fn being_certain_stretches_the_interval_further_than_guessing() {
        let now = clock::now_iso8601();

        let mut certain = item();
        certain.record(true, Some(Confidence::Certain), &now);

        let mut guessed = item();
        guessed.record(true, Some(Confidence::Guessing), &now);

        assert!(
            certain.due_at > guessed.due_at,
            "certain {} vs guessing {}",
            certain.due_at,
            guessed.due_at
        );
    }

    /// Intervals must actually grow across repetitions, which is the difference
    /// between spaced practice and a daily chore.
    #[test]
    fn repeated_success_lengthens_the_interval() {
        let mut it = item();
        let mut at = clock::now_iso8601();
        let mut previous = 0i64;

        for repetition in 0..4 {
            it.record(true, Some(Confidence::FairlySure), &at);
            let gap = it.scheduled_days;
            assert!(
                gap >= previous,
                "repetition {repetition} scheduled {gap} days, previous was {previous}"
            );
            previous = gap;
            // Answer it again on the day it comes due.
            at = it.due_at.clone();
        }

        assert!(
            previous > 1,
            "four correct repetitions never got past a day"
        );
    }

    /// The bug that would make review measure nothing: the same letter every
    /// time.
    #[test]
    fn the_option_order_changes_between_repetitions() {
        let mut it = item();
        let mut seen = vec![it.presentation.clone()];
        let mut at = clock::now_iso8601();

        for _ in 0..5 {
            it.record(true, Some(Confidence::FairlySure), &at);
            seen.push(it.presentation.clone());
            at = it.due_at.clone();
        }

        let distinct: std::collections::HashSet<&Vec<usize>> = seen.iter().collect();
        assert!(
            distinct.len() > 2,
            "six repetitions produced {} distinct orders: {seen:?}",
            distinct.len()
        );
        for order in &seen {
            let mut sorted = order.clone();
            sorted.sort_unstable();
            assert_eq!(sorted, vec![0, 1, 2, 3], "an order lost an option");
        }
    }

    /// A typed figure has nothing to permute, and asking for a permutation of
    /// nothing must not produce one.
    #[test]
    fn a_typed_figure_carries_no_presentation() {
        let mut it = ReviewItem::new(
            "doc-1",
            "n1",
            "A Reference Document",
            0,
            Shelf::Dated,
            &clock::now_iso8601(),
        );
        assert!(it.presentation.is_empty());
        it.record(true, Some(Confidence::Certain), &clock::now_iso8601());
        assert!(it.presentation.is_empty());
    }

    #[test]
    fn outcome_history_is_capped() {
        let mut it = item();
        let mut at = clock::now_iso8601();
        for _ in 0..(MAX_OUTCOMES + 5) {
            it.record(true, Some(Confidence::FairlySure), &at);
            at = it.due_at.clone();
        }
        assert_eq!(it.outcomes.len(), MAX_OUTCOMES);
    }

    /// Rows go to DynamoDB, and the floats are the schedule. A number that does
    /// not survive the round trip is a schedule that silently resets.
    #[test]
    fn review_items_round_trip_through_dynamodb() {
        use aws_sdk_dynamodb::types::AttributeValue;

        let mut original = item();
        original.record(true, Some(Confidence::Certain), &clock::now_iso8601());

        let stored: AttributeValue =
            serde_dynamo::to_attribute_value(&original).expect("serializes");
        let back: ReviewItem = serde_dynamo::from_attribute_value(stored).expect("reads back");

        assert_eq!(back.due_at, original.due_at);
        assert_eq!(back.stability, original.stability);
        assert_eq!(back.difficulty, original.difficulty);
        assert_eq!(back.reps, original.reps);
        assert_eq!(back.presentation, original.presentation);
        assert_eq!(back.outcomes.len(), 1);
    }
}

#[cfg(test)]
mod shelf_tests {
    use super::*;

    fn aged(shelf: Shelf, days_old: i64) -> ReviewItem {
        let source = clock::iso_at(clock::unix_now() - days_old * fsrs::SECONDS_PER_DAY);
        ReviewItem::new("doc-1", "c1", "Doc", 4, shelf, &source)
    }

    /// The case that prompted all of this: a quarterly forecast, still perfectly
    /// answerable three years on, and no longer worth drilling.
    #[test]
    fn a_dated_question_retires_and_a_perennial_one_does_not() {
        let now = clock::now_iso8601();

        assert!(!aged(Shelf::Dated, 300).is_retired(&now), "still current");
        assert!(
            aged(Shelf::Dated, 1000).is_retired(&now),
            "a 2026 forecast in 2029"
        );

        assert!(!aged(Shelf::Slow, 1000).is_retired(&now));
        assert!(aged(Shelf::Slow, 3000).is_retired(&now));

        assert!(!aged(Shelf::Perennial, 100_000).is_retired(&now));
    }

    /// Retirement has to take the question out of the queue, not merely label
    /// it, or nothing changes.
    #[test]
    fn a_retired_question_is_never_due() {
        let now = clock::now_iso8601();
        let mut item = aged(Shelf::Dated, 1000);
        item.due_at = clock::iso_at(clock::unix_now() - 10 * fsrs::SECONDS_PER_DAY);

        assert!(item.due_at.as_str() <= now.as_str(), "overdue by the clock");
        assert!(!item.is_due(&now), "but retired, so not offered");
    }

    /// Rows written before shelf life existed must not vanish.
    #[test]
    fn a_row_with_no_source_date_never_retires() {
        let mut item = aged(Shelf::Dated, 100_000);
        item.source_dated_at = String::new();
        assert!(!item.is_retired(&clock::now_iso8601()));
    }
}
