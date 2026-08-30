//! FSRS, the long-term scheduler, implemented here.
//!
//! # What this is
//!
//! The Free Spaced Repetition Scheduler models memory with three numbers:
//!
//!   - **stability** — days until the probability of recall falls to 90%;
//!   - **difficulty** — how hard this particular item has proved, 1 to 10;
//!   - **retrievability** — the probability of recall *right now*, which falls
//!     along a power curve as time passes since the last review.
//!
//! A review updates the first two from the third and the grade, and the next
//! interval is read straight off the new stability. The 19 weights below are not
//! chosen; they are the published defaults, fitted on hundreds of millions of
//! real reviews.
//!
//! # Why it is written out rather than imported
//!
//! It was imported first. `rs-fsrs` is a correct implementation and using it was
//! the right call at the time, for a reason worth keeping in mind: the two most
//! detailed write-ups of FSRS on the open web give *different* formulas, and one
//! of them silently describes a different version of the algorithm. A scheduler
//! transcribed from those would look like it worked. It would produce plausible
//! intervals, and nothing would ever tell you they were wrong.
//!
//! What made writing it out safe was having the reference to check against.
//! Every function here was verified to agree with `rs-fsrs` exactly — bit for
//! bit on the floats — over thousands of randomly generated review sequences,
//! and the dependency was removed only after that passed. The equivalence is
//! pinned by [`tests::golden_sequences_match_the_reference_implementation`],
//! whose expected values were captured from that comparison, so it still holds
//! without anything to compare against.
//!
//! # What is deliberately not here
//!
//! **Short-term (learning) steps.** They exist for Anki's within-session
//! repetitions — the ten-minute and one-day steps of a study session. This app
//! has no study session: a document is read once and its questions come back on
//! day-scale intervals, so short-term steps would schedule a re-quiz minutes
//! after the first sitting, which is massed practice wearing the clothes of
//! spaced practice.
//!
//! **Fuzz.** Randomised jitter exists to stop a large deck synchronising into
//! daily spikes. With one reader and a few hundred items there is nothing to
//! desynchronise, and determinism is worth more: it makes a schedule
//! reproducible from its outcomes, which is what makes a bug in here findable.
//!
//! **The optimiser.** Fitting the weights to one person's history needs
//! thousands of reviews. The defaults are what everybody starts on, and until
//! there is a corpus worth fitting, "your own parameters" would be overfitting
//! with extra steps.

/// The published FSRS-4.5 defaults, fitted on hundreds of millions of reviews.
///
/// Indices are load-bearing and each is used in exactly one place below; the
/// formulas name them `w[n]` to stay legible against the published algorithm
/// rather than inventing names it does not use.
const W: [f64; 19] = [
    0.4072, 1.1829, 3.1262, 15.4722, 7.2102, 0.5316, 1.0651, 0.0234, 1.616, 0.1544, 1.0824, 1.9813,
    0.0953, 0.2975, 2.2042, 0.2407, 2.9466, 0.5034, 0.6567,
];

/// Exponent of the forgetting curve.
const DECAY: f64 = -0.5;

/// Chosen so that `retrievability(S, S) == 0.9` — which is what makes
/// "stability" mean "days until 90%" rather than an arbitrary scale.
///
/// `(9/10)^(1/DECAY) - 1`, written as the exact rational it works out to.
const FACTOR: f64 = 19.0 / 81.0;

pub const SECONDS_PER_DAY: i64 = 86_400;

/// How sure we are of recall at the moment an item is scheduled to return.
///
/// Lower means fewer reviews and more forgetting; higher means the reverse.
/// 0.9 is the standard starting point and the right one here, where the cost of
/// forgetting is a fact you would have used in an argument.
pub const REQUEST_RETENTION: f64 = 0.9;

/// Ten years. Effectively "never again" for this corpus; the cap exists so a run
/// of easy repetitions cannot schedule something past the heat death of the
/// reader.
pub const MAXIMUM_INTERVAL_DAYS: f64 = 36500.0;

