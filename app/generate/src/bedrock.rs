//! The model call, and the validation that decides whether to trust it.
//!
//! Three rules govern this file:
//!
//! 1. **The model's output is untrusted input.** It is validated exactly as
//!    strictly as a request body from the internet would be. The JSON Schema
//!    below is a constraint on the model, not a guarantee about it — Bedrock
//!    does not reject a tool call that violates the schema it was given, and a
//!    Nova run against this very schema returned a question with duplicate
//!    options. Every rule the schema states is therefore *also* checked here.
//! 2. **The full response is never logged.** It contains the answer key. A log
//!    group is not as private as a DynamoDB item, it is retained on a different
//!    schedule, and "the answers are in CloudWatch" is the same bug as "the
//!    answers are in the quiz payload" with more steps.
//! 3. **Nothing here trusts the model's sense of position.** See
//!    [`shuffle_options`].
//!
//! # Why tool use rather than "return JSON"
//!
//! Asking for bare JSON in a prompt and parsing the reply worked, in the sense
//! that it produced output. On the reference document it produced nine
//! questions instead of ten, and all nine had five options with a duplicated
//! id. Every one would have been discarded. Handing the model a JSON Schema as
//! a tool and letting it fill that in produced ten well-formed questions on the
//! first attempt, from the same model, on the same document.
//!
//! # Why the tool is not forced
//!
//! `toolChoice: {tool: ...}` would guarantee the model calls the tool. It is
//! also refused outright when thinking is enabled — *"Thinking may not be
//! enabled when tool_choice forces tool use"* — and thinking is what produces
//! questions anchored to the document rather than answerable from general
//! knowledge. So the choice is left automatic, the instruction to call the tool
//! is explicit, and a response with no tool call is treated as a failed
//! generation. In practice the model calls it; the handling exists because
//! "in practice" is not a guarantee.

use aws_sdk_bedrockruntime::primitives::Blob;
use aws_sdk_bedrockruntime::types::{
    ContentBlock, ConversationRole, ConverseOutput, DocumentBlock, DocumentFormat, DocumentSource,
    InferenceConfiguration, Message, SystemContentBlock, Tool, ToolConfiguration, ToolInputSchema,
    ToolSpecification,
};
use aws_sdk_bedrockruntime::Client;
use aws_smithy_types::{Document, Number};
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use trainer_core::error::{aws, Error, Result};
use trainer_core::model::Question;
use trainer_core::tags::{
    self, Choice, Skill, Topic, MAX_TOPICS_PER_DOC, TOPIC_MAX_LEN, TOPIC_MIN_LEN,
};

/// How many questions a quiz has. Not configurable: it is baked into the
/// schema, the validation, and the reader's expectations, and a quiz whose
/// length varies run to run makes scores incomparable across documents.
pub const QUESTIONS_PER_DOC: usize = 10;

/// Exactly four options, because [`Choice`] has exactly four variants. These
/// two numbers are not independent — see `Choice::index`.
pub const OPTIONS_PER_QUESTION: usize = 4;

/// The name the model must call. Referenced from the instruction text too, so
/// they cannot drift.
const TOOL_NAME: &str = "emit_quiz";

/// Bounds shared by the schema and by [`validate`].
///
/// Declared once because the two must agree. A schema that permits a 500
/// character prompt while validation rejects it at 300 does not produce a
/// clear error — it produces a paid-for generation that is thrown away, with a
/// message blaming the model for obeying its instructions.
mod limits {
    pub const TITLE_MIN: usize = 4;
    pub const TITLE_MAX: usize = 120;
    pub const PROMPT_MIN: usize = 15;
    pub const PROMPT_MAX: usize = 300;
    pub const OPTION_MIN: usize = 1;
    pub const OPTION_MAX: usize = 200;
    pub const EXPLANATION_MIN: usize = 15;
    pub const EXPLANATION_MAX: usize = 400;
}

