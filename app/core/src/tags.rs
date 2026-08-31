//! The tag vocabularies.
//!
//! Tags exist so history can be segmented *retrospectively* — "how am I doing
//! on causal questions about housing documents" is only answerable if every
//! attempt ever recorded used the same words for the same thing.
//!
//! **Two different vocabularies live here, governed by two different rules.**
//!
//! [`Skill`], [`QuestionFormat`] and [`Choice`] are *closed*, and closure is
//! enforced by the type system rather than by a validation pass: these are
//! unit-variant enums, and serde rejects any string that is not an exact
//! match. A model that invents `macroeconomic` produces a deserialization
//! failure, which the generate handler turns into `status: failed` with a
//! readable message. These three can be closed because they describe the
//! *quiz*, which this app defines completely — there is no such thing as a
//! sixth skill arriving from outside.
//!
//! [`Topic`] is *open*. It describes the subject matter of documents nobody
//! has chosen yet, and a closed list was wrong for a reason that only showed
//! up once the model started picking: given a provincial housing report, three
//! generations picked three disjoint tag sets — `(international_economics,
//! trade)`, `(fiscal, monetary, regulatory)`, `(municipal, fiscal, energy)` —
//! because the list had no housing tag and every option was equally wrong. A
//! closed list does not prevent bad tags when it lacks the right one; it
//! guarantees them, and hides the fact by making every value "valid".
//!
//! So the model may coin new topics. What it may **not** do is coin the same
//! topic twice under different spellings, which is the failure that made
//! free-text tags worthless in the first place.
//!
//! The rule that prevents it: **a topic is one lowercase word.** Not a phrase,
//! not a compound. `energy-policy` is not a tag, it is two tags — `energy` and
//! `policy` — and [`normalise`] splits it into exactly that. This is a much
//! stronger constraint than canonicalising punctuation, because it collapses
//! the combinatorial part of the problem rather than tidying it: with compounds
//! allowed, `energy_policy`, `policy_energy` and `energy_and_policy` are three
//! distinct tags that a matrix cannot relate, while as single words they are
//! the same two facts in a different order. It also means a document about
//! energy policy is findable under `energy`, which a compound tag defeats.
//!
//! Consolidating genuinely distinct words that turn out to mean the same thing
//! is left as a deliberate later act — the registry makes the full set visible,
//! which is what makes that possible.

/// Bumped whenever the meaning or membership of a *closed* vocabulary changes,
/// or when the rules governing an open one do.
///
/// Adding a `Skill` variant is a breaking change to historical comparability
/// even though it is not a breaking change to the code: attempts recorded
/// before the addition could never have carried the new tag, so their absence
/// is not evidence.
///
/// Version 2 is where `Topic` stopped being a closed enum. Attempts stamped
/// version 1 carry topics drawn from the original eight and chosen by hand;
/// attempts stamped 2 carry topics chosen by the model from an open set. Both
/// are meaningful, they are just not the same measurement.
///
/// Version 4 replaced the three-band points table with a continuous
/// logarithmic score in bits over chance — see [`score_bits`]. A version-3
/// attempt's points and a version-4 attempt's score are numbers on different
/// scales measuring the same idea, so they must never be summed or averaged
/// together. The bands themselves survive as the review scheduler's input.
///
/// Version 3 is where two things changed at once, both of which make an
/// attempt's numbers mean something different:
///
///   - [`Confidence`] arrived, so a version-3 attempt has a points total and a
///     calibration record and a version-2 attempt has neither. Absence is not a
///     zero — those questions were answered without a confidence being asked
///     for, so they cannot be pooled into a reliability estimate.
///   - [`QuestionFormat::Numeric`] arrived and took [`Skill::FigureRecall`] with
///     it. From version 3 a figure-recall question is typed, not picked from
///     four options, so a version-2 figure-recall rate and a version-3 one are
///     rates on different tasks. Guessing gets you 25% on one and 0% on the
///     other.
pub const TAG_VERSION: u32 = 4;

/// Upper bound on how many topics one document may carry.
///
/// A document about everything is a document you cannot segment on, and each
/// extra tag multiplies that document's weight in the history matrix — a
/// twelve-tag document contributes to twelve topic rows.
pub const MAX_TOPICS_PER_DOC: usize = 12;

