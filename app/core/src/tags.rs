//! The closed tag vocabulary.
//!
//! Tags exist so history can be segmented *retrospectively* — "how am I doing
//! on causal questions about energy documents" is only answerable if every
//! attempt ever recorded used the same words. A free-text tag field would
//! answer that question with `energy`, `Energy`, `energy-policy` and
//! `energy_and_utilities` and the feature would quietly be worthless.
//!
//! So the vocabulary is closed, and closure is enforced by the *type system*
//! rather than by a validation pass: these are unit-variant enums, and serde
//! rejects any string that is not an exact match. A model that invents
//! `macroeconomic` produces a deserialization failure, which the generate
//! handler turns into `status: failed` with a readable message. There is no
//! path by which an unknown tag reaches DynamoDB.
//!
//! `TAG_VERSION` is stamped on every document and every attempt. When the
//! vocabulary changes — and it will, this is a personal tool — old rows keep
//! their old version, so a chart can either exclude them or map them forward
//! deliberately instead of silently comparing incomparable things.

/// Bumped whenever the meaning or membership of any list below changes.
///
/// Adding a variant is a breaking change to historical comparability even
/// though it is not a breaking change to the code: attempts recorded before
/// the addition could never have carried the new tag, so their absence is not
/// evidence.
pub const TAG_VERSION: u32 = 1;

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
    /// Exactly one member today. It is still an enum rather than an implicit
    /// assumption because grading branches on it: `multiple_choice` is graded
    /// by equality in the handler, and anything added later (short answer,
    /// ordering) will not be, so the grader needs somewhere to dispatch.
    QuestionFormat {
        MultipleChoice => "multiple_choice",
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

closed_vocab! {
    /// Subject matter. Attached per *document* (`doc_tags`) and copied onto
    /// each attempt at submit time.
    ///
    /// Copied rather than joined on purpose: `doc_tags` on a document may be
    /// re-derived if the vocabulary changes, but an attempt is a historical
    /// record of what was true when it was taken. Joining back to the document
    /// would silently rewrite history every time a document was re-tagged.
    Topic {
        InternationalEconomics => "international_economics",
        Fiscal                 => "fiscal",
        Energy                 => "energy",
        Municipal              => "municipal",
        Regulatory             => "regulatory",
        Audit                  => "audit",
        Monetary               => "monetary",
        Trade                  => "trade",
    }
}

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
    fn unknown_tags_are_rejected_rather_than_coerced() {
        // This is the property the whole module exists for. If serde ever
        // starts accepting an unlisted variant — an added `#[serde(other)]`,
        // say — the tags become decorative and this test is the tripwire.
        assert!(serde_json::from_str::<Skill>("\"macroeconomic\"").is_err());
        assert!(serde_json::from_str::<Topic>("\"Energy\"").is_err());
        assert!(serde_json::from_str::<Choice>("\"e\"").is_err());
        assert!(serde_json::from_str::<QuestionFormat>("\"short_answer\"").is_err());
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
}