/// What the model is asked to return.
///
/// `questions` deserializes into the *same* [`Question`] type the rest of the
/// app uses, so there is no separate "model shape" that could drift from the
/// stored shape. The closed enums do the vocabulary enforcement for `skill` and
/// `answer`: a value outside the list is a serde error here, before any of the
/// checks below run. `format` is not asked for at all and defaults — see
/// `Question::format`.
///
/// `topics` is the exception, and is `Vec<String>` rather than `Vec<Topic>` on
/// purpose. The topic vocabulary is open, so there is no enum to reject
/// against; what there is instead is `tags::normalise`, which splits phrases
/// into words and drops what it cannot use. Deserializing straight into
/// `Vec<Topic>` would fail the whole document over a hyphen.
#[derive(Debug, Deserialize)]
struct GeneratedQuiz {
    title: String,
    topics: Vec<String>,
    questions: Vec<Question>,
}

/// The JSON Schema handed to the model as a tool.
///
/// Built from the same constants and enums the validator uses, rather than
/// written out, so the two cannot disagree. That specific drift — schema says
/// one word, enum expects another — produces a 100% failure rate with an error
/// that reads like the model is misbehaving.
///
/// `known` is every topic used so far. It is embedded in the description rather
/// than as an `enum`, because an `enum` would close the vocabulary again and a
/// closed list is what made a housing report get tagged `energy`.
fn quiz_schema(known: &[Topic]) -> serde_json::Value {
    let skills: Vec<&str> = Skill::ALL.iter().map(|s| s.as_str()).collect();
    let choices: Vec<&str> = Choice::ALL.iter().map(|c| c.as_str()).collect();
    let known: Vec<&str> = known.iter().map(|t| t.as_str()).collect();

    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["title", "topics", "questions"],
        "properties": {
            "title": {
                "type": "string",
                "minLength": limits::TITLE_MIN,
                "maxLength": limits::TITLE_MAX,
                "description":
                    "The document's own title as printed on it, not a description you compose. \
                     Include a subtitle only if the document has one and it is short."
            },
            "topics": {
                "type": "array",
                "minItems": 1,
                "maxItems": MAX_TOPICS_PER_DOC,
                "uniqueItems": true,
                "items": {
                    "type": "string",
                    "pattern": "^[a-z]+$",
                    "minLength": TOPIC_MIN_LEN,
                    "maxLength": TOPIC_MAX_LEN
                },
                "description": format!(
                    "Subject matter, as single lowercase words. NEVER a phrase or a compound: \
                     a report on energy policy is tagged [\"energy\", \"policy\"], not \
                     [\"energy_policy\"]. Reuse these existing tags wherever one fits, so that \
                     documents about the same subject are comparable: {}. Coin a new word only \
                     when nothing in that list fits.",
                    known.join(", ")
                )
            },
            "questions": {
                "type": "array",
                "minItems": QUESTIONS_PER_DOC,
                "maxItems": QUESTIONS_PER_DOC,
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    // No `format`. It has exactly one legal value, so asking
                    // for it cannot add information and can only be got wrong —
                    // and was: Sonnet returned `"format": "definitional"`,
                    // putting a skill in the format field, and ten otherwise
                    // good questions were discarded. The handler fills it in.
                    "required": [
                        "id", "skill", "prompt", "options", "answer", "explanation"
                    ],
                    "properties": {
                        "id": {
                            "type": "string",
                            "pattern": "^q([1-9]|10)$",
                            "description": "q1 through q10, each used once."
                        },
                        "skill": {
                            "type": "string",
                            "enum": skills,
                            "description":
                                "Spread the ten questions across all five skills. A quiz that \
                                 leaves a skill unused cannot measure it."
                        },
                        "prompt": {
                            "type": "string",
                            "minLength": limits::PROMPT_MIN,
                            "maxLength": limits::PROMPT_MAX
                        },
                        "options": {
                            "type": "array",
                            "minItems": OPTIONS_PER_QUESTION,
                            "maxItems": OPTIONS_PER_QUESTION,
                            "uniqueItems": true,
                            "items": {
                                "type": "string",
                                "minLength": limits::OPTION_MIN,
                                "maxLength": limits::OPTION_MAX
                            },
                            "description":
                                "Four options. The three wrong ones must be plausible and drawn \
                                 from the document — not obviously absurd, and not different in \
                                 length or specificity from the correct one."
                        },
                        "answer": {
                            "type": "string",
                            "enum": choices,
                            "description":
                                "The letter of the correct option. Position is randomised after \
                                 you answer, so do not try to vary it."
                        },
                        "explanation": {
                            "type": "string",
                            "minLength": limits::EXPLANATION_MIN,
                            "maxLength": limits::EXPLANATION_MAX,
                            "description":
                                "Where in the document the answer comes from — name the table, \
                                 chart or section."
                        }
                    }
                }
            }
        }
    })
}

