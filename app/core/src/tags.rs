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
pub const TAG_VERSION: u32 = 3;

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
        $name:ident { $($variant:ident => $wire:literal),+ $(,)? }
    ) => {
        $(#[$meta])*
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord,
            serde::Serialize, serde::Deserialize,
        )]
        pub enum $name {
            $( #[serde(rename = $wire)] $variant, )+
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
    /// Grading dispatches on this, which is the reason it is an enum rather
    /// than an implicit assumption: `multiple_choice` is graded by comparing
    /// letters, `numeric` by parsing a typed number and comparing it against a
    /// tolerance, and the two share no code.
    ///
    /// **Both are still graded in this process, deterministically, with no
    /// model call.** That is the property to preserve when a third format is
    /// added. A format whose grading is a model call is a format whose scores
    /// drift when the model changes, and a score series that silently changes
    /// scale is worse than no score series.
    QuestionFormat {
        MultipleChoice => "multiple_choice",
        Numeric        => "numeric",
    }
}

/// What a question with no stated format is.
///
/// **This is a compatibility rule, not a preference.** Every question written
/// before `numeric` existed was multiple choice, and the generator was
/// deliberately not asked for a `format` — handing a model a field with one
/// legal value is all downside, and Sonnet proved it by returning
/// `"format": "definitional"` and costing a whole generation.
///
/// Now that there are two formats the generator is *still* not asked, because
/// it is still not a judgement call: the two kinds of question are requested in
/// two separate arrays, so the handler knows which is which without asking. See
/// `bedrock::assemble`.
impl Default for QuestionFormat {
    fn default() -> Self {
        Self::MultipleChoice
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

impl Confidence {
    /// Points for an answer in this band.
    ///
    /// # This table is derived, not chosen
    ///
    /// A scoring rule is **proper** when the highest expected score comes from
    /// reporting the confidence you actually hold. Most hand-written schemes
    /// are not — "+3 if certain and right, −3 if certain and wrong" pays you to
    /// misreport, which makes the resulting calibration record meaningless.
    ///
    /// Writing `S(band, correct)` for the entries below, the reader with a true
    /// belief `p` that they are right earns `p·S(b, true) + (1−p)·S(b, false)`.
    /// Two bands are equally good at the `p` where those expectations cross, so
    /// picking the crossover points *defines* the table:
    ///
    /// ```text
    /// guessing ↔ fairly_sure at p = 0.50:
    ///     0.50·(1−2) = 0.50·(S_wrong(fairly_sure) − 0)   →  −1
    /// fairly_sure ↔ certain at p = 0.80:
    ///     0.80·(2−3) = 0.20·(S_wrong(certain) − (−1))    →  −5
    /// ```
    ///
    /// # Why 0.50 and 0.80 rather than the published 0.67 and 0.80
    ///
    /// The scheme this is modelled on — Gardner-Medwin's certainty-based
    /// marking, used in UCL's summative medical exams — sits at 0.67/0.80. Those
    /// thresholds are correct for **true/false** questions, where guessing pays
    /// 0.5. Multiple choice here has four options, so guessing pays 0.25 and the
    /// bottom band has to start lower or it would never be the right report.
    ///
    /// # The floor is zero, not a penalty
    ///
    /// `guessing` + wrong scores 0. Saying "I don't know" is a *correct*
    /// statement about your own knowledge and must never cost anything, or the
    /// rule teaches you to bluff — the exact habit this is meant to train out.
    pub const fn points(&self, correct: bool) -> i32 {
        match (self, correct) {
            (Confidence::Guessing, true) => 1,
            (Confidence::Guessing, false) => 0,
            (Confidence::FairlySure, true) => 2,
            (Confidence::FairlySure, false) => -1,
            (Confidence::Certain, true) => 3,
            (Confidence::Certain, false) => -5,
        }
    }

    /// The best score any single answer can earn. Used to report a total
    /// against its ceiling rather than as a bare number.
    pub const MAX_POINTS_PER_QUESTION: i32 = 3;

    /// The belief range this band is the best report for, as percentages.
    ///
    /// Shown in the UI next to the band. The thresholds *are* the training
    /// signal — a band labelled only "fairly sure" asks for a feeling, whereas
    /// one labelled "50–80%" asks for a judgement you can be wrong about.
    pub const fn belief_range(&self) -> (u8, u8) {
        match self {
            // The floor is 25 rather than 0 because four options means you
            // cannot honestly hold less than a one-in-four belief in a guess.
            // On a numeric question the true floor is ~0, and quoting 25 there
            // would be wrong — so this is documented as the multiple-choice
            // reading and the UI does not print the lower bound for `guessing`.
            Confidence::Guessing => (25, 50),
            Confidence::FairlySure => (50, 80),
            Confidence::Certain => (80, 100),
        }
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

    /// Expected score for reporting `band` while actually believing `p`.
    fn expected(band: Confidence, p: f64) -> f64 {
        p * f64::from(band.points(true)) + (1.0 - p) * f64::from(band.points(false))
    }

    /// **The property the whole confidence feature rests on.**
    ///
    /// If reporting a band you do not hold ever pays better than reporting the
    /// one you do, the calibration record measures strategy rather than belief
    /// and every reliability number computed from it is fiction. This walks the
    /// belief range and asserts the honest report always wins.
    ///
    /// It is written against `points` rather than against the derivation in the
    /// doc comment on purpose: the comment explains the table, this checks it.
    #[test]
    fn the_scoring_rule_is_proper() {
        // Away from the crossovers themselves, where two bands legitimately tie.
        let honest = |p: f64| {
            if p < 0.50 {
                Confidence::Guessing
            } else if p < 0.80 {
                Confidence::FairlySure
            } else {
                Confidence::Certain
            }
        };

        for step in 0..=1000 {
            let p = f64::from(step) / 1000.0;
            // Skip the exact indifference points.
            if (p - 0.50).abs() < 1e-9 || (p - 0.80).abs() < 1e-9 {
                continue;
            }

            let truthful = honest(p);
            for band in Confidence::ALL {
                if *band == truthful {
                    continue;
                }
                assert!(
                    expected(truthful, p) > expected(*band, p),
                    "at p={p}, reporting {band} beats the honest {truthful} — \
                     the rule is not proper"
                );
            }
        }
    }

    /// The crossovers must sit where `belief_range` says they do, or the UI is
    /// printing thresholds that do not match the scoring.
    #[test]
    fn the_advertised_thresholds_are_the_real_ones() {
        for (lower, upper) in Confidence::ALL.iter().map(|c| c.belief_range()) {
            assert!(
                lower < upper,
                "an empty band would never be worth reporting"
            );
        }

        // Ties, exactly at the advertised boundaries.
        let eps = 1e-12;
        assert!(
            (expected(Confidence::Guessing, 0.50) - expected(Confidence::FairlySure, 0.50)).abs()
                < eps
        );
        assert!(
            (expected(Confidence::FairlySure, 0.80) - expected(Confidence::Certain, 0.80)).abs()
                < eps
        );

        // And those boundaries are the ones shown to the reader.
        assert_eq!(Confidence::Guessing.belief_range().1, 50);
        assert_eq!(Confidence::FairlySure.belief_range().1, 80);
    }

    /// Admitting ignorance must be free. If this ever goes negative the rule
    /// starts paying for bluffing, which is the habit it exists to train out.
    #[test]
    fn saying_you_do_not_know_never_costs_anything() {
        assert_eq!(Confidence::Guessing.points(false), 0);
        for band in Confidence::ALL {
            assert!(
                band.points(true) > 0,
                "a correct answer must never score zero or less"
            );
            assert!(band.points(true) <= Confidence::MAX_POINTS_PER_QUESTION);
        }
    }
}