/// The four-point grade FSRS takes.
///
/// Derived here from an objective outcome and a confidence stated before the
/// answer was revealed, rather than self-reported after — see
/// `crate::review::rating_for`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rating {
    Again = 1,
    Hard = 2,
    Good = 3,
    Easy = 4,
}

impl Rating {
    const fn as_f64(self) -> f64 {
        self as i32 as f64
    }

    /// Every grade, in order. The interval constraint below needs all four
    /// computed even when only one is being applied.
    const ALL: [Rating; 4] = [Rating::Again, Rating::Hard, Rating::Good, Rating::Easy];
}

/// An item's memory state, and when it is next due.
///
/// `state` is the scheduler's own encoding — 0 new, 1 learning, 2 review,
/// 3 relearning. Only "new or not" is consulted: the long-term scheduler treats
/// learning and relearning exactly as review, and always produces `2`. The
/// wider encoding is kept because it is what is already stored.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Memory {
    pub stability: f64,
    pub difficulty: f64,
    pub elapsed_days: i64,
    pub scheduled_days: i64,
    pub reps: i32,
    pub lapses: i32,
    pub state: u8,
    /// Unix seconds. Zero on an item that has never been reviewed.
    pub last_review_unix: i64,
    /// Unix seconds.
    pub due_unix: i64,
}

impl Memory {
    /// An item nothing has been recorded against yet.
    pub const fn new(now_unix: i64) -> Self {
        Self {
            stability: 0.0,
            difficulty: 0.0,
            elapsed_days: 0,
            scheduled_days: 0,
            reps: 0,
            lapses: 0,
            state: STATE_NEW,
            last_review_unix: now_unix,
            due_unix: now_unix,
        }
    }

    /// Probability of recalling this right now.
    ///
    /// Zero for an item never reviewed, which is what makes the first grade use
    /// the initial-stability table rather than the update formulas.
    fn retrievability(&self, now_unix: i64) -> f64 {
        if self.state == STATE_NEW {
            return 0.0;
        }
        let elapsed = days_between(self.last_review_unix, now_unix) as f64;
        forgetting_curve(elapsed, self.stability)
    }
}

pub const STATE_NEW: u8 = 0;
pub const STATE_REVIEW: u8 = 2;

/// `(1 + FACTOR · t/S) ^ DECAY`.
pub fn forgetting_curve(elapsed_days: f64, stability: f64) -> f64 {
    (1.0 + FACTOR * elapsed_days / stability).powf(DECAY)
}

/// Whole days from `from` to `to`, truncated toward zero.
///
/// Truncating rather than rounding, and toward zero rather than down, because
/// that is what the reference does — a review taken twenty-three hours early is
/// zero days elapsed, not one.
fn days_between(from_unix: i64, to_unix: i64) -> i64 {
    (to_unix - from_unix) / SECONDS_PER_DAY
}

/// Record one review and return the new state.
///
/// This is the whole scheduler. The shape is worth reading once because it is
/// not obvious: **all four grades are computed even though one was given.**
/// That is not waste, it is the algorithm — the four candidate intervals are
/// then forced into strict order (`again < hard < good < easy`), so the interval
/// you get for "good" depends on what "hard" would have been. Computing only the
/// grade that was given produces intervals that are subtly too short, and
/// nothing about the output would look wrong.
pub fn next(memory: &Memory, rating: Rating, now_unix: i64) -> Memory {
    let elapsed_days = if memory.state == STATE_NEW {
        0
    } else {
        days_between(memory.last_review_unix, now_unix)
    };

    let first_review = memory.state == STATE_NEW;
    let retrievability = memory.retrievability(now_unix);

    // Stability and difficulty for each of the four grades.
    let mut candidates: [(f64, f64); 4] = [(0.0, 0.0); 4];
    for (slot, rating) in Rating::ALL.iter().enumerate() {
        candidates[slot] = if first_review {
            (init_stability(*rating), init_difficulty(*rating))
        } else {
            let difficulty = next_difficulty(memory.difficulty, *rating);
            let stability = match rating {
                Rating::Again => {
                    next_forget_stability(memory.difficulty, memory.stability, retrievability)
                }
                _ => next_recall_stability(
                    memory.difficulty,
                    memory.stability,
                    retrievability,
                    *rating,
                ),
            };
            (stability, difficulty)
        };
    }

    // The ordering constraint. Each interval is pushed at least a day past the
    // one below it, so a harder grade can never schedule further out than an
    // easier one.
    let mut intervals = [0.0f64; 4];
    for (slot, (stability, _)) in candidates.iter().enumerate() {
        intervals[slot] = next_interval(*stability);
    }
    intervals[0] = intervals[0].min(intervals[1]);
    intervals[1] = intervals[1].max(intervals[0] + 1.0);
    intervals[2] = intervals[2].max(intervals[1] + 1.0);
    intervals[3] = intervals[3].max(intervals[2] + 1.0);

    let slot = rating as usize - 1;
    let (stability, difficulty) = candidates[slot];
    let scheduled_days = intervals[slot] as i64;

    Memory {
        stability,
        difficulty,
        elapsed_days,
        scheduled_days,
        reps: memory.reps + 1,
        lapses: memory.lapses + i32::from(!first_review && rating == Rating::Again),
        state: STATE_REVIEW,
        last_review_unix: now_unix,
        due_unix: now_unix + scheduled_days * SECONDS_PER_DAY,
    }
}

