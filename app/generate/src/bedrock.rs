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
//! # The rule that cannot be checked here, and cost the most
//!
//! Every constraint below is restated in [`validate`] because a schema is an
//! instruction rather than an enforcement point. One is not, and cannot be:
//! **exactly one option may be a defensible answer to the prompt.** Deciding
//! that needs the document, so it lives only in the wording handed to the
//! model.
//!
//! It is worth knowing what it looks like when it fails, because the failure is
//! invisible from the data. Asked why Canada's labour market was tightening
//! while GDP contracted, a generation offered both of the report's own
//! explanations — the inventory drawdown that hid resilient domestic demand,
//! and the shrinking labour force from immigration policy. One was keyed
//! correct. The other was almost verbatim from the report's Highlights. The
//! reader picked it at 99% confidence, because they had read the Highlights,
//! and lost the maximum penalty the scoring rule can impose.
//!
//! Note where that came from: the instruction to draw wrong options *from the
//! document*, which exists to stop invented statistics being taught as real.
//! That rule is right and it is what makes this one necessary — a true sentence
//! lifted from another paragraph reads as a distractor and argues as an answer.
//! Drawn from the document has to mean **mis-bound**, not merely true.
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
use trainer_core::model::{Question, QuestionBody};
use trainer_core::numeric::NumericAnswer;
use trainer_core::tags::{
    self, Choice, Shelf, Skill, Topic, MAX_TOPICS_PER_DOC, TOPIC_MAX_LEN, TOPIC_MIN_LEN,
};

/// How many questions a quiz has. Not configurable: it is baked into the
/// schema, the validation, and the reader's expectations, and a quiz whose
/// length varies run to run makes scores incomparable across documents.
pub const QUESTIONS_PER_DOC: usize = 10;

/// How many of those are typed figures rather than four options.
///
/// # Why the mix is fixed rather than left to the model
///
/// Because it is the mix, not the wording, that decides what the quiz trains.
/// Asked to write ten questions about a document full of tables, a model writes
/// mostly figure-recall questions — and a figure-recall question in
/// multiple-choice form needs three wrong figures, which the model invents.
/// Reading three invented statistics and deliberating over them is how they get
/// remembered; the intrusions persist, and by reconstruction rather than
/// familiarity, so knowing one of them was wrong does not undo it. For an app
/// whose whole purpose is having accurate numbers to hand, that is the worst
/// available failure.
///
/// Splitting the request into two arrays fixes it structurally. Figures are
/// asked for as figures, with a tolerance, and there are no wrong ones to read.
/// The other seven are about causes, relationships and scope, where the wrong
/// options are claims from the document rather than fabricated quantities.
///
/// Three is a floor on numeric practice and a ceiling on trivia: enough that
/// every document leaves a few recallable anchors, few enough that the quiz is
/// mostly about what the document *argues*.
pub const NUMERIC_QUESTIONS_PER_DOC: usize = 3;

/// The remainder, asked as multiple choice.
pub const CHOICE_QUESTIONS_PER_DOC: usize = QUESTIONS_PER_DOC - NUMERIC_QUESTIONS_PER_DOC;

/// Exactly four options, because [`Choice`] has exactly four variants. These
/// two numbers are not independent — see `Choice::index`.
pub const OPTIONS_PER_QUESTION: usize = 4;

/// The fewest distinct skills the seven multiple-choice questions must cover.
///
/// A quiz where all seven are `definitional` measures one thing and reports it
/// as a document score, and the skill × topic matrix — the entire reason
/// questions are tagged — collapses to a single row. Three of the four is loose
/// enough that a document genuinely light on causal claims still generates, and
/// tight enough to catch a model that has settled into one groove.
const MIN_DISTINCT_CHOICE_SKILLS: usize = 3;