/// Length bounds on a single topic word, in characters.
///
/// The lower bound rejects the initialisms a model reaches for when it is
/// unsure (`gd`, `re`) and the connectives that fall out of splitting a phrase
/// (`of`, `to`). The upper bound is generous for one English word — the longest
/// plausible tag here is something like `infrastructure` at fourteen — and
/// exists to reject a compound that arrived without a separator to split on.
pub const TOPIC_MIN_LEN: usize = 3;
pub const TOPIC_MAX_LEN: usize = 20;

/// Generates a closed enum whose serde representation, `Display`, and the list
/// handed to the model in the prompt all come from one place.
///
/// The prompt and the deserializer drifting apart is the specific failure this
/// prevents: the model is told to use `figure_recall`, the enum was renamed to
/// `figure_recognition`, and every generation fails validation for a week
/// before anyone reads the error field.
macro_rules! closed_vocab {
    (
        $(#[$meta:meta])*
        $name:ident {
            $( $(#[$variant_meta:meta])* $variant:ident => $wire:literal ),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord,
            serde::Serialize, serde::Deserialize,
        )]
        pub enum $name {
            $( $(#[$variant_meta])* #[serde(rename = $wire)] $variant, )+
        }

        impl $name {
            /// Every member, in declaration order. Used to build the prompt and
            /// to answer `?format=`/`?skill=`/`?topic=` filter parsing.
            pub const ALL: &'static [$name] = &[ $( $name::$variant, )+ ];

            pub const fn as_str(&self) -> &'static str {
                match self { $( $name::$variant => $wire, )+ }
            }

            /// Parse a filter value supplied as a query-string parameter.
            /// Returns `None` for anything outside the vocabulary, which the
            /// caller reports as a 400 rather than silently matching nothing —
            /// a typo'd filter that returns an empty list looks exactly like
            /// "you have never done that", which is a misleading answer.
            pub fn parse(s: &str) -> Option<Self> {
                match s { $( $wire => Some($name::$variant), )+ _ => None }
            }

            /// Comma-separated list for embedding in the model prompt.
            pub fn vocabulary() -> String {
                Self::ALL
                    .iter()
                    .map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

closed_vocab! {
    /// How a question is presented and graded.
    ///
    /// **On a question this is the enum tag, not a field anyone sets.**
    /// `crate::model::QuestionBody` is `#[serde(tag = "format")]`, so the value
    /// stored on a question is generated from the variant and checked against
    /// the payload on the way back in. Nothing can write one that disagrees
    /// with the key beside it — which is precisely how `"format":
    /// "definitional"` once cost a whole generation.
    ///
    /// On an *attempt response* it is an ordinary stored field, and that is a
    /// different thing: an attempt records what the question was when it was
    /// answered, in the same way it copies the skill and the topics rather than
    /// joining back to a document that may since have been regenerated.
    ///
    /// It is also what the browser discriminates on and what `?format=` filters
    /// by, so it stays a closed vocabulary with a wire form.
    ///
    /// **Both formats are graded in this process, deterministically, with no
    /// model call.** That is the property to preserve when a third is added. A
    /// format whose grading is a model call is a format whose scores drift when
    /// the model changes, and a score series that silently changes scale is
    /// worse than no score series.
    QuestionFormat {
        MultipleChoice => "multiple_choice",
        Numeric        => "numeric",
    }
}

closed_vocab! {
    /// How sure the reader was, recorded with the answer.
    ///
    /// # Why this exists
    ///
    /// Accuracy alone cannot distinguish the two failures that matter in an
    /// argument: not knowing something, and *not knowing that you don't know
    /// it*. The second is the one that ends badly, and it is invisible in a
    /// score out of ten. Recording a confidence alongside every answer makes it
    /// measurable — and, because the scoring below is proper, makes it worth
    /// reporting honestly.
    ///
    /// The band names are deliberately about consequences rather than feelings.
    /// "Certain" means *I would say this on the record*, which is a decision,
    /// not a mood.
    Confidence {
        Guessing   => "guessing",
        FairlySure => "fairly_sure",
        Certain    => "certain",
    }
}

/// The score for one answer, in **bits of information over chance**.
///
/// # The rule
///
/// ```text
/// correct:  log2(  p   /   c  )
/// wrong:    log2((1−p) / (1−c))
/// ```
///
/// where `p` is the probability the reader stated and `c` is what pure chance
/// would have given them on a question of this shape — a quarter on four
/// options, near zero on a typed figure.
///
/// # Why this rule and not a table of points
///
/// The three bands were a step function fitted to a curve. They were *proper*,
/// so the arithmetic was honest, but they threw away most of what the reader
/// said: 51% and 79% scored identically, and the whole reason to ask for a
/// number rather than a feeling is that those are different claims. A step
/// function also creates cliffs — one point of slider movement worth two points
/// of score at the boundary, and nothing anywhere else.
///
/// This is the logarithmic scoring rule, which is the continuous thing the
/// table was approximating. Scaled by roughly 1.5 it reproduces the old entries
/// closely — `+1.85` where the table said `+3`, `−2.9` where it said `−5` — so
/// the change is a refinement rather than a reversal.
///
/// # Chance-referencing is free
///
/// Subtracting the chance baseline looks like it might break properness. It
/// does not, and the reason is worth writing down. With a true belief `q`:
///
/// ```text
/// E[S] = q·log2(p/c) + (1−q)·log2((1−p)/(1−c))
///      = q·log2(p) + (1−q)·log2(1−p)  −  [q·log2(c) + (1−q)·log2(1−c)]
/// ```
///
/// The bracket has no `p` in it. It is an additive constant, so it shifts every
/// score by the same amount and cannot move the maximum — which still sits at
/// `p = q`. The rule is strictly proper before and after.
///
/// What the constant buys is a meaningful zero. **Reporting chance scores
/// exactly nothing, whichever way the answer goes.** Admitting you are guessing
/// is free, which the old table achieved by hand and this gets by construction;
/// and getting a coin-flip right no longer pays, because luck at chance odds is
/// not knowledge.
///
/// # The units are real
///
/// A bit is a bit: `+2` means your confidence carried two bits of information
/// beyond guessing. That is also why a typed figure is worth more than a
/// multiple-choice question — there is more uncertainty to remove — and why a
/// confident error costs so much more than a confident hit earns. Those are
/// properties of the information, not of a scale someone picked.
pub fn score_bits(percent: u8, correct: bool, format: QuestionFormat) -> f64 {
    let chance = f64::from(Confidence::chance_floor_percent(format)) / 100.0;
    let p = f64::from(percent.clamp(
        Confidence::chance_floor_percent(format),
        Confidence::MAX_PERCENT,
    )) / 100.0;

    if correct {
        (p / chance).log2()
    } else {
        ((1.0 - p) / (1.0 - chance)).log2()
    }
}

/// The most one answer can earn, given the format's chance baseline.
///
/// Used to report a total against its ceiling. It differs by format on purpose
/// — see the note on units above.
pub fn max_score_bits(format: QuestionFormat) -> f64 {
    score_bits(Confidence::MAX_PERCENT, true, format)
}

impl Confidence {
    /// The highest probability the slider will report.
    ///
    /// Not 100, for two reasons that agree. The arithmetic one: `log2(0)` is
    /// negative infinity, so a claim of certainty that turned out wrong would
    /// score `-inf` and poison every total it entered. The real one: certainty
    /// is never warranted about a figure you read once, and a scale that offers
    /// it invites a claim nobody should make. Forecasting tournaments cap at 99%
    /// for the same reason.
    pub const MAX_PERCENT: u8 = 99;

    /// The belief range this band is the best report for, as percentages.
    ///
    /// The thresholds *are* the training signal — a band labelled only "fairly
    /// sure" asks for a feeling, whereas one labelled "50–80%" asks for a
    /// judgement you can be wrong about.
    pub const fn belief_range(&self) -> (u8, u8) {
        match self {
            // The floor is 25 rather than 0 because four options means you
            // cannot honestly hold less than a one-in-four belief in a guess.
            // On a numeric question the true floor is ~0 — see
            // `chance_floor_percent`.
            Confidence::Guessing => (25, 50),
            Confidence::FairlySure => (50, 80),
            Confidence::Certain => (80, 100),
        }
    }

    /// The band a stated probability falls in.
    ///
    /// **The bands are now derived, not chosen.** The reader states a
    /// percentage; this puts it in a bucket. Both numbers are kept: the
    /// percentage is what a reliability curve and a Brier score are computed
    /// from, and the band is what the points table and the review scheduler
    /// consume — neither of which wants a continuous input, and both of which
    /// already have years of meaning attached to three buckets.
    ///
    /// The cut points are the same 0.50 and 0.80 the scoring rule crosses over
    /// at, so a reader who reports honestly lands in the band that pays best.
    /// That is what keeps the rule proper under a finer input rather than in
    /// spite of it.
    pub fn from_percent(percent: u8) -> Self {
        if percent < Confidence::Guessing.belief_range().1 {
            Confidence::Guessing
        } else if percent < Confidence::FairlySure.belief_range().1 {
            Confidence::FairlySure
        } else {
            Confidence::Certain
        }
    }

    /// The lowest honest probability on a question of this shape.
    ///
    /// Below chance is not modesty, it is an error: on four options you will
    /// answer *something*, so a one-in-four belief is the floor. The slider
    /// starts here and cannot go under it, which removes a whole class of
    /// meaningless report rather than scoring it.
    ///
    /// A typed figure has no options and therefore no floor worth speaking of —
    /// the space of wrong numbers is unbounded. Two percent stands in for zero
    /// so that a log-style score would stay finite if one is ever added, and so
    /// the slider has somewhere to sit.
    pub const fn chance_floor_percent(format: QuestionFormat) -> u8 {
        match format {
            QuestionFormat::MultipleChoice => 25,
            QuestionFormat::Numeric => 2,
        }
    }

    /// Squared error of a stated probability against what happened.
    ///
    /// The Brier score, per answer, in `[0, 1]` — lower is better. Averaged
    /// over enough answers it is *the* summary of calibration, and unlike the
    /// band table it uses the whole of what the reader said: 79% and 51% are
    /// the same band and very different claims.
    ///
    /// Kept separate from `points` on purpose. Points are what you play
    /// against, and being sure and wrong has to hurt; a Brier score is a
    /// measurement, and it correctly *rewards* having said 30% on something you
    /// got wrong. One number cannot do both jobs without lying about one of
    /// them.
    pub fn brier(percent: u8, correct: bool) -> f64 {
        let p = f64::from(percent) / 100.0;
        let outcome = f64::from(u8::from(correct));
        (p - outcome).powi(2)
    }
}

closed_vocab! {
    /// How long a question stays worth being asked again.
    ///
    /// # The problem this exists for
    ///
    /// A quarterly forecast read today is reviewed in three years. The question
    /// is still *answerable* — what a report predicted in March 2026 is a fixed
    /// historical fact — but drilling it has stopped being useful, and the
    /// review queue fills with dead trivia that crowds out live material.
    ///
    /// Worse, and this is the part that matters: a figure rehearsed for years
    /// without its date attached stops being remembered as "TD's 2026 forecast"
    /// and starts being remembered as "the number". Spaced repetition is very
    /// good at making things stick, which makes it very good at making a stale
    /// figure stick. An app whose purpose is having accurate numbers to hand
    /// must not be the reason you cite a three-year-old forecast as current.
    ///
    /// Two things guard against that and they are different. This one retires
    /// the question. The other is that every prompt must name its source and
    /// period — see the generator's schema — so that what is rehearsed is a
    /// correctly-scoped historical claim rather than a free-floating number.
    ///
    /// # Why the model picks it
    ///
    /// Shelf life is a property of the claim, and the model has the document in
    /// front of it. "We forecast 2.1% growth in 2027" and "the equalization
    /// formula is set out in section 4" come out of the same PDF and age
    /// completely differently, so it cannot be a property of the document, and
    /// no rule over titles or dates would separate them.
    Shelf {
        /// Structural, definitional, or historical-by-construction. Never
        /// retires: how a mechanism works does not stop being true.
        Perennial => "perennial",
        /// Policy, institutional arrangements, multi-year trends. Ages slowly.
        Slow      => "slow",
        /// Forecasts, current conditions, quarterly figures. The thing this
        /// vocabulary exists for.
        Dated     => "dated",
    }
}

impl Shelf {
    /// How long after the document was read this question keeps being
    /// scheduled. `None` means never retire.
    ///
    /// These are deliberately generous. Retiring a question is not free — it
    /// removes something you chose to learn — so the bar is "this is now
    /// clearly historical", not "this is getting old". A `dated` question
    /// surviving eighteen months has been through five or six repetitions,
    /// which is most of the value it was ever going to give.
    pub const fn horizon_days(&self) -> Option<i64> {
        match self {
            Shelf::Perennial => None,
            Shelf::Slow => Some(5 * 365),
            Shelf::Dated => Some(548),
        }
    }
}

/// What a question written before shelf life existed is assumed to be.
///
/// `Slow`, not `Dated`, and the asymmetry is the point: the two errors do not
/// cost the same. Guessing `slow` for something genuinely dated costs a handful
/// of reviews of a stale question. Guessing `dated` for something perennial
/// silently deletes it from the queue at eighteen months, and nothing would
/// ever tell you it had gone.
impl Default for Shelf {
    fn default() -> Self {
        Self::Slow
    }
}

closed_vocab! {
    /// What the question is testing. Attached per question, not per document —
    /// a single document produces questions across several skills, and the
    /// whole point of storing them per response is being able to ask "am I bad
    /// at relational questions" across every document ever read.
    Skill {
        FigureRecall  => "figure_recall",
        Relational    => "relational",
        Definitional  => "definitional",
        Causal        => "causal",
        Scope         => "scope",
    }
}

/// Subject matter: one lowercase word. Attached per *document* and copied onto
/// each attempt at submit time.
///
/// Copied rather than joined on purpose: a document's topics may be re-derived
/// if it is regenerated, but an attempt is a historical record of what was true
/// when it was taken. Joining back to the document would silently rewrite
/// history every time a document was re-tagged.
///
/// # Strict on the way in, permissive on the way out
///
/// [`Topic::parse`] is the *only* constructor, and every ingress path goes
/// through it: model output, and the `?topic=` filter. So everything written
/// from here on satisfies the one-lowercase-word rule.
///
/// `Deserialize`, by contrast, accepts whatever is already stored. That
/// asymmetry is deliberate. Rows written under the closed vocabulary carry
/// values like `international_economics`, and a strict `Deserialize` would turn
/// every one of them into a 500 on `GET /docs` — a validation rule applied to
/// data that already exists does not clean it, it just makes it unreadable. Old
/// tags display as they are and age out; new ones are clean by construction.
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
pub struct Topic(String);

impl Topic {
    /// The one constructor. `None` if `word` is not a single lowercase word
    /// within the length bounds.
    ///
    /// Case and surrounding whitespace are forgiven because they are unambiguous
    /// — `" Energy"` can only have meant `energy`. Anything else is refused
    /// rather than guessed at: `energy-policy` is not repaired here, because
    /// repairing it means deciding whether it is one tag or two, and that
    /// decision belongs to [`normalise`], which can return both.
    pub fn parse(word: &str) -> Option<Self> {
        let word = word.trim().to_ascii_lowercase();

        if !(TOPIC_MIN_LEN..=TOPIC_MAX_LEN).contains(&word.chars().count()) {
            return None;
        }
        if !word.chars().all(|c| c.is_ascii_lowercase()) {
            return None;
        }
        if STOPWORDS.contains(&word.as_str()) {
            return None;
        }

        Some(Self(word))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Topic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Words that survive the length and charset rules but carry no subject matter.
///
/// These exist because splitting a phrase produces them: `energy_and_policy`
/// becomes three fragments and one of them is `and`. Without this list, `and`
/// becomes a topic attached to a third of the corpus and a row in the history
/// matrix that means nothing.
const STOPWORDS: &[&str] = &[
    "and", "the", "for", "with", "from", "into", "its", "our", "their", "this", "that", "not",
    "are", "was", "were", "been", "has", "had", "can", "may", "per", "via", "new", "all", "any",
];

/// Turn whatever the model returned into storable topics.
///
/// Every ingress path for model-chosen tags goes through here, and it is
/// forgiving on purpose: a model that returns `"Energy Policy"` has understood
/// the task and formatted it wrong, and failing the whole document over that
/// would cost a Bedrock call to punish a formatting slip. So a phrase is split
/// into its words rather than rejected.
///
/// What it will not do is invent order or meaning. Fragments that are not
/// usable words are dropped, not repaired; duplicates collapse; the result is
/// capped at [`MAX_TOPICS_PER_DOC`]. First occurrence wins, so the order the
/// model considered most relevant is the order that survives truncation.
///
/// Returning empty is possible and is *not* silently acceptable — the caller
/// treats it as a failed generation, because a document with no topics is
/// invisible to every segment of the history view.
pub fn normalise<I, S>(raw: I) -> Vec<Topic>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut out: Vec<Topic> = Vec::new();

    for entry in raw {
        // Split on anything that is not a letter, which covers every separator
        // a model has reached for: spaces, hyphens, underscores, slashes,
        // commas, and the digits in `2026_outlook`.
        for fragment in entry.as_ref().split(|c: char| !c.is_ascii_alphabetic()) {
            let Some(topic) = Topic::parse(fragment) else {
                continue;
            };
            if !out.contains(&topic) {
                out.push(topic);
            }
            if out.len() == MAX_TOPICS_PER_DOC {
                return out;
            }
        }
    }

    out
}

/// Tags the registry is seeded with, so the first generation has something to
/// reuse rather than coining a vocabulary from nothing.
///
/// These are the original closed vocabulary, minus the compounds it contained:
/// `international_economics` is seeded as `international` and `economics`,
/// which is precisely the change this module now enforces.
pub const SEED_TOPICS: &[&str] = &[
    "international",
    "economics",
    "fiscal",
    "energy",
    "municipal",
    "regulatory",
    "audit",
    "monetary",
    "trade",
    "housing",
    "labour",
];

closed_vocab! {
    /// The answer key for a multiple-choice question, and the shape of a
    /// submitted response.
    ///
    /// A letter rather than an index into `options`, because that is what the
    /// model emits and what the browser posts back, and because an out-of-range
    /// index is a runtime panic waiting to happen whereas an out-of-range
    /// letter is a deserialization error. "answer must be one of a-d" is
    /// enforced by this type existing; there is no separate check.
    Choice {
        A => "a",
        B => "b",
        C => "c",
        D => "d",
    }
}

impl Choice {
    /// Index into `options`. Total by construction — `Choice::ALL.len()` is 4
    /// and every question is validated to have exactly 4 options, so this can
    /// never index out of bounds.
    pub const fn index(&self) -> usize {
        match self {
            Choice::A => 0,
            Choice::B => 1,
            Choice::C => 2,
            Choice::D => 3,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_closed_tags_are_rejected_rather_than_coerced() {
        // This is the property the closed vocabularies exist for. If serde ever
        // starts accepting an unlisted variant — an added `#[serde(other)]`,
        // say — those tags become decorative and this test is the tripwire.
        //
        // `Topic` is deliberately absent: it is open, and its rules are
        // enforced by `parse` on ingress rather than by serde. See below.
        assert!(serde_json::from_str::<Skill>("\"macroeconomic\"").is_err());
        assert!(serde_json::from_str::<Choice>("\"e\"").is_err());
        assert!(serde_json::from_str::<QuestionFormat>("\"short_answer\"").is_err());
    }

    #[test]
    fn a_topic_is_one_lowercase_word() {
        assert_eq!(
            Topic::parse("energy").map(|t| t.as_str().to_string()),
            Some("energy".into())
        );
        // Unambiguous, so forgiven.
        assert_eq!(
            Topic::parse("  Energy ").map(|t| t.as_str().to_string()),
            Some("energy".into())
        );
        // Ambiguous — one tag or two? `normalise` decides, `parse` refuses.
        assert!(Topic::parse("energy-policy").is_none());
        assert!(Topic::parse("energy policy").is_none());
        assert!(Topic::parse("of").is_none(), "under the length floor");
        assert!(Topic::parse("and").is_none(), "stopword");
        assert!(
            Topic::parse("housing2026").is_none(),
            "digits are not letters"
        );
        assert!(Topic::parse(&"a".repeat(TOPIC_MAX_LEN + 1)).is_none());
    }

    #[test]
    fn compounds_become_separate_tags() {
        // The rule the module exists to enforce, in the form the model actually
        // breaks it.
        let got: Vec<String> = normalise(["Energy-Policy", "housing"])
            .iter()
            .map(|t| t.as_str().to_string())
            .collect();
        assert_eq!(got, vec!["energy", "policy", "housing"]);
    }

    #[test]
    fn normalise_drops_connectives_and_collapses_duplicates() {
        let got: Vec<String> = normalise(["energy and policy", "policy", "ENERGY"])
            .iter()
            .map(|t| t.as_str().to_string())
            .collect();
        assert_eq!(got, vec!["energy", "policy"], "no `and`, no repeats");
    }

    #[test]
    fn normalise_caps_at_the_document_limit() {
        // Twelve distinct words plus one more; the extra must not land.
        let words = [
            "energy",
            "policy",
            "housing",
            "fiscal",
            "monetary",
            "trade",
            "audit",
            "municipal",
            "regulatory",
            "labour",
            "economics",
            "international",
            "excess",
        ];
        let got = normalise(words);
        assert_eq!(got.len(), MAX_TOPICS_PER_DOC);
        assert!(!got.iter().any(|t| t.as_str() == "excess"));
    }

    #[test]
    fn every_seed_topic_satisfies_the_rule_it_seeds() {
        // A seed that `parse` would reject would be offered to the model as a
        // reusable tag and then refused when the model reused it.
        for word in SEED_TOPICS {
            assert!(
                Topic::parse(word).is_some(),
                "seed {word:?} is not a valid topic"
            );
        }
    }

    #[test]
    fn legacy_compound_topics_still_deserialize() {
        // Rows written under the closed vocabulary contain these. If this ever
        // starts failing, `GET /docs` 500s on every document created before the
        // vocabulary opened.
        let legacy: Topic =
            serde_json::from_str("\"international_economics\"").expect("legacy tag still reads");
        assert_eq!(legacy.as_str(), "international_economics");
    }

    #[test]
    fn wire_form_matches_the_prompt_vocabulary() {
        // The macro guarantees this structurally; the test guards against
        // someone "simplifying" the macro away and hand-writing both lists.
        for skill in Skill::ALL {
            let json = serde_json::to_string(skill).expect("unit variant serializes");
            assert_eq!(json, format!("\"{}\"", skill.as_str()));
            assert!(Skill::vocabulary().contains(skill.as_str()));
        }
    }

    #[test]
    fn choice_indexes_stay_within_four_options() {
        assert_eq!(Choice::ALL.len(), 4);
        for (i, c) in Choice::ALL.iter().enumerate() {
            assert_eq!(c.index(), i);
        }
    }

    /// Expected score for stating `stated` while actually believing `belief`.
    fn expected(stated: u8, belief: f64, format: QuestionFormat) -> f64 {
        belief * score_bits(stated, true, format)
            + (1.0 - belief) * score_bits(stated, false, format)
    }

    /// **The property the whole confidence feature rests on.**
    ///
    /// If stating a probability you do not hold ever pays better than stating
    /// the one you do, the record measures strategy rather than belief and
    /// every calibration number computed from it is fiction. This walks the
    /// belief range and asserts the truthful report maximises expected score,
    /// for both formats, since they have different baselines.
    #[test]
    fn the_scoring_rule_is_proper() {
        for format in [QuestionFormat::MultipleChoice, QuestionFormat::Numeric] {
            let floor = Confidence::chance_floor_percent(format);

            for truth in (floor..=Confidence::MAX_PERCENT).step_by(3) {
                let belief = f64::from(truth) / 100.0;
                let honest = expected(truth, belief, format);

                for stated in (floor..=Confidence::MAX_PERCENT).step_by(3) {
                    if stated == truth {
                        continue;
                    }
                    assert!(
                        honest >= expected(stated, belief, format) - 1e-12,
                        "{format}: believing {truth}% but stating {stated}% pays better"
                    );
                }
            }
        }
    }

    /// The meaningful zero. Reporting chance earns nothing whichever way the
    /// answer falls — so admitting a guess is free, and getting a guess right
    /// is not mistaken for knowing something.
    #[test]
    fn reporting_chance_scores_exactly_nothing() {
        for format in [QuestionFormat::MultipleChoice, QuestionFormat::Numeric] {
            let floor = Confidence::chance_floor_percent(format);
            assert!(score_bits(floor, true, format).abs() < 1e-12, "{format}");
            assert!(score_bits(floor, false, format).abs() < 1e-12, "{format}");
        }
    }

    /// Continuity is the point of the change: every step of the slider has to
    /// move the score, and in the direction you would expect.
    #[test]
    fn the_score_moves_with_every_step_of_the_slider() {
        let format = QuestionFormat::MultipleChoice;
        let floor = Confidence::chance_floor_percent(format);

        for percent in floor..Confidence::MAX_PERCENT {
            let next = percent + 1;
            assert!(
                score_bits(next, true, format) > score_bits(percent, true, format),
                "being more confident and right must pay more at {percent}%"
            );
            assert!(
                score_bits(next, false, format) < score_bits(percent, false, format),
                "being more confident and wrong must cost more at {percent}%"
            );
        }

        // The cliff the bands had: 51% and 79% used to score identically.
        assert!(score_bits(79, true, format) > score_bits(51, true, format));
    }

    /// A confident error has to cost more than a confident hit earns, or
    /// overconfidence is cheap and the rule trains the wrong habit.
    #[test]
    fn being_sure_and_wrong_costs_more_than_being_sure_and_right_earns() {
        for format in [QuestionFormat::MultipleChoice, QuestionFormat::Numeric] {
            let gain = score_bits(Confidence::MAX_PERCENT, true, format);
            let loss = score_bits(Confidence::MAX_PERCENT, false, format);
            assert!(gain > 0.0 && loss < 0.0, "{format}");
            assert!(
                loss.abs() > gain,
                "{format}: sure-and-wrong {loss} must hurt more than sure-and-right {gain} pays"
            );
        }
    }

    /// Every score must be finite. An infinity would poison the attempt total
    /// and every average downstream, which is what capping the slider below
    /// 100% buys — including for a percentage no UI should ever send.
    #[test]
    fn no_stated_probability_produces_an_infinite_score() {
        for format in [QuestionFormat::MultipleChoice, QuestionFormat::Numeric] {
            for percent in 0..=255u8 {
                for correct in [true, false] {
                    let s = score_bits(percent, correct, format);
                    assert!(
                        s.is_finite(),
                        "{format} {percent}% correct={correct} -> {s}"
                    );
                }
            }
        }
    }

    /// The new rule should be recognisably the old table rather than a
    /// different scheme wearing its name — the bands were a step function
    /// fitted to this curve, so scaled by about 1.5 they should still line up.
    #[test]
    fn the_curve_reproduces_the_shape_of_the_table_it_replaces() {
        let mc = QuestionFormat::MultipleChoice;
        let scaled = |p: u8, c: bool| score_bits(p, c, mc) * 1.5;

        // The old table: guessing 1/0, fairly_sure 2/-1, certain 3/-5.
        assert!((scaled(65, true) - 2.0).abs() < 0.5, "{}", scaled(65, true));
        assert!(
            (scaled(65, false) + 1.0).abs() < 0.8,
            "{}",
            scaled(65, false)
        );
        assert!((scaled(90, true) - 3.0).abs() < 0.5, "{}", scaled(90, true));
        assert!(
            (scaled(90, false) + 5.0).abs() < 1.0,
            "{}",
            scaled(90, false)
        );
    }

    /// The bands survive only as the review scheduler's input — the score no
    /// longer uses them. The cut points must still be the ones the UI
    /// advertises, or the scheduler grades a claim the reader did not make.
    #[test]
    fn the_bands_tile_the_whole_range() {
        assert_eq!(Confidence::from_percent(0), Confidence::Guessing);
        assert_eq!(Confidence::from_percent(49), Confidence::Guessing);
        assert_eq!(Confidence::from_percent(50), Confidence::FairlySure);
        assert_eq!(Confidence::from_percent(79), Confidence::FairlySure);
        assert_eq!(Confidence::from_percent(80), Confidence::Certain);
        assert_eq!(Confidence::from_percent(100), Confidence::Certain);
    }

    /// Below chance is not modesty, it is an error — and the floor differs by
    /// format, because four options guarantee a one-in-four hit and a typed
    /// figure guarantees nothing.
    #[test]
    fn the_floor_is_chance_on_multiple_choice_and_near_zero_on_a_figure() {
        assert_eq!(
            Confidence::chance_floor_percent(QuestionFormat::MultipleChoice),
            25
        );
        assert!(Confidence::chance_floor_percent(QuestionFormat::Numeric) < 25);
    }

    /// The Brier score is a *measurement*, not a score to play against, and the
    /// difference shows exactly here: saying 30% and being wrong is good
    /// calibration and scores well, while the points table still charges you
    /// nothing for it and would have charged you for saying 90%.
    #[test]
    fn brier_rewards_being_uncertain_about_something_you_got_wrong() {
        assert!((Confidence::brier(100, true) - 0.0).abs() < 1e-12);
        assert!((Confidence::brier(100, false) - 1.0).abs() < 1e-12);
        assert!((Confidence::brier(50, true) - 0.25).abs() < 1e-12);
        assert!((Confidence::brier(50, false) - 0.25).abs() < 1e-12);

        assert!(
            Confidence::brier(30, false) < Confidence::brier(90, false),
            "a hedged wrong answer must score better than a confident one"
        );
        assert!(
            Confidence::brier(30, true) > Confidence::brier(90, true),
            "and worse when it turns out you were right"
        );
    }
}
