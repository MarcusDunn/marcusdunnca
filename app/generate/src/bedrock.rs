//! The model call, and the validation that decides whether to trust it.
//!
//! Two rules govern this file:
//!
//! 1. **The model's output is untrusted input.** It is validated exactly as
//!    strictly as a request body from the internet would be, because a model
//!    that invents a tag or returns nine questions is not a rare event — it is
//!    the normal behaviour of a small, cheap model, and the whole reason the
//!    vocabulary is closed.
//! 2. **The full response is never logged.** It contains the answer key. A log
//!    group is not as private as a DynamoDB item, it is retained on a different
//!    schedule, and "the answers are in CloudWatch" is the same bug as "the
//!    answers are in the quiz payload" with more steps.

use aws_sdk_bedrockruntime::primitives::Blob;
use aws_sdk_bedrockruntime::types::{
    ContentBlock, ConversationRole, ConverseOutput, DocumentBlock, DocumentFormat, DocumentSource,
    InferenceConfiguration, Message, SystemContentBlock,
};
use aws_sdk_bedrockruntime::Client;
use serde::Deserialize;
use trainer_core::error::{aws, Error, Result};
use trainer_core::model::Question;
use trainer_core::tags::{Choice, QuestionFormat, Skill};

/// How many questions a quiz has. Not configurable: it is baked into the
/// prompt, the validation, and the reader's expectations, and a quiz whose
/// length varies run to run makes scores incomparable across documents.
pub const QUESTIONS_PER_DOC: usize = 10;

/// Exactly four options, because [`Choice`] has exactly four variants. These
/// two numbers are not independent — see `Choice::index`.
pub const OPTIONS_PER_QUESTION: usize = 4;

/// What the model is asked to return.
///
/// Deserialized with the *same* [`Question`] type the rest of the app uses, so
/// there is no separate "model shape" that could drift from the stored shape.
/// The closed enums do the vocabulary enforcement: a `skill` outside the list is
/// a serde error here, before any of the checks below run.
///
/// **The model is not asked for topics.** Topics are chosen by the reader at
/// upload time — the document list shows them while status is still `pending`,
/// so they must exist before this runs — and asking the model for a value that
/// is already known would create a conflict with no arbiter and a class of
/// hallucination with no upside.
#[derive(Debug, Deserialize)]
struct GeneratedQuiz {
    questions: Vec<Question>,
}

/// System prompt.
///
/// The vocabulary lists are interpolated from the enums rather than typed out,
/// so the instruction and the deserializer cannot disagree. That specific drift
/// — prompt says one word, enum expects another — produces a 100% failure rate
/// with a validation error that reads like the model is misbehaving.
fn system_prompt() -> String {
    format!(
        "You generate reading-comprehension quizzes from documents.\n\
         \n\
         Return ONLY a JSON object. No prose, no markdown fences, no commentary.\n\
         \n\
         Shape:\n\
         {{\n\
         \x20 \"questions\": [\n\
         \x20   {{\"id\": \"q1\", \"format\": \"multiple_choice\", \"skill\": \"<skill>\",\n\
         \x20    \"prompt\": \"...\", \"options\": [\"...\",\"...\",\"...\",\"...\"],\n\
         \x20    \"answer\": \"a\", \"explanation\": \"...\"}}\n\
         \x20 ]\n\
         }}\n\
         \n\
         Hard requirements. A response violating any of these is discarded:\n\
         - exactly {questions} questions, with ids q1 through q{questions}\n\
         - exactly {options} options per question\n\
         - \"answer\" is one of: {choices} — the letter of the correct option\n\
         - \"format\" is one of: {formats}\n\
         - \"skill\" is exactly one of: {skills}\n\
         \n\
         Do not invent skill values. If none fits well, choose the closest; an \
         unlisted value causes the whole response to be discarded.\n\
         \n\
         Question quality:\n\
         - answerable only by having read this document, not from general knowledge\n\
         - the three wrong options must be plausible and drawn from the document, \
           not obviously absurd\n\
         - \"explanation\" cites where in the document the answer comes from\n\
         - spread the questions across the available skills rather than asking \
           ten of the same kind",
        questions = QUESTIONS_PER_DOC,
        options = OPTIONS_PER_QUESTION,
        choices = Choice::vocabulary(),
        formats = QuestionFormat::vocabulary(),
        skills = Skill::vocabulary(),
    )
}