/// System prompt. Deliberately short: the schema carries the structure, so this
/// carries only what a schema cannot express.
fn system_prompt() -> String {
    format!(
        "You generate reading-comprehension quizzes from documents.\n\
         \n\
         Return your result by calling the `{TOOL_NAME}` tool. Do not answer in prose.\n\
         \n\
         The single thing that makes a quiz here worth taking: every question must be \
         answerable ONLY by someone who has read this specific document. A question that \
         a well-read person could answer without opening it — a definition of a standard \
         term, a fact about how markets generally work — is a failure, however well \
         formed. Prefer questions that turn on this document's own figures, its specific \
         comparisons, and the claims it actually makes.\n\
         \n\
         Wrong options must be drawn from the document too. An option that is obviously \
         absurd, or noticeably longer and more qualified than the others, gives the answer \
         away without any reading at all."
    )
}

/// Everything the model is told about *this* run.
pub struct Request<'a> {
    pub model_id: &'a str,
    /// Zero disables thinking. Non-zero enables it with this budget, and the
    /// output cap is raised above it — a budget at or above `max_tokens` leaves
    /// no room for the answer and the call fails.
    pub thinking_budget_tokens: u32,
    /// Topics used so far, offered back for reuse.
    pub known_topics: &'a [Topic],
    /// Seeds option shuffling. The document id, so a regeneration of the same
    /// document is reproducible while different documents differ.
    pub seed: &'a str,
    pub pdf: Vec<u8>,
}

/// What a successful generation produced.
pub struct Generated {
    pub title: String,
    pub topics: Vec<Topic>,
    pub questions: Vec<Question>,
}

/// Send the PDF to Bedrock as a Converse `document` block and parse the result.
///
/// The PDF goes in as bytes, not as text this function extracted. That is the
/// verified-working path: the model reads the document's own structure —
/// tables, figures, headings — which is what makes `figure_recall` a meaningful
/// skill tag. Pre-extracting to plain text would flatten exactly the structure
/// several of the skills are about.
pub async fn generate(client: &Client, req: Request<'_>) -> Result<Generated> {
    let document = DocumentBlock::builder()
        .format(DocumentFormat::Pdf)
        // Bedrock restricts this to alphanumerics, single spaces, hyphens,
        // parentheses and brackets, and AWS explicitly warns it is a prompt
        // injection vector. It is a constant for that reason — and now also
        // because the only name available would be the uploader's filename,
        // and the model is supposed to derive the title from the document
        // itself rather than be told what to think it is.
        .name("uploaded document")
        .source(DocumentSource::Bytes(Blob::new(req.pdf)))
        .build()
        .map_err(|e| Error::Aws(format!("building document block: {e}")))?;

    let message = Message::builder()
        .role(ConversationRole::User)
        .content(ContentBlock::Document(document))
        .content(ContentBlock::Text(format!(
            "Read this document and call `{TOOL_NAME}` with its title, its topics, and ten \
             questions."
        )))
        .build()
        .map_err(|e| Error::Aws(format!("building message: {e}")))?;

    let tool = Tool::ToolSpec(
        ToolSpecification::builder()
            .name(TOOL_NAME)
            .description(
                "Emit the title, topics and ten questions for the document. This is the only \
                 way to return a result.",
            )
            .input_schema(ToolInputSchema::Json(json_to_document(&quiz_schema(
                req.known_topics,
            ))))
            .build()
            .map_err(|e| Error::Aws(format!("building tool spec: {e}")))?,
    );

    // No `tool_choice`. Forcing it is incompatible with thinking — see the
    // module header.
    let tools = ToolConfiguration::builder()
        .tools(tool)
        .build()
        .map_err(|e| Error::Aws(format!("building tool config: {e}")))?;

    let thinking = req.thinking_budget_tokens > 0;

    let mut inference = InferenceConfiguration::builder()
        // Ten questions with four options and an explanation each runs to
        // roughly 2k output tokens; the thinking budget is spent on top of
        // that. The cap exists so a model that starts looping cannot run up a
        // bill.
        .max_tokens((req.thinking_budget_tokens + 4096) as i32);

    if !thinking {
        // Low, not zero: a little variance keeps a retry of a failed
        // generation from reproducing the same malformed output. Omitted
        // entirely when thinking is on, because the two cannot both be set —
        // reasoning requires the default sampling temperature.
        inference = inference.temperature(0.2);
    }

    let mut call = client
        .converse()
        .model_id(req.model_id)
        .system(SystemContentBlock::Text(system_prompt()))
        .messages(message)
        .tool_config(tools)
        .inference_config(inference.build());

    if thinking {
        call = call.additional_model_request_fields(json_to_document(&json!({
            "reasoning_config": {
                "type": "enabled",
                "budget_tokens": req.thinking_budget_tokens,
            }
        })));
    }

    let out = call.send().await.map_err(aws)?;

    let quiz = parse(extract_tool_input(out.output)?)?;

    let title = quiz.title.trim().to_string();
    let topics = tags::normalise(&quiz.topics);
    let mut questions = quiz.questions;

    validate(&title, &topics, &questions)?;
    shuffle_options(&mut questions, req.seed);

    Ok(Generated {
        title,
        topics,
        questions,
    })
}