/// The skill every numeric question has.
///
/// Not asked for. A typed figure *is* figure recall, so offering the model a
/// choice would only let it be wrong — the same reasoning that removed
/// `format` from the schema. The consequence worth stating: from now on
/// `figure_recall` and `numeric` are the same segment of the history matrix,
/// and a `figure_recall` rate from before this change is a rate on a different
/// task, where guessing paid 25%.
const NUMERIC_SKILL: Skill = Skill::FigureRecall;

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
/// **Two arrays, not one array with a discriminator.** A single array of
/// polymorphic questions would need `oneOf` in the schema, which models honour
/// unevenly, and a runtime check that each item's shape matches its declared
/// kind. Two arrays make the same constraint structural: the count of each is a
/// `minItems`/`maxItems` pair, the shapes cannot be confused because they are
/// different schemas, and the handler knows which format it is holding without
/// asking. It is also why `format` is still not a field the model fills in.
///
/// `topics` is `Vec<String>` rather than `Vec<Topic>` on purpose. The topic
/// vocabulary is open, so there is no enum to reject against; what there is
/// instead is `tags::normalise`, which splits phrases into words and drops what
/// it cannot use. Deserializing straight into `Vec<Topic>` would fail the whole
/// document over a hyphen.
#[derive(Debug, Deserialize)]
struct GeneratedQuiz {
    title: String,
    topics: Vec<String>,
    choice_questions: Vec<ChoiceQuestion>,
    numeric_questions: Vec<NumericQuestion>,
}

/// A multiple-choice question as the model returns it.
///
/// The closed enums do the vocabulary enforcement: a `skill` or `answer`
/// outside the list is a serde error here, before any of the checks below run.
#[derive(Debug, Deserialize)]
struct ChoiceQuestion {
    id: String,
    skill: Skill,
    shelf: Shelf,
    prompt: String,
    options: Vec<String>,
    answer: Choice,
    explanation: String,
}

/// A typed-figure question as the model returns it.
///
/// No `skill` field — every one of these is [`NUMERIC_SKILL`]. No `options`,
/// no `answer` letter. The flat `value`/`tolerance`/`unit` triple is assembled
/// into a `NumericAnswer` and validated by it.
#[derive(Debug, Deserialize)]
struct NumericQuestion {
    id: String,
    shelf: Shelf,
    prompt: String,
    value: f64,
    tolerance: f64,
    #[serde(default)]
    unit: String,
    explanation: String,
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
    // Figure recall is deliberately absent from the multiple-choice list. It is
    // what the numeric array is for, and offering it here is what produces a
    // question with three invented statistics in it.
    let choice_skills: Vec<&str> = Skill::ALL
        .iter()
        .filter(|s| **s != NUMERIC_SKILL)
        .map(|s| s.as_str())
        .collect();
    let choices: Vec<&str> = Choice::ALL.iter().map(|c| c.as_str()).collect();
    let shelves: Vec<&str> = Shelf::ALL.iter().map(|s| s.as_str()).collect();

