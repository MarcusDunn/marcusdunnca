//! `GET /history` — every attempt, broken down per question.
//!
//! **Per-question, not per-attempt.** The history screen builds a skill × topic
//! matrix, which is only computable if each attempt carries its questions with
//! their skill, the document's topics, the format and whether it was correct.
//! An attempt-level score can be included or excluded whole and nothing else;
//! it cannot be segmented, and segmenting is the entire feature.
//!
//! The client filters in memory (`historyQuery` has no parameters, deliberately,
//! so the `n` counts stay stable while checkboxes toggle instead of flickering
//! through loading states). The query-parameter filters below are therefore
//! unused by the current frontend. They are kept because the endpoint is
//! specified as filterable and because they cost nothing when absent — but note
//! that filtering *drops* non-matching questions, so a filtered response is not
//! the shape the matrix expects.

use serde::Serialize;
use trainer_core::error::{Error, Result};
use trainer_core::model::Attempt;
use trainer_core::tags::{QuestionFormat, Skill, Topic};

use crate::state::AppState;

#[derive(Debug, Default)]
pub struct HistoryFilter {
    pub format: Option<QuestionFormat>,
    pub skill: Option<Skill>,
    pub topic: Option<Topic>,
}

impl HistoryFilter {
    /// Parse `?format=&skill=&topic=`.
    ///
    /// An unrecognised value is a 400, not an empty result. Those look
    /// identical to a user — a page saying "no attempts" — but mean opposite
    /// things, and the wrong one of them ("you have never done any causal
    /// questions") is a confidently false statement about the data.
    pub fn from_query(pairs: &[(String, String)]) -> Result<Self> {
        let mut f = Self::default();

        for (k, v) in pairs {
            match k.as_str() {
                "format" => {
                    f.format = Some(QuestionFormat::parse(v).ok_or_else(|| {
                        Error::Invalid(format!(
                            "unknown format {v:?}; expected one of: {}",
                            QuestionFormat::vocabulary()
                        ))
                    })?)
                }
                "skill" => {
                    f.skill = Some(Skill::parse(v).ok_or_else(|| {
                        Error::Invalid(format!(
                            "unknown skill {v:?}; expected one of: {}",
                            Skill::vocabulary()
                        ))
                    })?)
                }
                // No "expected one of" here, unlike `skill` above. The topic
                // vocabulary is open — the model coins new words as it meets
                // new subject matter — so there is no list to quote back, and
                // the only thing that can be wrong is the *shape*. A filter for
                // a well-formed word nobody has used yet is not an error; it
                // legitimately matches nothing.
                "topic" => {
                    f.topic = Some(Topic::parse(v).ok_or_else(|| {
                        Error::Invalid(format!(
                            "{v:?} is not a topic; topics are single lowercase words"
                        ))
                    })?)
                }
                // Unknown parameters are ignored rather than rejected: browsers
                // and link shorteners append their own, and a 400 on `?fbclid=`
                // would be maddening.
                _ => {}
            }
        }

        Ok(f)
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryQuestionDto {
    pub question_id: String,
    /// Carried per question rather than per attempt: once another format lands,
    /// one document can produce a mixed attempt, and the aggregation must still
    /// be able to segment it.
    pub format: QuestionFormat,
    pub skill: Skill,
    pub topics: Vec<Topic>,
    pub correct: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryAttemptDto {
    pub attempt_id: String,
    pub document_id: String,
    pub document_title: String,
    pub submitted_at: String,
    pub questions: Vec<HistoryQuestionDto>,
}

#[derive(Debug, Serialize)]
pub struct HistoryResponse {
    pub attempts: Vec<HistoryAttemptDto>,
}

pub async fn list(state: &AppState, filter: &HistoryFilter) -> Result<HistoryResponse> {
    let attempts = state.store.list_attempts().await?;

    let mut out = Vec::new();
    for attempt in attempts {
        // A topic filter is a property of the whole attempt — topics are tagged
        // per document — so it excludes the attempt entirely rather than
        // narrowing its questions.
        if let Some(topic) = &filter.topic {
            if !attempt.topics.contains(topic) {
                continue;
            }
        }

        if let Some(entry) = narrow(attempt, filter) {
            out.push(entry);
        }
    }

    Ok(HistoryResponse { attempts: out })
}

fn narrow(attempt: Attempt, filter: &HistoryFilter) -> Option<HistoryAttemptDto> {
    let questions: Vec<HistoryQuestionDto> = attempt
        .responses
        .into_iter()
        .filter(|r| filter.format.is_none_or(|f| r.format == f))
        .filter(|r| filter.skill.is_none_or(|s| r.skill == s))
        .map(|r| HistoryQuestionDto {
            question_id: r.qid,
            format: r.format,
            skill: r.skill,
            // Read from the response, not from the attempt, because the
            // response is where the snapshot was taken. Older rows written
            // before topics moved onto responses fall back to the attempt's.
            topics: if r.topics.is_empty() {
                attempt.topics.clone()
            } else {
                r.topics
            },
            correct: r.correct,
        })
        .collect();

    // An attempt with nothing matching is dropped rather than returned empty:
    // `HistoryQuestion.topics` is `min(1)` on the client and an attempt with no
    // questions contributes nothing to the matrix anyway.
    if questions.is_empty() {
        return None;
    }

    Some(HistoryAttemptDto {
        attempt_id: attempt.attempt_id,
        document_id: attempt.doc_id,
        document_title: attempt.doc_title,
        submitted_at: attempt.submitted_at,
        questions,
    })
}

#[cfg(test)]
mod tests {
    use trainer_core::model::AttemptResponse;
    use trainer_core::tags::{Choice, TAG_VERSION};

    use super::*;

    fn attempt_with(skills: Vec<Skill>, correct: Vec<bool>) -> Attempt {
        Attempt {
            pk: "DOC#x".into(),
            sk: "ATTEMPT#2026-08-29T00:00:00.000Z".into(),
            attempt_id: "11111111-1111-4111-8111-111111111111".into(),
            doc_id: "x".into(),
            doc_title: "t".into(),
            submitted_at: "2026-08-29T00:00:00.000Z".into(),
            responses: skills
                .into_iter()
                .zip(correct)
                .enumerate()
                .map(|(i, (skill, correct))| AttemptResponse {
                    qid: format!("q{i}"),
                    format: QuestionFormat::MultipleChoice,
                    skill,
                    topics: vec![Topic::parse("fiscal").expect("a valid topic")],
                    answer: Some(Choice::A),
                    correct,
                })
                .collect(),
            topics: vec![Topic::parse("fiscal").expect("a valid topic")],
            tag_version: TAG_VERSION,
            duration_ms: 0,
            score: 0,
            total: 0,
        }
    }

    /// The whole point of per-question storage: the client can compute a
    /// skill × topic rate because every question carries both.
    #[test]
    fn every_question_carries_skill_topics_format_and_correctness() {
        let attempt = attempt_with(vec![Skill::Causal, Skill::Definitional], vec![true, false]);

        let entry = narrow(attempt, &HistoryFilter::default()).expect("unfiltered");
        let json = serde_json::to_string(&entry).expect("serializes");

        for required in [
            "attemptId",
            "documentId",
            "documentTitle",
            "submittedAt",
            "questionId",
            "format",
            "skill",
            "topics",
            "correct",
        ] {
            assert!(
                json.contains(required),
                "history payload missing {required}"
            );
        }
        assert_eq!(entry.questions.len(), 2);
    }

    #[test]
    fn skill_filter_narrows_the_questions_within_an_attempt() {
        let attempt = attempt_with(
            vec![Skill::Causal, Skill::Causal, Skill::Definitional],
            vec![true, false, true],
        );

        let filter = HistoryFilter {
            skill: Some(Skill::Causal),
            ..Default::default()
        };

        let entry = narrow(attempt, &filter).expect("causal questions exist");
        assert_eq!(entry.questions.len(), 2);
        assert_eq!(entry.questions.iter().filter(|q| q.correct).count(), 1);
    }

    #[test]
    fn unknown_filter_values_are_rejected_not_silently_empty() {
        assert!(matches!(
            HistoryFilter::from_query(&[("skill".into(), "vibes".into())]),
            Err(Error::Invalid(_))
        ));

        // Unknown *parameters* are fine.
        assert!(HistoryFilter::from_query(&[("fbclid".into(), "x".into())]).is_ok());
    }

    #[test]
    fn attempts_with_no_matching_questions_are_dropped() {
        let attempt = attempt_with(vec![Skill::Scope], vec![true]);
        let filter = HistoryFilter {
            skill: Some(Skill::Causal),
            ..Default::default()
        };
        assert!(narrow(attempt, &filter).is_none());
    }
}