/// Pull the tool call's arguments out of the response.
///
/// Reasoning blocks are skipped rather than concatenated: with thinking on, the
/// message contains a `reasoningContent` block before the `toolUse` one, and it
/// is not JSON.
fn extract_tool_input(output: Option<ConverseOutput>) -> Result<serde_json::Value> {
    let Some(ConverseOutput::Message(message)) = output else {
        return Err(Error::Invalid(
            "the model returned no message; try again".into(),
        ));
    };

    for block in message.content {
        if let ContentBlock::ToolUse(call) = block {
            if call.name() != TOOL_NAME {
                // A second tool it was never given. Refusing is not pedantry:
                // whatever it contains is not a quiz.
                tracing::warn!(tool = %call.name(), "model called an unexpected tool");
                continue;
            }
            return Ok(document_to_json(call.input()));
        }
    }

    // Reached when the model answered in prose instead of calling the tool.
    // Retryable — `record_outcome` maps `Invalid` to a terminal failure, so
    // this is deliberately worded for someone reading it next to a Retry
    // button.
    Err(Error::Invalid(
        "the model replied without generating a quiz; try again".into(),
    ))
}

/// Deserialize the tool arguments into the quiz.
///
/// Note what is *not* logged on failure. The response contains the answer key,
/// so an error logs the serde message — which names a path and a position, not
/// content — and nothing else.
fn parse(input: serde_json::Value) -> Result<GeneratedQuiz> {
    serde_json::from_value(input).map_err(|e| {
        // `e` here can name an unknown enum variant, which is the closed
        // vocabulary firing. That is worth surfacing to the reader, because
        // "the model used a skill that does not exist" is a real and
        // actionable explanation for a failed document.
        tracing::warn!(error = %e, "model tool call failed validation");
        Error::Invalid(format!("the model returned unusable output: {e}"))
    })
}