/// Days until this stability decays to the requested retention, rounded.
fn next_interval(stability: f64) -> f64 {
    (stability / FACTOR * (REQUEST_RETENTION.powf(1.0 / DECAY) - 1.0))
        .round()
        .clamp(1.0, MAXIMUM_INTERVAL_DAYS)
}

/// Stability of an item being graded for the first time: straight off the
/// weight table, one entry per grade.
fn init_stability(rating: Rating) -> f64 {
    W[rating as usize - 1].max(0.1)
}

/// Difficulty of an item being graded for the first time.
fn init_difficulty(rating: Rating) -> f64 {
    (W[4] - (W[5] * (rating.as_f64() - 1.0)).exp() + 1.0).clamp(1.0, 10.0)
}

/// Difficulty after a review: a step proportional to how far the grade is from
/// "good", pulled back toward the difficulty an easy first review would have
/// given.
///
/// That pull-back is *mean reversion*, and it is why difficulty does not ratchet
/// to 10 and stay there for an item you once got wrong.
fn next_difficulty(difficulty: f64, rating: Rating) -> f64 {
    let stepped = (-W[6]).mul_add(rating.as_f64() - 3.0, difficulty);
    let reverted = W[7].mul_add(init_difficulty(Rating::Easy), (1.0 - W[7]) * stepped);
    reverted.clamp(1.0, 10.0)
}

/// Stability after remembering.
///
/// The gain shrinks as difficulty rises, as stability is already large, and as
/// retrievability is already high — which is the formal statement of the reason
/// spacing works at all: **a review you would have passed easily teaches you
/// almost nothing.** Reviewing something you were about to forget is what moves
/// the number.
fn next_recall_stability(
    difficulty: f64,
    stability: f64,
    retrievability: f64,
    rating: Rating,
) -> f64 {
    let modifier = match rating {
        Rating::Hard => W[15],
        Rating::Easy => W[16],
        _ => 1.0,
    };

    stability
        * ((W[8].exp()
            * (11.0 - difficulty)
            * stability.powf(-W[9])
            * ((1.0 - retrievability) * W[10]).exp_m1())
        .mul_add(modifier, 1.0))
}