/// Send the PDF to Bedrock as a Converse `document` block and parse the result.
///
/// The PDF goes in as bytes, not as text this function extracted. That is the
/// verified-working path: the model reads the document's own structure —
/// tables, figures, headings — which is what makes `figure_recall` a meaningful
/// skill tag. Pre-extracting to plain text would flatten exactly the structure
/// several of the skills are about.
pub async fn generate(
    client: &Client,
    model_id: &str,
    title: &str,
    pdf: Vec<u8>,
) -> Result<Vec<Question>> {
    let document = DocumentBlock::builder()
        .format(DocumentFormat::Pdf)
        // Bedrock restricts this to alphanumerics, single spaces, hyphens,
        // parentheses and brackets, and AWS explicitly warns it is a prompt
        // injection vector. A user-supplied title would be both a validation
        // hazard and an injection one, so it is not used: the name is a
        // constant and the title is passed separately, as data, below.
        .name("uploaded document")
        .source(DocumentSource::Bytes(Blob::new(pdf)))
        .build()
        .map_err(|e| Error::Aws(format!("building document block: {e}")))?;

    let message = Message::builder()
        .role(ConversationRole::User)
        .content(ContentBlock::Document(document))
        .content(ContentBlock::Text(format!(
            "The document is titled {title:?}. Generate the quiz."
        )))
        .build()
        .map_err(|e| Error::Aws(format!("building message: {e}")))?;

    let inference = InferenceConfiguration::builder()
        // Ten questions with four options and an explanation each runs to
        // roughly 2-3k tokens. The headroom is for verbose explanations; the
        // cap exists so a model that starts looping cannot run up a bill.
        .max_tokens(4096)
        // Low, not zero. This is an extraction-and-transformation task where
        // creativity is not wanted, but a little variance keeps a retry of a
        // failed generation from reproducing the same malformed output.
        .temperature(0.2)
        .build();

    let out = client
        .converse()
        .model_id(model_id)
        .system(SystemContentBlock::Text(system_prompt()))
        .messages(message)
        .inference_config(inference)
        .send()
        .await
        .map_err(aws)?;

    let text = extract_text(out.output)?;
    let quiz = parse(&text)?;

    validate(&quiz)?;

    Ok(quiz.questions)
}

fn extract_text(output: Option<ConverseOutput>) -> Result<String> {
    let Some(ConverseOutput::Message(message)) = output else {
        return Err(Error::Invalid(
            "the model returned no message; try again".into(),
        ));
    };

    let text: String = message
        .content
        .into_iter()
        .filter_map(|block| match block {
            ContentBlock::Text(t) => Some(t),
            // Reasoning blocks and anything else are dropped rather than
            // concatenated — they are not JSON and would break the parse.
            _ => None,
        })
        .collect();

    if text.trim().is_empty() {
        return Err(Error::Invalid(
            "the model returned an empty response; try again".into(),
        ));
    }

    Ok(text)
}

/// Parse the model's text into the quiz.
///
/// The fence-stripping is not politeness, it is necessary: instructing a model
/// to emit bare JSON works most of the time and fails often enough that a
/// handler which does not tolerate ```` ```json ```` wrappers will mark
/// perfectly good generations as failed.
///
/// Note what is *not* logged on failure. The response contains the answer key,
/// so a parse error logs its length and the serde message — which names a path
/// and a position, not content — and nothing else.
fn parse(text: &str) -> Result<GeneratedQuiz> {
    let json = extract_json_object(text).ok_or_else(|| {
        tracing::warn!(
            response_len = text.len(),
            "model response contained no JSON object"
        );
        Error::Invalid("the model did not return JSON; try again".into())
    })?;

    serde_json::from_str(json).map_err(|e| {
        // `e` here can name an unknown enum variant, which is the vocabulary
        // enforcement firing. That is worth surfacing to the reader, because
        // "the model used a tag that does not exist" is a real and actionable
        // explanation for a failed document.
        tracing::warn!(response_len = text.len(), error = %e, "model response failed validation");
        Error::Invalid(format!("the model returned unusable output: {e}"))
    })
}

/// Find the outermost `{...}` in a blob of text.
///
/// Brace-matching rather than a regex, and string-aware, because an
/// `explanation` field containing a `}` inside a quoted string would truncate a
/// naive last-`}` search and produce a parse error that looks like the model's
/// fault.
fn extract_json_object(text: &str) -> Option<&str> {
    let start = text.find('{')?;

    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;

    for (i, c) in text[start..].char_indices() {
        if in_string {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }

        match c {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&text[start..start + i + c.len_utf8()]);
                }
            }
            _ => {}
        }
    }

    None
}