/// Everything the schema asked for, checked again.
///
/// The schema is an instruction, not an enforcement point — Bedrock passes a
/// non-conforming tool call straight through, which a Nova run against this
/// exact schema demonstrated by returning duplicate options. So every bound
/// stated in `quiz_schema` is restated here, against the same constants.
///
/// The vocabulary, the answer letter and the format are guaranteed by
/// deserialization instead, and there is deliberately no code here re-checking
/// them: a second copy of a rule is a second place for it to be wrong.
fn validate(title: &str, topics: &[Topic], questions: &[Question]) -> Result<()> {
    let title_len = title.chars().count();
    if !(limits::TITLE_MIN..=limits::TITLE_MAX).contains(&title_len) {
        return Err(Error::Invalid(format!(
            "the model returned a {title_len}-character title"
        )));
    }

    // Empty is the case that matters. `normalise` drops what it cannot use, so
    // a model that returned only compounds and connectives arrives here with
    // nothing — and a document with no topics is invisible to every segment of
    // the history view, which is the feature the tags exist for.
    if topics.is_empty() {
        return Err(Error::Invalid(
            "the model returned no usable topics; try again".into(),
        ));
    }

    if questions.len() != QUESTIONS_PER_DOC {
        return Err(Error::Invalid(format!(
            "the model returned {} questions, expected {QUESTIONS_PER_DOC}",
            questions.len()
        )));
    }

    let mut seen_ids = std::collections::HashSet::new();

    for (i, q) in questions.iter().enumerate() {
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
        // two together are what make `Choice::index` total, and what makes the
        // shuffle below safe.
        if q.answer.index() >= q.options.len() {
            return Err(Error::Invalid(format!(
                "question {n} answers with an option it does not have"
            )));
        }

        let prompt_len = q.prompt.trim().chars().count();
        if !(limits::PROMPT_MIN..=limits::PROMPT_MAX).contains(&prompt_len) {
            return Err(Error::Invalid(format!(
                "question {n} has a {prompt_len}-character prompt"
            )));
        }

        let explanation_len = q.explanation.trim().chars().count();
        if !(limits::EXPLANATION_MIN..=limits::EXPLANATION_MAX).contains(&explanation_len) {
            return Err(Error::Invalid(format!(
                "question {n} has a {explanation_len}-character explanation"
            )));
        }

        for option in &q.options {
            let len = option.trim().chars().count();
            if !(limits::OPTION_MIN..=limits::OPTION_MAX).contains(&len) {
                return Err(Error::Invalid(format!(
                    "question {n} has a {len}-character option"
                )));
            }
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

/// Randomise which letter is correct.
///
/// **Not cosmetic.** Measured on one document: Sonnet put the answer at `b` in
/// nine questions out of ten and never once used `d`; Nova put six of ten at
/// `c`. A reader who notices that — and a reader taking their own quizzes will
/// — can score well above their real comprehension by guessing `b`, which makes
/// every number in the history view an overestimate. No prompt fixes this
/// reliably, because the model is not choosing a position, it is exhibiting a
/// bias it cannot introspect on. Moving the answer after the fact does.
///
/// Seeded from the document id rather than the clock, so regenerating a
/// document produces the same arrangement and a bug here is reproducible.
fn shuffle_options(questions: &mut [Question], seed: &str) {
    let mut state = fnv1a(seed);

    for q in questions.iter_mut() {
        let mut order: Vec<usize> = (0..q.options.len()).collect();

        // Fisher-Yates, which is uniform. Repeatedly swapping random pairs is
        // the version that looks equivalent and is not.
        for i in (1..order.len()).rev() {
            let j = (next_u64(&mut state) % (i as u64 + 1)) as usize;
            order.swap(i, j);
        }

        let was_correct = q.answer.index();
        q.options = order.iter().map(|&i| q.options[i].clone()).collect();

        // `order` is a permutation of every index, so the old correct index is
        // in it exactly once. Handled rather than unwrapped because a panic
        // here would surface as an invocation error with no failed-status row
        // to explain it.
        let now_correct = order
            .iter()
            .position(|&i| i == was_correct)
            .and_then(|pos| Choice::ALL.get(pos).copied());

        if let Some(choice) = now_correct {
            q.answer = choice;
        }
    }
}

/// FNV-1a. Any stable hash would do; this one is four lines and needs no
/// dependency.
fn fnv1a(s: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in s.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    // xorshift64* requires a non-zero state, and FNV of the empty string is
    // non-zero, so this only guards a hash that happens to land on zero.
    if hash == 0 {
        0x9e37_79b9_7f4a_7c15
    } else {
        hash
    }
}

/// xorshift64*. Not cryptographic and does not need to be — it decides where a
/// quiz answer sits, and the quiz is taken by the person who generated it.
fn next_u64(state: &mut u64) -> u64 {
    *state ^= *state >> 12;
    *state ^= *state << 25;
    *state ^= *state >> 27;
    state.wrapping_mul(0x2545_F491_4F6C_DD1D)
}

/// `serde_json::Value` to the Smithy `Document` the SDK wants.
///
/// The schema is far more readable as a `json!` literal than as nested
/// `Document::Object(HashMap::from([...]))`, and the SDK offers no conversion
/// without opting into serde features on `aws-smithy-types`.
fn json_to_document(value: &serde_json::Value) -> Document {
    match value {
        serde_json::Value::Null => Document::Null,
        serde_json::Value::Bool(b) => Document::Bool(*b),
        serde_json::Value::String(s) => Document::String(s.clone()),
        serde_json::Value::Number(n) => {
            if let Some(u) = n.as_u64() {
                Document::Number(Number::PosInt(u))
            } else if let Some(i) = n.as_i64() {
                Document::Number(Number::NegInt(i))
            } else {
                // `as_f64` is `None` only for a number that is neither integral
                // nor floating, which serde_json cannot represent.
                Document::Number(Number::Float(n.as_f64().unwrap_or_default()))
            }
        }
        serde_json::Value::Array(items) => {
            Document::Array(items.iter().map(json_to_document).collect())
        }
        serde_json::Value::Object(fields) => Document::Object(
            fields
                .iter()
                .map(|(k, v)| (k.clone(), json_to_document(v)))
                .collect::<HashMap<_, _>>(),
        ),
    }
}

/// The inverse, for reading the tool call's arguments back.
fn document_to_json(document: &Document) -> serde_json::Value {
    match document {
        Document::Null => serde_json::Value::Null,
        Document::Bool(b) => serde_json::Value::Bool(*b),
        Document::String(s) => serde_json::Value::String(s.clone()),
        Document::Number(n) => match n {
            Number::PosInt(u) => serde_json::Value::from(*u),
            Number::NegInt(i) => serde_json::Value::from(*i),
            Number::Float(f) => serde_json::Number::from_f64(*f)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null),
        },
        Document::Array(items) => {
            serde_json::Value::Array(items.iter().map(document_to_json).collect())
        }
        Document::Object(fields) => serde_json::Value::Object(
            fields
                .iter()
                .map(|(k, v)| (k.clone(), document_to_json(v)))
                .collect(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn question_json(i: usize, options: usize) -> String {
        let opts: Vec<String> = (0..options)
            .map(|j| format!("\"option {i}-{j}\""))
            .collect();
        format!(
            r#"{{"id":"q{i}","skill":"causal",
                "prompt":"why does this document say {i} happened?",
                "options":[{}],"answer":"a",
                "explanation":"stated in the section on {i}, second paragraph"}}"#,
            opts.join(",")
        )
    }

    fn quiz_json(questions: usize, options: usize) -> serde_json::Value {
        let qs: Vec<String> = (1..=questions).map(|i| question_json(i, options)).collect();
        serde_json::from_str(&format!(
            r#"{{"title":"A Reference Document","topics":["fiscal"],"questions":[{}]}}"#,
            qs.join(",")
        ))
        .expect("test fixture is valid json")
    }

    fn parsed(questions: usize, options: usize) -> (String, Vec<Topic>, Vec<Question>) {
        let quiz = parse(quiz_json(questions, options)).expect("parses");
        let topics = tags::normalise(&quiz.topics);
        (quiz.title, topics, quiz.questions)
    }

    #[test]
    fn a_well_formed_quiz_is_accepted() {
        let (title, topics, questions) = parsed(10, 4);
        validate(&title, &topics, &questions).expect("validates");
    }

    #[test]
    fn wrong_question_count_is_rejected() {
        let (title, topics, questions) = parsed(9, 4);
        assert!(matches!(
            validate(&title, &topics, &questions),
            Err(Error::Invalid(_))
        ));
    }

    #[test]
    fn wrong_option_count_is_rejected() {
        let (title, topics, questions) = parsed(10, 3);
        assert!(matches!(
            validate(&title, &topics, &questions),
            Err(Error::Invalid(_))
        ));
    }

    /// The bound the schema states and the bound the validator enforces have to
    /// be the same number. They are only the same number because both read
    /// `limits::PROMPT_MAX`; this test fails if one is ever hardcoded.
    #[test]
    fn an_over_long_prompt_is_rejected() {
        let (title, topics, mut questions) = parsed(10, 4);
        questions[3].prompt = "x".repeat(limits::PROMPT_MAX + 1);
        assert!(matches!(
            validate(&title, &topics, &questions),
            Err(Error::Invalid(_))
        ));
    }

    #[test]
    fn a_document_with_no_usable_topics_is_rejected() {
        // Every tag a compound or a connective, so `normalise` yields nothing.
        let (title, _, questions) = parsed(10, 4);
        let topics = tags::normalise(["and", "of"]);
        assert!(topics.is_empty(), "fixture assumption");
        assert!(matches!(
            validate(&title, &topics, &questions),
            Err(Error::Invalid(_))
        ));
    }

    /// The vocabulary check, which lives in the deserializer rather than in
    /// `validate`. This is the case the closed enums exist for.
    #[test]
    fn an_invented_skill_is_rejected_at_parse_time() {
        let mut quiz = quiz_json(10, 4);
        quiz["questions"][0]["skill"] = json!("macroeconomic");
        assert!(matches!(parse(quiz), Err(Error::Invalid(_))));
    }

    #[test]
    fn an_answer_outside_a_to_d_is_rejected_at_parse_time() {
        let mut quiz = quiz_json(10, 4);
        quiz["questions"][0]["answer"] = json!("e");
        assert!(matches!(parse(quiz), Err(Error::Invalid(_))));
    }

    #[test]
    fn duplicate_options_are_rejected() {
        let mut quiz = quiz_json(10, 4);
        quiz["questions"][0]["options"][1] = quiz["questions"][0]["options"][0].clone();
        let quiz = parse(quiz).expect("parses");
        let topics = tags::normalise(&quiz.topics);
        assert!(matches!(
            validate(&quiz.title, &topics, &quiz.questions),
            Err(Error::Invalid(_))
        ));
    }

    /// The property the shuffle exists for: the correct *text* must survive,
    /// even though the correct *letter* changes.
    #[test]
    fn shuffling_moves_the_letter_but_not_the_answer() {
        let (_, _, mut questions) = parsed(10, 4);

        let before: Vec<String> = questions
            .iter()
            .map(|q| q.options[q.answer.index()].clone())
            .collect();

        shuffle_options(&mut questions, "doc-under-test");

        for (q, expected) in questions.iter().zip(&before) {
            assert_eq!(
                &q.options[q.answer.index()],
                expected,
                "the answer key must still point at the same text"
            );
            assert_eq!(q.options.len(), OPTIONS_PER_QUESTION, "nothing lost");
        }
    }

    #[test]
    fn shuffling_actually_moves_something() {
        // Every fixture question keys "a". If the shuffle were a no-op this
        // test would pass silently against a broken implementation, so assert
        // the distribution changed rather than that it is uniform.
        let (_, _, mut questions) = parsed(10, 4);
        shuffle_options(&mut questions, "doc-under-test");
        assert!(
            questions.iter().any(|q| q.answer != Choice::A),
            "shuffle left every answer at 'a'"
        );
    }

    #[test]
    fn shuffling_is_reproducible_for_a_document() {
        let (_, _, mut first) = parsed(10, 4);
        let (_, _, mut second) = parsed(10, 4);
        shuffle_options(&mut first, "same-doc");
        shuffle_options(&mut second, "same-doc");

        let keys = |qs: &[Question]| qs.iter().map(|q| q.answer).collect::<Vec<_>>();
        assert_eq!(keys(&first), keys(&second));
    }

    /// The schema is generated, so it can silently stop mentioning a skill the
    /// deserializer still accepts. That drift is exactly what produced a 100%
    /// failure rate under the old prose prompt.
    #[test]
    fn the_schema_lists_the_whole_closed_vocabulary() {
        let schema = quiz_schema(&[]).to_string();
        for skill in Skill::ALL {
            assert!(
                schema.contains(skill.as_str()),
                "schema omits skill {skill}"
            );
        }
        for choice in Choice::ALL {
            assert!(schema.contains(choice.as_str()));
        }
    }

    #[test]
    fn the_schema_offers_known_topics_for_reuse() {
        let known: Vec<Topic> = ["housing", "monetary"]
            .iter()
            .filter_map(|w| Topic::parse(w))
            .collect();
        let schema = quiz_schema(&known).to_string();
        assert!(schema.contains("housing"));
        assert!(schema.contains("monetary"));
    }

    /// Round-tripping is what carries the schema to Bedrock and the arguments
    /// back. A bug in either direction would show up as a validation failure
    /// blamed on the model.
    #[test]
    fn documents_round_trip_through_json() {
        let original = quiz_schema(&[]);
        let round_tripped = document_to_json(&json_to_document(&original));
        assert_eq!(original, round_tripped);
    }
}