    let known: Vec<&str> = known.iter().map(|t| t.as_str()).collect();

    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["title", "topics", "choice_questions", "numeric_questions"],
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
            "choice_questions": {
                "type": "array",
                "minItems": CHOICE_QUESTIONS_PER_DOC,
                "maxItems": CHOICE_QUESTIONS_PER_DOC,
                "description": format!(
                    "{CHOICE_QUESTIONS_PER_DOC} questions about what the document argues — its \
                     causes, comparisons, definitions and limits. NOT about what its figures \
                     are: those go in numeric_questions, and asking one here would need three \
                     invented statistics as wrong options."
                ),
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    // No `format`. The array a question arrives in determines
                    // it, so asking cannot add information and can only be got
                    // wrong — and was: Sonnet returned
                    // `"format": "definitional"`, putting a skill in the format
                    // field, and ten otherwise good questions were discarded.
                    "required": [
                        "id", "skill", "shelf", "prompt", "options", "answer", "explanation"
                    ],
                    "properties": {
                        "id": {
                            "type": "string",
                            "pattern": "^c[1-9][0-9]?$",
                            "description": format!(
                                "c1 through c{CHOICE_QUESTIONS_PER_DOC}, each used once."
                            )
                        },
                        "skill": {
                            "type": "string",
                            "enum": choice_skills,
                            "description":
                                "Spread these across the listed skills. A quiz that leaves a \
                                 skill unused cannot measure it."
                        },
                        "shelf": {
                            "type": "string",
                            "enum": shelves,
                            "description":
                                "How long this stays worth re-testing months from now. \
                                 `dated` for a forecast, a current condition, or a quarterly \
                                 figure — anything a reader would stop citing once it is \
                                 superseded. `slow` for policy, institutional arrangements and \
                                 multi-year trends. `perennial` only for how a mechanism works, \
                                 or a definition, which does not go out of date. Most questions \
                                 about a forecast document are `dated`; be honest rather than \
                                 generous, because a stale question that keeps being asked \
                                 gets remembered as though it were current."
                        },
                        "prompt": {
                            "type": "string",
                            "minLength": limits::PROMPT_MIN,
                            "maxLength": limits::PROMPT_MAX,
                            "description":
                                "Must name the document and the period it describes, not just \
                                 the fact. \"In this March 2026 outlook, why was the Ontario \
                                 forecast cut?\" — not \"why was the Ontario forecast cut?\". \
                                 These questions come back months later, and a claim rehearsed \
                                 without its date attached stops being remembered as something \
                                 a document said and starts being remembered as something that \
                                 is true."
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
                                "Four options, of which EXACTLY ONE may be a defensible answer \
                                 to the prompt. That is stronger than 'one is correct', and it \
                                 is the rule that breaks. A prompt like 'why is X tightening \
                                 even though Y fell' has two halves; if the document gives one \
                                 reason for X and a different reason for Y, then an option \
                                 stating either reason is defensible and the question has two \
                                 answers. Test every wrong option by asking whether someone who \
                                 had read the document could argue for it. If they could, it is \
                                 not a wrong option.\n\
                                 \n\
                                 The three wrong ones must still come from the document — never \
                                 an invented statistic, because a reader who deliberates over a \
                                 fabricated figure remembers it. But 'from the document' means \
                                 MIS-BOUND, not merely true: a real claim attached to the wrong \
                                 year, the wrong province, or the wrong mechanism. A true \
                                 sentence lifted out of another paragraph is the trap — it looks \
                                 like a distractor and argues like an answer."
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
            },
            "numeric_questions": {
                "type": "array",
                "minItems": NUMERIC_QUESTIONS_PER_DOC,
                "maxItems": NUMERIC_QUESTIONS_PER_DOC,
                "description": format!(
                    "{NUMERIC_QUESTIONS_PER_DOC} figures from the document, answered by typing \
                     a number. Pick the figures worth carrying out of the document — the ones \
                     someone would cite in an argument — not the ones that happen to be easy \
                     to look up."
                ),
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    // No `skill`: every one of these is figure recall by
                    // construction. No `options`, no `answer` letter.
                    "required": [
                        "id", "shelf", "prompt", "value", "tolerance", "unit", "explanation"
                    ],
                    "properties": {
                        "id": {
                            "type": "string",
                            "pattern": "^n[1-9][0-9]?$",
                            "description": format!(
                                "n1 through n{NUMERIC_QUESTIONS_PER_DOC}, each used once."
                            )
                        },
                        "shelf": {
                            "type": "string",
                            "enum": shelves,
                            "description":
                                "How long this stays worth re-testing months from now. \
                                 `dated` for a forecast, a current condition, or a quarterly \
                                 figure — anything a reader would stop citing once it is \
                                 superseded. `slow` for policy, institutional arrangements and \
                                 multi-year trends. `perennial` only for how a mechanism works, \
                                 or a definition, which does not go out of date. Most questions \
                                 about a forecast document are `dated`; be honest rather than \
                                 generous, because a stale question that keeps being asked \
                                 gets remembered as though it were current."
                        },
                        "prompt": {
                            "type": "string",
                            "minLength": limits::PROMPT_MIN,
                            "maxLength": limits::PROMPT_MAX,
                            "description":
                                "Must name the source and period as well as the unit and basis \
                                 — which document, which year, which region, percent or \
                                 percentage points. \"In this March 2026 outlook, what was the \
                                 2026 Ontario home-price forecast, in percent?\" The reader \
                                 types a bare number months later, so a prompt that leaves the \
                                 vintage open teaches a stale figure as a current one."
                        },
                        "value": {
                            "type": "number",
                            "description":
                                "The figure exactly as the document prints it, in the unit \
                                 named below. Negative for a decline."
                        },
                        "tolerance": {
                            "type": "number",
                            "exclusiveMinimum": 0,
                            "description": format!(
                                "How far off an answer may be and still count, in the same \
                                 unit. Between {}% and {}% of the figure's magnitude. Set it \
                                 where a reader who has genuinely absorbed the document lands: \
                                 the point is to know the figure well enough to use it, not to \
                                 reproduce its last decimal.",
                                trainer_core::numeric::MIN_TOLERANCE_FRACTION * 100.0,
                                trainer_core::numeric::MAX_TOLERANCE_FRACTION * 100.0
                            )
                        },
                        "unit": {
                            "type": "string",
                            "maxLength": trainer_core::numeric::UNIT_MAX_LEN,
                            "description":
                                "Short label shown beside the input: \"%\", \"pp\", \"$B\", \
                                 \"bps\". Empty string for a bare count."
                        },
                        "explanation": {
                            "type": "string",
                            "minLength": limits::EXPLANATION_MIN,
                            "maxLength": limits::EXPLANATION_MAX,
                            "description":
                                "Where in the document the figure comes from — name the table, \
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
         The reader is training to argue from these documents in a fast-moving, \
         data-driven setting. What they need out of one is its direction, its causes, and \
         a handful of figures they can carry accurately. They do not need its decimals.\n\
         \n\
         Two rules follow from that, and they are the ones worth getting right:\n\
         \n\
         1. Questions about a FIGURE go in `numeric_questions`, never in \
         `choice_questions`. A multiple-choice question about a statistic needs three \
         wrong statistics, and inventing them teaches them — the reader remembers having \
         weighed the number, not that they rejected it. Set each tolerance where someone \
         who genuinely absorbed the document would land.\n\
         \n\
         2. Exactly one option in a `choice_questions` item may be a defensible answer. \
         The other three must come from the document and be MIS-BOUND — a real claim \
         attached to the wrong year, region or mechanism — never invented, and never a \
         claim the document makes about the thing the prompt is actually asking. \
         Lifting a true sentence from elsewhere in the report is the specific way this \
         goes wrong: it reads as a distractor and argues as an answer. An option that is \
         obviously absurd, or noticeably longer and more qualified than the others, \
         gives the answer away without any reading at all."
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
            "Read this document and call `{TOOL_NAME}` with its title, its topics, \
             {CHOICE_QUESTIONS_PER_DOC} multiple-choice questions about what it argues, and \
             {NUMERIC_QUESTIONS_PER_DOC} numeric questions about figures worth carrying out of it."
        )))
        .build()
        .map_err(|e| Error::Aws(format!("building message: {e}")))?;

    let tool = Tool::ToolSpec(
        ToolSpecification::builder()
            .name(TOOL_NAME)
            .description(
                "Emit the title, topics and questions for the document. This is the only way \
                 to return a result.",
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

    // Validated before assembly, because the checks are stated per array — a
    // count, a skill vocabulary, a tolerance band — and every one of them is
    // easier to phrase, and to report, against the shape the model returned.
    validate(&title, &topics, &quiz)?;

    let questions = assemble(quiz, req.seed);

    Ok(Generated {
        title,
        topics,
        questions,
    })
}

/// Turn the two validated arrays into the ten stored questions.
///
/// Three things happen here, all of them seeded from the document id so a
/// regeneration reproduces the same quiz and a bug in any of them is
/// reproducible:
///
/// 1. Each question becomes the [`QuestionBody`] variant its array implies, and
///    numeric ones get their skill. Neither the variant nor the skill was asked
///    of the model — see the note on `NUMERIC_SKILL`. The variant *is* the
///    format; there is no separate field to fill in.
/// 2. Multiple-choice options are shuffled. See [`shuffle_options`].
/// 3. The ten are shuffled together. Without this the three numeric questions
///    are always the last three, and a reader learns "the typing starts at
///    eight" instead of reading the prompt — the same positional tell the
///    option shuffle exists to remove, one level up.
fn assemble(quiz: GeneratedQuiz, seed: &str) -> Vec<Question> {
    let mut questions: Vec<Question> = quiz
        .choice_questions
        .into_iter()
        .map(|q| Question {
            id: q.id,
            skill: q.skill,
            shelf: q.shelf,
            prompt: q.prompt,
            explanation: q.explanation,
            body: QuestionBody::MultipleChoice {
                options: q.options,
                answer: q.answer,
            },
        })
        .chain(quiz.numeric_questions.into_iter().map(|q| Question {
            id: q.id,
            skill: NUMERIC_SKILL,
            shelf: q.shelf,
            prompt: q.prompt,
            explanation: q.explanation,
            body: QuestionBody::Numeric {
                numeric: NumericAnswer {
                    value: q.value,
                    tolerance: q.tolerance,
                    unit: q.unit,
                },
            },
        }))
        .collect();

    shuffle_options(&mut questions, seed);

    // A separate stream from the option shuffle's, so that changing one does
    // not silently reshuffle the other and make an old bug irreproducible.
    let mut state = fnv1a(&format!("order:{seed}"));
    for i in (1..questions.len()).rev() {
        let j = (next_u64(&mut state) % (i as u64 + 1)) as usize;
        questions.swap(i, j);
    }

    questions
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
fn validate(title: &str, topics: &[Topic], quiz: &GeneratedQuiz) -> Result<()> {
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

    if quiz.choice_questions.len() != CHOICE_QUESTIONS_PER_DOC {
        return Err(Error::Invalid(format!(
            "the model returned {} multiple-choice questions, expected {CHOICE_QUESTIONS_PER_DOC}",
            quiz.choice_questions.len()
        )));
    }
    if quiz.numeric_questions.len() != NUMERIC_QUESTIONS_PER_DOC {
        return Err(Error::Invalid(format!(
            "the model returned {} numeric questions, expected {NUMERIC_QUESTIONS_PER_DOC}",
            quiz.numeric_questions.len()
        )));
    }

    // Shared across both arrays. The two are assembled into one list, so an id
    // reused between them is the same collision as one reused within either:
    // the browser keys its list by id, and a submitted answer would be
    // ambiguous at grading time.
    let mut seen_ids = std::collections::HashSet::new();
    let mut claim = |id: &str| -> Result<()> {
        if !seen_ids.insert(id.to_string()) {
            return Err(Error::Invalid(format!(
                "the model returned two questions with id {id:?}"
            )));
        }
        Ok(())
    };

    for (i, q) in quiz.choice_questions.iter().enumerate() {
        let n = format!("multiple-choice question {}", i + 1);
        claim(&q.id)?;
        check_prompt(&n, &q.prompt)?;
        check_explanation(&n, &q.explanation)?;

        // Belt and braces against the schema's `enum` being ignored. This is
        // the rule that keeps invented statistics out of the option lists, so
        // it is not left to the schema alone.
        if q.skill == NUMERIC_SKILL {
            return Err(Error::Invalid(format!(
                "{n} is tagged {NUMERIC_SKILL}, which belongs in the numeric questions"
            )));
        }

        if q.options.len() != OPTIONS_PER_QUESTION {
            return Err(Error::Invalid(format!(
                "{n} has {} options, expected {OPTIONS_PER_QUESTION}",
                q.options.len()
            )));
        }

        // `Choice` guarantees a-d; this guarantees a-d indexes something. The
        // two together are what make `Choice::index` total, and what makes the
        // shuffle below safe.
        if q.answer.index() >= q.options.len() {
            return Err(Error::Invalid(format!(
                "{n} answers with an option it does not have"
            )));
        }

        for option in &q.options {
            let len = option.trim().chars().count();
            if !(limits::OPTION_MIN..=limits::OPTION_MAX).contains(&len) {
                return Err(Error::Invalid(format!("{n} has a {len}-character option")));
            }
        }

        // Duplicate options make the question unanswerable — two identical
        // choices where one is keyed correct and the other is not.
        let unique: std::collections::HashSet<&str> = q.options.iter().map(|o| o.trim()).collect();
        if unique.len() != q.options.len() {
            return Err(Error::Invalid(format!("{n} repeats an option")));
        }
    }

    let distinct_skills: std::collections::HashSet<Skill> =
        quiz.choice_questions.iter().map(|q| q.skill).collect();
    if distinct_skills.len() < MIN_DISTINCT_CHOICE_SKILLS {
        return Err(Error::Invalid(format!(
            "the model used only {} of the available skills; a quiz that measures one thing \
             cannot be segmented",
            distinct_skills.len()
        )));
    }

    for (i, q) in quiz.numeric_questions.iter().enumerate() {
        let n = format!("numeric question {}", i + 1);
        claim(&q.id)?;
        check_prompt(&n, &q.prompt)?;
        check_explanation(&n, &q.explanation)?;

        // The tolerance rules live on `NumericAnswer` rather than here, because
        // grading depends on them too and a second copy is a second place for
        // them to be wrong.
        NumericAnswer {
            value: q.value,
            tolerance: q.tolerance,
            unit: q.unit.clone(),
        }
        .validate()
        .map_err(|why| Error::Invalid(format!("{n}: {why}")))?;
    }

    Ok(())
}

fn check_prompt(which: &str, prompt: &str) -> Result<()> {
    let len = prompt.trim().chars().count();
    if !(limits::PROMPT_MIN..=limits::PROMPT_MAX).contains(&len) {
        return Err(Error::Invalid(format!(
            "{which} has a {len}-character prompt"
        )));
    }
    Ok(())
}

fn check_explanation(which: &str, explanation: &str) -> Result<()> {
    let len = explanation.trim().chars().count();
    if !(limits::EXPLANATION_MIN..=limits::EXPLANATION_MAX).contains(&len) {
        return Err(Error::Invalid(format!(
            "{which} has a {len}-character explanation"
        )));
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
///
/// Numeric questions pass through untouched: they have no options and no
/// letter, so there is no position to give anything away. That is one of the
/// quieter benefits of the format — the bias this function exists to correct
/// cannot arise there at all.
fn shuffle_options(questions: &mut [Question], seed: &str) {
    let mut state = fnv1a(seed);

    for q in questions.iter_mut() {
        // The numeric variant is skipped structurally rather than by a check on
        // a nullable field — there is no letter here to move.
        let QuestionBody::MultipleChoice { options, answer } = &mut q.body else {
            continue;
        };

        let mut order: Vec<usize> = (0..options.len()).collect();

        // Fisher-Yates, which is uniform. Repeatedly swapping random pairs is
        // the version that looks equivalent and is not.
        for i in (1..order.len()).rev() {
            let j = (next_u64(&mut state) % (i as u64 + 1)) as usize;
            order.swap(i, j);
        }

        let was_correct = answer.index();
        *options = order.iter().map(|&i| options[i].clone()).collect();

        // `order` is a permutation of every index, so the old correct index is
        // in it exactly once. Handled rather than unwrapped because a panic
        // here would surface as an invocation error with no failed-status row
        // to explain it.
        let now_correct = order
            .iter()
            .position(|&i| i == was_correct)
            .and_then(|pos| Choice::ALL.get(pos).copied());

        if let Some(choice) = now_correct {
            *answer = choice;
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
    use trainer_core::tags::QuestionFormat;

    use super::*;

    /// Cycles the permitted skills so the fixture satisfies the spread rule
    /// without every test having to think about it.
    fn choice_json(i: usize, options: usize) -> String {
        const SPREAD: [&str; 4] = ["causal", "relational", "definitional", "scope"];
        let opts: Vec<String> = (0..options)
            .map(|j| format!("\"option {i}-{j}\""))
            .collect();
        format!(
            r#"{{"id":"c{i}","skill":"{}","shelf":"slow",
                "prompt":"why does this document say {i} happened?",
                "options":[{}],"answer":"a",
                "explanation":"stated in the section on {i}, second paragraph"}}"#,
            SPREAD[(i - 1) % SPREAD.len()],
            opts.join(",")
        )
    }

    fn numeric_json(i: usize) -> String {
        format!(
            r#"{{"id":"n{i}","shelf":"dated","prompt":"what was the {i}th figure, in percent?",
                "value":-4.0,"tolerance":1.0,"unit":"%",
                "explanation":"printed in table {i}, the second row"}}"#
        )
    }

    fn quiz_json(choice: usize, options: usize, numeric: usize) -> serde_json::Value {
        let cs: Vec<String> = (1..=choice).map(|i| choice_json(i, options)).collect();
        let ns: Vec<String> = (1..=numeric).map(numeric_json).collect();
        serde_json::from_str(&format!(
            r#"{{"title":"A Reference Document","topics":["fiscal"],
                "choice_questions":[{}],"numeric_questions":[{}]}}"#,
            cs.join(","),
            ns.join(",")
        ))
        .expect("test fixture is valid json")
    }

    /// The shape every test starts from: a quiz that passes.
    fn good() -> serde_json::Value {
        quiz_json(
            CHOICE_QUESTIONS_PER_DOC,
            OPTIONS_PER_QUESTION,
            NUMERIC_QUESTIONS_PER_DOC,
        )
    }

    fn check(value: serde_json::Value) -> Result<Vec<Question>> {
        let quiz = parse(value)?;
        let topics = tags::normalise(&quiz.topics);
        validate(&quiz.title, &topics, &quiz)?;
        Ok(assemble(quiz, "doc-under-test"))
    }

    #[test]
    fn a_well_formed_quiz_is_accepted() {
        let questions = check(good()).expect("validates");
        assert_eq!(questions.len(), QUESTIONS_PER_DOC);
        assert_eq!(
            questions
                .iter()
                .filter(|q| q.format() == QuestionFormat::Numeric)
                .count(),
            NUMERIC_QUESTIONS_PER_DOC
        );
        // Every multiple-choice question keeps a letter and every numeric one
        // keeps a figure. Guaranteed by the enum rather than checked, so this
        // asserts the assembly put things in the right variant at all.
        assert_eq!(
            questions.iter().filter(|q| q.answer().is_some()).count(),
            CHOICE_QUESTIONS_PER_DOC
        );
        assert_eq!(
            questions.iter().filter(|q| q.numeric().is_some()).count(),
            NUMERIC_QUESTIONS_PER_DOC
        );
    }

    #[test]
    fn wrong_question_counts_are_rejected() {
        for (choice, numeric) in [
            (CHOICE_QUESTIONS_PER_DOC - 1, NUMERIC_QUESTIONS_PER_DOC),
            (CHOICE_QUESTIONS_PER_DOC, NUMERIC_QUESTIONS_PER_DOC - 1),
            (CHOICE_QUESTIONS_PER_DOC, NUMERIC_QUESTIONS_PER_DOC + 1),
            // Ten questions, all multiple choice — the shape this change
            // exists to make impossible.
            (QUESTIONS_PER_DOC, 0),
        ] {
            assert!(
                matches!(
                    check(quiz_json(choice, OPTIONS_PER_QUESTION, numeric)),
                    Err(Error::Invalid(_))
                ),
                "{choice} choice + {numeric} numeric was accepted"
            );
        }
    }

    #[test]
    fn wrong_option_count_is_rejected() {
        assert!(matches!(
            check(quiz_json(
                CHOICE_QUESTIONS_PER_DOC,
                3,
                NUMERIC_QUESTIONS_PER_DOC
            )),
            Err(Error::Invalid(_))
        ));
    }

    /// The bound the schema states and the bound the validator enforces have to
    /// be the same number. They are only the same number because both read
    /// `limits::PROMPT_MAX`; this test fails if one is ever hardcoded.
    #[test]
    fn an_over_long_prompt_is_rejected() {
        let mut quiz = good();
        quiz["choice_questions"][3]["prompt"] = json!("x".repeat(limits::PROMPT_MAX + 1));
        assert!(matches!(check(quiz), Err(Error::Invalid(_))));

        let mut quiz = good();
        quiz["numeric_questions"][1]["prompt"] = json!("x".repeat(limits::PROMPT_MAX + 1));
        assert!(matches!(check(quiz), Err(Error::Invalid(_))));
    }

    #[test]
    fn a_document_with_no_usable_topics_is_rejected() {
        // Every tag a compound or a connective, so `normalise` yields nothing.
        let mut quiz = good();
        quiz["topics"] = json!(["and", "of"]);
        assert!(matches!(check(quiz), Err(Error::Invalid(_))));
    }

    /// The vocabulary check, which lives in the deserializer rather than in
    /// `validate`. This is the case the closed enums exist for.
    #[test]
    fn an_invented_skill_is_rejected_at_parse_time() {
        let mut quiz = good();
        quiz["choice_questions"][0]["skill"] = json!("macroeconomic");
        assert!(matches!(parse(quiz), Err(Error::Invalid(_))));
    }

    /// **The rule that keeps invented statistics out of the option lists.**
    /// The schema states it as an `enum`, but Bedrock does not enforce a tool
    /// schema, so a model that ignores it must still be caught.
    #[test]
    fn a_figure_recall_question_may_not_be_multiple_choice() {
        let mut quiz = good();
        quiz["choice_questions"][0]["skill"] = json!(NUMERIC_SKILL.as_str());
        assert!(matches!(check(quiz), Err(Error::Invalid(_))));
    }

    #[test]
    fn a_quiz_that_measures_one_thing_is_rejected() {
        let mut quiz = good();
        for i in 0..CHOICE_QUESTIONS_PER_DOC {
            quiz["choice_questions"][i]["skill"] = json!("definitional");
        }
        assert!(matches!(check(quiz), Err(Error::Invalid(_))));
    }

    #[test]
    fn an_answer_outside_a_to_d_is_rejected_at_parse_time() {
        let mut quiz = good();
        quiz["choice_questions"][0]["answer"] = json!("e");
        assert!(matches!(parse(quiz), Err(Error::Invalid(_))));
    }

    #[test]
    fn duplicate_options_are_rejected() {
        let mut quiz = good();
        quiz["choice_questions"][0]["options"][1] =
            quiz["choice_questions"][0]["options"][0].clone();
        assert!(matches!(check(quiz), Err(Error::Invalid(_))));
    }

    /// An id reused *between* the two arrays is the same collision as one
    /// reused within either, because they are assembled into one list.
    #[test]
    fn an_id_reused_across_the_two_arrays_is_rejected() {
        let mut quiz = good();
        quiz["numeric_questions"][0]["id"] = quiz["choice_questions"][0]["id"].clone();
        assert!(matches!(check(quiz), Err(Error::Invalid(_))));
    }

    /// The tolerance rules are enforced by `NumericAnswer::validate`, which
    /// grading also depends on. This checks they are actually reached from
    /// here — a numeric question that skipped them would be stored with a
    /// tolerance that makes it unanswerable or free.
    #[test]
    fn a_useless_tolerance_is_rejected() {
        for tolerance in [json!(0), json!(-1), json!(0.0001), json!(1000)] {
            let mut quiz = good();
            quiz["numeric_questions"][0]["tolerance"] = tolerance.clone();
            assert!(
                matches!(check(quiz), Err(Error::Invalid(_))),
                "tolerance {tolerance} was accepted"
            );
        }
    }

    /// The property the shuffle exists for: the correct *text* must survive,
    /// even though the correct *letter* changes.
    #[test]
    fn shuffling_moves_the_letter_but_not_the_answer() {
        let quiz = parse(good()).expect("parses");
        let before: Vec<(String, String)> = quiz
            .choice_questions
            .iter()
            .map(|q| (q.id.clone(), q.options[q.answer.index()].clone()))
            .collect();

        let questions = assemble(quiz, "doc-under-test");

        for (id, expected) in before {
            let q = questions.iter().find(|q| q.id == id).expect("survives");
            let QuestionBody::MultipleChoice { options, answer } = &q.body else {
                panic!("a choice question came back as something else");
            };
            assert_eq!(
                options[answer.index()],
                expected,
                "the answer key must still point at the same text"
            );
            assert_eq!(options.len(), OPTIONS_PER_QUESTION, "nothing lost");
        }
    }

    #[test]
    fn shuffling_actually_moves_something() {
        // Every fixture question keys "a". If the shuffle were a no-op this
        // test would pass silently against a broken implementation, so assert
        // the distribution changed rather than that it is uniform.
        let questions = check(good()).expect("validates");
        assert!(
            questions
                .iter()
                .any(|q| q.answer().is_some_and(|a| a != Choice::A)),
            "shuffle left every answer at 'a'"
        );
    }

    /// Without this the three typed questions are always the last three, and
    /// the reader learns the position instead of reading the prompt.
    #[test]
    fn the_numeric_questions_are_not_all_at_the_end() {
        let questions = check(good()).expect("validates");
        let tail = &questions[QUESTIONS_PER_DOC - NUMERIC_QUESTIONS_PER_DOC..];
        assert!(
            !tail.iter().all(|q| q.format() == QuestionFormat::Numeric),
            "the numeric questions are still bunched at the end"
        );
    }

    #[test]
    fn assembly_is_reproducible_for_a_document() {
        let first = assemble(parse(good()).expect("parses"), "same-doc");
        let second = assemble(parse(good()).expect("parses"), "same-doc");

        let fingerprint = |qs: &[Question]| {
            qs.iter()
                .map(|q| (q.id.clone(), q.answer(), q.format()))
                .collect::<Vec<_>>()
        };
        assert_eq!(fingerprint(&first), fingerprint(&second));
    }

    /// The schema is generated, so it can silently stop mentioning a skill the
    /// deserializer still accepts. That drift is exactly what produced a 100%
    /// failure rate under the old prose prompt.
    #[test]
    fn the_schema_lists_the_choice_vocabulary_and_excludes_the_numeric_one() {
        let schema = quiz_schema(&[]).to_string();
        for skill in Skill::ALL.iter().filter(|s| **s != NUMERIC_SKILL) {
            assert!(
                schema.contains(skill.as_str()),
                "schema omits skill {skill}"
            );
        }
        for choice in Choice::ALL {
            assert!(schema.contains(choice.as_str()));
        }

        // And the numeric array must not offer a skill field at all — if it
        // did, the model could tag a typed figure as `causal` and the history
        // matrix would stop meaning what it says.
        let numeric = &quiz_schema(&[])["properties"]["numeric_questions"]["items"];
        assert!(numeric["properties"]["skill"].is_null());
        assert!(numeric["properties"]["options"].is_null());
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