/// Everything the type system could not enforce.
///
/// The vocabulary, the answer letter and the format are already guaranteed by
/// deserialization — there is deliberately no code here re-checking them,
/// because a second copy of a rule is a second place for it to be wrong. What
/// remains is arity and coherence.
fn validate(quiz: &GeneratedQuiz) -> Result<()> {
    if quiz.questions.len() != QUESTIONS_PER_DOC {
        return Err(Error::Invalid(format!(
            "the model returned {} questions, expected {QUESTIONS_PER_DOC}",
            quiz.questions.len()
        )));
    }

    let mut seen_ids = std::collections::HashSet::new();

    for (i, q) in quiz.questions.iter().enumerate() {
        let n = i + 1;

        // Duplicate ids would silently collapse when the browser keys its list
        // by id, and would make a submitted answer ambiguous at grading time.
        if !seen_ids.insert(q.id.as_str()) {
            return Err(Error::Invalid(format!(
                "the model returned two questions with id {:?}",
                q.id
            )));
        }

        if q.options.len() != OPTIONS_PER_QUESTION {
            return Err(Error::Invalid(format!(
                "question {n} has {} options, expected {OPTIONS_PER_QUESTION}",
                q.options.len()
            )));
        }

        // `Choice` guarantees a-d; this guarantees a-d indexes something. The
        // two together are what make `Choice::index` total.
        if q.answer.index() >= q.options.len() {
            return Err(Error::Invalid(format!(
                "question {n} answers with an option it does not have"
            )));
        }

        if q.prompt.trim().is_empty() {
            return Err(Error::Invalid(format!("question {n} has an empty prompt")));
        }

        if q.options.iter().any(|o| o.trim().is_empty()) {
            return Err(Error::Invalid(format!("question {n} has an empty option")));
        }

        // Duplicate options make the question unanswerable — two identical
        // choices where one is keyed correct and the other is not.
        let unique: std::collections::HashSet<&str> = q.options.iter().map(|o| o.trim()).collect();
        if unique.len() != q.options.len() {
            return Err(Error::Invalid(format!("question {n} repeats an option")));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quiz_json(questions: usize, options: usize) -> String {
        let qs: Vec<String> = (1..=questions)
            .map(|i| {
                let opts: Vec<String> = (0..options)
                    .map(|j| format!("\"option {i}-{j}\""))
                    .collect();
                format!(
                    r#"{{"id":"q{i}","format":"multiple_choice","skill":"causal",
                        "prompt":"why {i}?","options":[{}],"answer":"a",
                        "explanation":"page {i}"}}"#,
                    opts.join(",")
                )
            })
            .collect();

        format!(r#"{{"questions":[{}]}}"#, qs.join(","))
    }

    #[test]
    fn a_well_formed_quiz_is_accepted() {
        let quiz = parse(&quiz_json(10, 4)).expect("parses");
        validate(&quiz).expect("validates");
    }

    #[test]
    fn markdown_fences_are_tolerated() {
        let wrapped = format!("Here you go:\n```json\n{}\n```\n", quiz_json(10, 4));
        let quiz = parse(&wrapped).expect("fenced JSON still parses");
        assert_eq!(quiz.questions.len(), 10);
    }

    #[test]
    fn braces_inside_strings_do_not_truncate_the_object() {
        let json = r#"{"note":"a } brace","questions":[]}"#;
        assert_eq!(
            extract_json_object(&format!("text {json} trailer")),
            Some(json)
        );
    }

    #[test]
    fn wrong_question_count_is_rejected() {
        let quiz = parse(&quiz_json(9, 4)).expect("parses");
        assert!(matches!(validate(&quiz), Err(Error::Invalid(_))));
    }

    #[test]
    fn wrong_option_count_is_rejected() {
        let quiz = parse(&quiz_json(10, 3)).expect("parses");
        assert!(matches!(validate(&quiz), Err(Error::Invalid(_))));
    }

    /// The vocabulary check, which lives in the deserializer rather than in
    /// `validate`. This is the case the closed enums exist for.
    #[test]
    fn an_invented_skill_is_rejected_at_parse_time() {
        let json = quiz_json(10, 4).replace("\"causal\"", "\"macroeconomic\"");
        assert!(matches!(parse(&json), Err(Error::Invalid(_))));
    }

    #[test]
    fn an_answer_outside_a_to_d_is_rejected_at_parse_time() {
        let json = quiz_json(10, 4).replace("\"answer\":\"a\"", "\"answer\":\"e\"");
        assert!(matches!(parse(&json), Err(Error::Invalid(_))));
    }

    #[test]
    fn duplicate_options_are_rejected() {
        let json = quiz_json(10, 4).replace("\"option 1-1\"", "\"option 1-0\"");
        let quiz = parse(&json).expect("parses");
        assert!(matches!(validate(&quiz), Err(Error::Invalid(_))));
    }

    /// The prompt must always name every value the deserializer accepts.
    #[test]
    fn the_prompt_lists_the_whole_vocabulary() {
        let p = system_prompt();
        for s in Skill::ALL {
            assert!(p.contains(s.as_str()), "prompt omits skill {s}");
        }
    }
}