/// Stability after forgetting. Not a multiple of the old stability — a lapse
/// re-derives it from difficulty and how far the item had already decayed.
fn next_forget_stability(difficulty: f64, stability: f64, retrievability: f64) -> f64 {
    W[11]
        * difficulty.powf(-W[12])
        * ((stability + 1.0).powf(W[13]) - 1.0)
        * ((1.0 - retrievability) * W[14]).exp()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = SECONDS_PER_DAY;
    const T0: i64 = 1_800_000_000;

    fn first(rating: Rating) -> Memory {
        next(&Memory::new(T0), rating, T0)
    }

    /// **Captured from a bit-for-bit comparison against `rs-fsrs` 1.2.1**, run
    /// over thousands of random sequences before that dependency was removed.
    ///
    /// This is the test that makes writing the scheduler out rather than
    /// importing it a defensible thing to have done. Without it, a transcription
    /// error would show up as intervals that are plausible and wrong, which is
    /// the failure mode this whole feature cannot tolerate — a schedule that is
    /// subtly too generous simply stops working, silently, over months.
    ///
    /// Each row is `(grades applied in order, expected scheduled_days after
    /// each)`, reviewing exactly on the day each one comes due.
    #[test]
    fn golden_sequences_match_the_reference_implementation() {
        let cases: &[(&[Rating], &[i64])] = &[
            (&[Rating::Again], &[1]),
            (&[Rating::Hard], &[2]),
            (&[Rating::Good], &[3]),
            (&[Rating::Easy], &[15]),
            (&[Rating::Good, Rating::Good], &[3, 11]),
            (&[Rating::Good, Rating::Good, Rating::Good], &[3, 11, 36]),
            (&[Rating::Easy, Rating::Easy], &[15, 144]),
            (&[Rating::Good, Rating::Again], &[3, 1]),
            (&[Rating::Good, Rating::Again, Rating::Good], &[3, 1, 3]),
            (&[Rating::Good, Rating::Hard], &[3, 5]),
            (
                &[Rating::Easy, Rating::Good, Rating::Again, Rating::Good],
                &[15, 59, 5, 18],
            ),
        ];

        for (grades, expected) in cases {
            let mut memory = Memory::new(T0);
            let mut now = T0;
            let mut got = Vec::new();

            for rating in grades.iter() {
                memory = next(&memory, *rating, now);
                got.push(memory.scheduled_days);
                now = memory.due_unix;
            }

            assert_eq!(&got, expected, "sequence {grades:?}");
        }
    }

    /// The property everything else rests on. If a correct answer did not push
    /// an item further out, the queue would be a to-do list.
    #[test]
    fn better_grades_schedule_further_out() {
        let again = first(Rating::Again).scheduled_days;
        let hard = first(Rating::Hard).scheduled_days;
        let good = first(Rating::Good).scheduled_days;
        let easy = first(Rating::Easy).scheduled_days;

        assert!(
            again < hard && hard < good && good < easy,
            "{again} {hard} {good} {easy}"
        );
    }

    /// The ordering constraint is not decoration — it is applied across all four
    /// candidates, so it must hold at every point in an item's life, not just on
    /// the first review.
    #[test]
    fn the_ordering_holds_after_many_reviews() {
        let mut memory = Memory::new(T0);
        let mut now = T0;

        for _ in 0..6 {
            memory = next(&memory, Rating::Good, now);
            now = memory.due_unix;

            let branches: Vec<i64> = Rating::ALL
                .iter()
                .map(|r| next(&memory, *r, now).scheduled_days)
                .collect();

            assert!(
                branches.windows(2).all(|w| w[0] < w[1]),
                "grades out of order: {branches:?}"
            );
        }
    }

    #[test]
    fn repeated_success_lengthens_the_interval() {
        let mut memory = Memory::new(T0);
        let mut now = T0;
        let mut previous = 0;

        for _ in 0..5 {
            memory = next(&memory, Rating::Good, now);
            assert!(
                memory.scheduled_days > previous,
                "{} did not exceed {previous}",
                memory.scheduled_days
            );
            previous = memory.scheduled_days;
            now = memory.due_unix;
        }

        assert!(
            previous > 30,
            "five correct reviews only reached {previous} days"
        );
    }

    /// A lapse must actually cost something, or forgetting is free and the
    /// schedule drifts away from what is known.
    #[test]
    fn forgetting_pulls_the_interval_back_and_counts_a_lapse() {
        let mut memory = Memory::new(T0);
        let mut now = T0;
        for _ in 0..4 {
            memory = next(&memory, Rating::Good, now);
            now = memory.due_unix;
        }
        let before = memory.scheduled_days;

        let lapsed = next(&memory, Rating::Again, now);
        assert!(
            lapsed.scheduled_days < before,
            "a lapse scheduled {} against {before}",
            lapsed.scheduled_days
        );
        assert_eq!(lapsed.lapses, 1);
        assert!(
            lapsed.difficulty > memory.difficulty,
            "forgetting must make an item harder"
        );
    }

    /// The first grade cannot be a lapse — there was nothing to forget.
    #[test]
    fn a_first_review_never_counts_a_lapse() {
        assert_eq!(first(Rating::Again).lapses, 0);
        assert_eq!(first(Rating::Again).reps, 1);
    }

    /// The formal statement of why spacing works: a review taken while you would
    /// still have remembered easily buys less than one taken later.
    #[test]
    fn reviewing_early_buys_less_than_reviewing_on_time() {
        let established = {
            let mut m = Memory::new(T0);
            let mut now = T0;
            for _ in 0..3 {
                m = next(&m, Rating::Good, now);
                now = m.due_unix;
            }
            m
        };

        let on_time = next(&established, Rating::Good, established.due_unix);
        let early = next(&established, Rating::Good, established.due_unix - 5 * DAY);

        assert!(
            early.stability < on_time.stability,
            "early {} should gain less than on-time {}",
            early.stability,
            on_time.stability
        );
    }

    /// Difficulty is clamped and mean-reverting, so a run of failures cannot
    /// pin an item at 10 forever.
    #[test]
    fn difficulty_stays_in_range_and_recovers() {
        let mut memory = Memory::new(T0);
        let mut now = T0;

        for _ in 0..10 {
            memory = next(&memory, Rating::Again, now);
            now = memory.due_unix;
            assert!((1.0..=10.0).contains(&memory.difficulty), "{memory:?}");
        }
        let hardest = memory.difficulty;

        for _ in 0..10 {
            memory = next(&memory, Rating::Easy, now);
            now = memory.due_unix;
            assert!((1.0..=10.0).contains(&memory.difficulty), "{memory:?}");
        }
        assert!(
            memory.difficulty < hardest,
            "difficulty never recovered from {hardest}"
        );
    }

    /// Intervals stay bounded — but note the bound is the cap *plus three*, and
    /// that is not sloppiness.
    ///
    /// The ordering constraint is applied after each candidate is clamped, so a
    /// run of easy grades that pins every candidate at the ceiling still gets
    /// pushed one day apart: again, hard+1, good+2, easy+3. The reference does
    /// the same thing, which the equivalence run confirmed. Asserting the naive
    /// bound here is what surfaced it.
    #[test]
    fn intervals_stay_bounded_across_a_long_run() {
        let mut memory = Memory::new(T0);
        let mut now = T0;
        let ceiling = MAXIMUM_INTERVAL_DAYS as i64 + 3;

        for _ in 0..60 {
            memory = next(&memory, Rating::Easy, now);
            now = memory.due_unix;
            assert!(memory.scheduled_days >= 1);
            assert!(
                memory.scheduled_days <= ceiling,
                "scheduled {} days, past the {ceiling} ceiling",
                memory.scheduled_days
            );
            assert!(memory.stability.is_finite());
            assert!((1.0..=10.0).contains(&memory.difficulty));
        }
    }

    /// Stability is defined as "days until recall falls to 90%", and the factor
    /// exists to make that true. If it drifts, every interval is wrong by a
    /// constant nobody would notice.
    #[test]
    fn the_forgetting_curve_hits_ninety_percent_at_one_stability() {
        for stability in [1.0, 7.5, 100.0] {
            let r = forgetting_curve(stability, stability);
            assert!((r - 0.9).abs() < 1e-9, "R({stability}) = {r}");
        }
        assert!((forgetting_curve(0.0, 10.0) - 1.0).abs() < 1e-12);
    }

    /// A review taken before the due date is zero days elapsed, not one.
    #[test]
    fn elapsed_days_truncate_toward_zero() {
        assert_eq!(days_between(T0, T0 + DAY - 1), 0);
        assert_eq!(days_between(T0, T0 + DAY), 1);
        assert_eq!(days_between(T0, T0 - DAY + 1), 0);
    }
}
