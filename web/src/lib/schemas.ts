import { z } from "zod";

/* ------------------------------------------------------------------ *
 * Tag vocabularies
 *
 * `Skill` and `QuestionFormat` are closed and mirror the backend's enums
 * exactly. A value the generator emits that the client doesn't know about must
 * fail parsing loudly, because a silently-dropped tag corrupts every rate in
 * the history view without ever rendering an error.
 *
 * `Topic` is open — see `app/core/src/tags.rs`. The model coins topics as it
 * meets new subject matter, so there is no list to mirror here and no way to
 * "know about" a tag in advance.
 * ------------------------------------------------------------------ */

export const QuestionFormat = z.enum(["multiple_choice", "numeric"]);
export type QuestionFormat = z.infer<typeof QuestionFormat>;

/**
 * How sure the reader says they are, recorded with every answer.
 *
 * The bands are scored by a *proper* rule — see `Confidence::points` in
 * `app/core/src/tags.rs`, which derives the table rather than picking it. The
 * only thing that matters on this side is that the thresholds shown to the
 * reader are the ones the server actually scores against, which is why
 * CONFIDENCE_BOUNDS below is not a rounded-off paraphrase.
 */
export const Confidence = z.enum(["guessing", "fairly_sure", "certain"]);
export type Confidence = z.infer<typeof Confidence>;
export const CONFIDENCE_BANDS = Confidence.options;

export const CONFIDENCE_LABELS: Record<Confidence, string> = {
  guessing: "Guessing",
  fairly_sure: "Fairly sure",
  certain: "Certain",
};

/**
 * What each band commits you to, in points.
 *
 * **Display only — the server computes every score.** This exists so the reader
 * can see the price of a claim *before* making it; a cost revealed only
 * afterwards trains nothing.
 *
 * It is therefore a second copy of a table that lives in `Confidence::points`,
 * and there is no test holding the two together, because this package has no
 * test runner (issue #33). What limits the damage is that the copy is never
 * used to compute anything: the points on the results screen and in history are
 * the server's, so a drift here misinforms the reader about the price without
 * changing what they are charged. Worth fixing, not worth a wrong abstraction.
 */
export const CONFIDENCE_POINTS: Record<Confidence, { correct: number; wrong: number }> = {
  guessing: { correct: 1, wrong: 0 },
  fairly_sure: { correct: 2, wrong: -1 },
  certain: { correct: 3, wrong: -5 },
};

/** The belief range each band is the best report for, as percentages. */
export const CONFIDENCE_BOUNDS: Record<Confidence, { low: number; high: number }> = {
  guessing: { low: 25, high: 50 },
  fairly_sure: { low: 50, high: 80 },
  certain: { low: 80, high: 100 },
};

export const MAX_POINTS_PER_QUESTION = 3;

/**
 * The lowest honest probability on a question of this shape. Mirrors
 * `Confidence::chance_floor_percent`.
 *
 * Below chance is not modesty, it is an error: on four options you will answer
 * *something*, so a one-in-four belief is the floor. The slider starts here and
 * cannot go under it, which removes a class of meaningless report rather than
 * scoring it. A typed figure has no options and so effectively no floor.
 */
export const CHANCE_FLOOR_PERCENT: Record<QuestionFormat, number> = {
  multiple_choice: 25,
  numeric: 2,
};

/**
 * The band a stated percentage falls in. Mirrors `Confidence::from_percent`.
 *
 * Used only to show the reader what their slider position is worth *before*
 * they commit to it. The server derives the band again from the number it is
 * sent, and that derivation is the one that scores.
 */
export function bandForPercent(percent: number): Confidence {
  if (percent < CONFIDENCE_BOUNDS.guessing.high) return "guessing";
  if (percent < CONFIDENCE_BOUNDS.fairly_sure.high) return "fairly_sure";
  return "certain";
}

export const Skill = z.enum([
  "figure_recall",
  "relational",
  "definitional",
  "causal",
  "scope",
]);
export type Skill = z.infer<typeof Skill>;

/**
 * A topic: one lowercase word, chosen by the model.
 *
 * Deliberately permissive — no `^[a-z]+$` regex, even though that is the rule
 * the server enforces on everything it writes. Two reasons, and they point the
 * same way:
 *
 *   - Rows written under the old closed vocabulary contain `international_economics`.
 *     A strict schema here would throw on the document list rather than render
 *     a slightly odd tag, and a blank screen is much worse than a stale word.
 *   - Validating a *response* re-checks a rule the server already enforced on
 *     the way in. The client cannot fix a bad tag; it can only refuse to draw
 *     the page.
 *
 * Strictness belongs at ingress, and ingress is `Topic::parse` in Rust.
 */
export const Topic = z.string().min(1).max(60);
export type Topic = z.infer<typeof Topic>;

export const SKILLS = Skill.options;
export const FORMATS = QuestionFormat.options;

/**
 * Which skills each format can produce, as of tag version 3.
 *
 * Not a cosmetic grouping — it is the rule that keeps invented statistics out
 * of the option lists, mirrored from `NUMERIC_SKILL` in
 * `app/generate/src/bedrock.rs`. A question about a figure is asked as a typed
 * figure, so `figure_recall` is now numeric-only and the other four are
 * multiple-choice-only.
 *
 * The history matrix uses this to decide which rows to draw. An empty row means
 * "you have never been tested on this", which is worth seeing; a row that
 * *cannot* be filled means nothing at all, and drawing it invites the first
 * reading of the second thing.
 */
export const SKILLS_BY_FORMAT: Record<QuestionFormat, readonly Skill[]> = {
  multiple_choice: SKILLS.filter((skill) => skill !== "figure_recall"),
  numeric: ["figure_recall"],
};

/** Display labels. Kept next to the enums so adding a tag breaks in one place. */
export const SKILL_LABELS: Record<Skill, string> = {
  figure_recall: "Figure recall",
  relational: "Relational",
  definitional: "Definitional",
  causal: "Causal",
  scope: "Scope",
};

/**
 * Topics get a function rather than a lookup table, because there is no longer
 * a fixed set to write a table for. Underscores appear only in tags predating
 * the one-word rule.
 */
export function topicLabel(topic: Topic): string {
  const spaced = topic.replaceAll("_", " ");
  return spaced.charAt(0).toUpperCase() + spaced.slice(1);
}

export const FORMAT_LABELS: Record<QuestionFormat, string> = {
  multiple_choice: "Multiple choice",
  numeric: "Typed figure",
};

/* ------------------------------------------------------------------ *
 * Auth
 * ------------------------------------------------------------------ */

/**
 * `POST /auth/challenge`. Everything binary arrives base64url-encoded; the
 * client decodes into the ArrayBuffers WebAuthn wants (see lib/webauthn.ts).
 */
export const AuthChallenge = z.object({
  challenge: z.string().min(1),
  rpId: z.string().min(1),
  timeoutMs: z.number().int().positive().optional(),
  allowCredentials: z
    .array(
      z.object({
        id: z.string().min(1),
        transports: z.array(z.string()).optional(),
      }),
    )
    .default([]),
  userVerification: z.enum(["required", "preferred", "discouraged"]).default("preferred"),
});
export type AuthChallenge = z.infer<typeof AuthChallenge>;

/** `POST /auth/verify`. 30-day JWT; `expiresAt` lets us pre-empt a 401. */
export const AuthSession = z.object({
  token: z.string().min(1),
  expiresAt: z.iso.datetime(),
});
export type AuthSession = z.infer<typeof AuthSession>;

/* ------------------------------------------------------------------ *
 * Documents
 * ------------------------------------------------------------------ */

export const DocumentStatus = z.enum(["pending", "processing", "ready", "failed"]);
export type DocumentStatus = z.infer<typeof DocumentStatus>;

/** Statuses that mean "the backend still owes us something" — drives polling. */
export const UNSETTLED_STATUSES: readonly DocumentStatus[] = ["pending", "processing"];

export const isUnsettled = (status: DocumentStatus): boolean =>
  UNSETTLED_STATUSES.includes(status);

export const DocumentSummary = z.object({
  id: z.string().min(1),
  title: z.string(),
  topics: z.array(Topic),
  status: DocumentStatus,
  pageCount: z.number().int().nonnegative(),
  createdAt: z.iso.datetime(),
  /** Populated only when status is `failed`; surfaced verbatim next to Retry. */
  error: z.string().nullable().default(null),
  /** How many quizzes have been submitted against this document. */
  attemptCount: z.number().int().nonnegative().default(0),
});
export type DocumentSummary = z.infer<typeof DocumentSummary>;

export const DocumentList = z.object({
  documents: z.array(DocumentSummary),
});

/**
 * `POST /docs` response.
 *
 * `uploadUrl` is null on a retry: the PDF is already in S3, so re-running
 * generation must not make us re-upload it. See CreateDocumentRequest.
 */
export const CreateDocumentResult = z.object({
  id: z.string().min(1),
  uploadUrl: z.url().nullable().default(null),
});
export type CreateDocumentResult = z.infer<typeof CreateDocumentResult>;

/** `GET /docs/:id/url` — short-lived presigned GET for the `<embed>`. */
export const DocumentUrl = z.object({
  url: z.url(),
  expiresAt: z.iso.datetime(),
});
export type DocumentUrl = z.infer<typeof DocumentUrl>;

/* ------------------------------------------------------------------ *
 * Quiz
 * ------------------------------------------------------------------ */

export const QuestionOption = z.object({
  id: z.string().min(1),
  text: z.string().min(1),
});
export type QuestionOption = z.infer<typeof QuestionOption>;

const QuizQuestionCommon = {
  id: z.string().min(1),
  skill: Skill,
  topics: z.array(Topic).min(1),
  prompt: z.string().min(1),
};

/**
 * `GET /docs/:id/quiz` deliberately omits the answer key. There is no optional
 * `answer` field to fall back on — if one ever appeared here it would be a
 * backend leak, and this schema strips it rather than letting UI code read it.
 *
 * A discriminated union rather than one object with optional fields, because
 * the two formats need different inputs and different validation, and
 * `question.options[0]` on a typed-figure question should not typecheck. The
 * numeric variant carries `tolerance` and `unit` — hints about *precision*, not
 * about the value — and conspicuously no `value`.
 */
export const QuizQuestion = z.discriminatedUnion("format", [
  z.object({
    ...QuizQuestionCommon,
    format: z.literal("multiple_choice"),
    // Exactly four is a rendering invariant, not a preference: the option
    // letters (A–D) and the grid are built against it. A quiz with three
    // options is a generator bug we want to see immediately.
    options: z.array(QuestionOption).length(4),
  }),
  z.object({
    ...QuizQuestionCommon,
    format: z.literal("numeric"),
    // Present and empty, not absent. The server builds every question through
    // one code path; an options array that went missing here would mean it had
    // stopped doing that.
    options: z.array(QuestionOption).length(0),
    tolerance: z.number().positive(),
    /** May be the empty string, for a bare count. */
    unit: z.string(),
  }),
]);
export type QuizQuestion = z.infer<typeof QuizQuestion>;

export const Quiz = z.object({
  documentId: z.string().min(1),
  // Ten questions is the product target, but a short quiz is still answerable —
  // don't blank the screen over it. The option count above is the hard invariant.
  questions: z.array(QuizQuestion).min(1),
});
export type Quiz = z.infer<typeof Quiz>;

/**
 * `POST /docs/:id/submit` — graded, with the key revealed for the first time.
 *
 * Flat with nullable fields rather than a discriminated union, unlike
 * `QuizQuestion`. The results screen renders both formats in one list and
 * mostly cares about the parts they share; narrowing here would buy type
 * safety on fields the renderer already guards with a `format` check, at the
 * cost of two near-identical shapes.
 */
export const GradedQuestion = z.object({
  questionId: z.string().min(1),
  format: QuestionFormat,
  skill: Skill,
  topics: z.array(Topic).min(1),
  prompt: z.string().min(1),
  /** Four on a multiple-choice question, empty on a typed figure. */
  options: z.array(QuestionOption),
  selectedOptionId: z.string().nullable().default(null),
  correctOptionId: z.string().nullable().default(null),
  /** Verbatim, including an entry that did not parse as a number. */
  selectedValue: z.string().nullable().default(null),
  correctValue: z.number().nullable().default(null),
  tolerance: z.number().nullable().default(null),
  unit: z.string().nullable().default(null),
  correct: z.boolean(),
  /** Null only for attempts predating confidence — not for a skipped question. */
  confidence: Confidence.nullable().default(null),
  /** The probability actually stated. Null on attempts predating the slider. */
  confidencePercent: z.number().int().min(0).max(100).nullable().default(null),
  /** As awarded by the server. Never recomputed here. */
  points: z.number().int(),
  explanation: z.string().default(""),
});
export type GradedQuestion = z.infer<typeof GradedQuestion>;

export const AttemptResult = z.object({
  attemptId: z.string().min(1),
  documentId: z.string().min(1),
  submittedAt: z.iso.datetime(),
  correct: z.number().int().nonnegative(),
  total: z.number().int().positive(),
  /**
   * The calibration score, and its ceiling. Negative is possible and is the
   * whole point — reported beside `correct`, never instead of it, because they
   * answer different questions.
   */
  points: z.number().int(),
  maxPoints: z.number().int(),
  questions: z.array(GradedQuestion),
});
export type AttemptResult = z.infer<typeof AttemptResult>;

/* ------------------------------------------------------------------ *
 * Review
 *
 * The spaced queue. Same questions, same grading, no document on screen —
 * a review with the PDF open is a comprehension test, not a retrieval one.
 * ------------------------------------------------------------------ */

/**
 * `GET /review`.
 *
 * Options arrive **in this repetition's order**, which is not the stored order:
 * the option shown third is `c` whatever its position in the document's record.
 * The server holds the permutation and grades against it, so the client just
 * renders what it is given and posts back the letter — it must not sort,
 * re-letter, or otherwise second-guess this array.
 */
export const ReviewQuestion = z.object({
  questionId: z.string().min(1),
  documentId: z.string().min(1),
  documentTitle: z.string(),
  format: QuestionFormat,
  skill: Skill,
  topics: z.array(Topic),
  prompt: z.string().min(1),
  options: z.array(QuestionOption),
  tolerance: z.number().nullable().default(null),
  unit: z.string().nullable().default(null),
  /** Repetitions already done. Zero means this has never come back before. */
  reps: z.number().int().nonnegative(),
  dueAt: z.iso.datetime(),
  /** When the document was read. Rendered as an age, so a question from a
   *  two-year-old report is visibly one. */
  sourceDatedAt: z.string(),
});
export type ReviewQuestion = z.infer<typeof ReviewQuestion>;

export const ReviewQueue = z.object({
  questions: z.array(ReviewQuestion),
  /** Everything due, including what did not fit in this session. */
  dueTotal: z.number().int().nonnegative(),
  /** The whole schedule — the denominator for "how much is being kept alive". */
  scheduledTotal: z.number().int().nonnegative(),
  /** Aged out of being worth asking. Reported, not silently subtracted. */
  retiredTotal: z.number().int().nonnegative().default(0),
  /** Only when nothing is due: when the next thing comes back. */
  nextDueAt: z.iso.datetime().nullable().default(null),
});
export type ReviewQueue = z.infer<typeof ReviewQueue>;

export const ReviewResult = z.object({
  questionId: z.string().min(1),
  documentId: z.string().min(1),
  prompt: z.string().min(1),
  correct: z.boolean(),
  confidence: Confidence.nullable().default(null),
  confidencePercent: z.number().int().min(0).max(100).nullable().default(null),
  points: z.number().int(),
  options: z.array(QuestionOption),
  correctOptionId: z.string().nullable().default(null),
  selectedOptionId: z.string().nullable().default(null),
  selectedValue: z.string().nullable().default(null),
  correctValue: z.number().nullable().default(null),
  unit: z.string().nullable().default(null),
  explanation: z.string().default(""),
  /** What the answer just bought: when this comes back, and how far out. */
  nextDueAt: z.iso.datetime(),
  intervalDays: z.number().int(),
});
export type ReviewResult = z.infer<typeof ReviewResult>;

export const ReviewSubmitResult = z.object({
  correct: z.number().int().nonnegative(),
  total: z.number().int().nonnegative(),
  points: z.number().int(),
  maxPoints: z.number().int(),
  results: z.array(ReviewResult),
});
export type ReviewSubmitResult = z.infer<typeof ReviewSubmitResult>;

export const ReviewSubmitRequest = z.object({
  answers: z.array(
    z.object({
      documentId: z.string().min(1),
      questionId: z.string().min(1),
      /** The letter **as shown**. The server maps it back. */
      optionId: z.string().min(1).optional(),
      value: z.string().min(1).optional(),
      confidencePercent: z.number().int().min(0).max(100),
    }),
  ),
});
export type ReviewSubmitRequest = z.infer<typeof ReviewSubmitRequest>;

/* ------------------------------------------------------------------ *
 * History
 * ------------------------------------------------------------------ */

/**
 * One graded question inside a past attempt. `format` is carried per question,
 * not per attempt: once free-recall lands, one document can produce a mixed
 * attempt, and the aggregation must still be able to segment.
 */
export const HistoryQuestion = z.object({
  questionId: z.string().min(1),
  format: QuestionFormat,
  skill: Skill,
  topics: z.array(Topic).min(1),
  correct: z.boolean(),
  /** The probability stated, when one was. Absent before the slider existed. */
  confidencePercent: z.number().int().min(0).max(100).optional(),
  /**
   * Absent on every question answered before confidence was asked for.
   *
   * **Absent is not "guessing".** Those answers were given without the question
   * being put, so they carry no information about calibration and must be
   * dropped from the reliability table rather than bucketed at the bottom —
   * folding them in would invent a claim the reader never made, which is
   * exactly the error the table exists to detect in the reader.
   */
  confidence: Confidence.optional(),
  points: z.number().int().default(0),
});
export type HistoryQuestion = z.infer<typeof HistoryQuestion>;

export const HistoryAttempt = z.object({
  attemptId: z.string().min(1),
  documentId: z.string().min(1),
  documentTitle: z.string(),
  submittedAt: z.iso.datetime(),
  questions: z.array(HistoryQuestion),
});
export type HistoryAttempt = z.infer<typeof HistoryAttempt>;

export const History = z.object({
  attempts: z.array(HistoryAttempt),
});
export type History = z.infer<typeof History>;

/* ------------------------------------------------------------------ *
 * Request bodies
 * ------------------------------------------------------------------ */

/**
 * `POST /docs` serves two jobs, distinguished by `retryOf`:
 *   - new upload: filename/pageCount, response carries a presigned PUT URL
 *   - retry:      `retryOf: <failed document id>`, response carries uploadUrl: null
 *
 * Folding retry into `POST /docs` keeps the endpoint list as specified. The
 * alternative — re-uploading the same PDF to a fresh document — is precisely the
 * zombie-accumulation the retry button exists to prevent.
 */
export const CreateDocumentRequest = z.union([
  z.object({
    /**
     * Used as a placeholder title until the model reads the document and
     * replaces it, about a minute later. The uploader is no longer asked for a
     * title or for topics — the model picks both, from the document rather
     * than from a filename, which is better information and two fewer fields
     * between choosing a PDF and reading it.
     */
    filename: z.string().max(300),
    pageCount: z.number().int().positive(),
    contentType: z.literal("application/pdf"),
    // Required, not optional. The presigned PUT signs content-length, which
    // fixes the object size at exactly this value — a pinned-key presigned URL
    // with no size bound otherwise accepts a multi-gigabyte object. Sending it
    // costs the client nothing (File.size is already to hand) and the server
    // has no other way to obtain it before the upload happens.
    sizeBytes: z.number().int().positive(),
  }),
  z.object({ retryOf: z.string().min(1) }),
]);
export type CreateDocumentRequest = z.infer<typeof CreateDocumentRequest>;

export const SubmitQuizRequest = z.object({
  // Client-generated and stable across retries of the same submission. A
  // double-tap on a flaky phone connection otherwise writes two attempts and
  // skews every rate in the history view. The server stores an idempotency
  // marker alongside the attempt in one transaction and replays the original
  // response rather than regrading.
  attemptId: z.uuid(),
  answers: z.array(
    z.object({
      questionId: z.string().min(1),
      /** Multiple choice only. Sending it for a typed figure is a 400. */
      optionId: z.string().min(1).optional(),
      /** Typed figures only, as entered. Sending it for a letter is a 400. */
      value: z.string().min(1).optional(),
      /**
       * The probability off the slider. Authoritative: the server derives the
       * band from it and ignores `confidence` when this is present.
       */
      confidencePercent: z.number().int().min(0).max(100),
    }),
  ),
  // Wall-clock time on the quiz. Stored as 0 when absent.
  durationMs: z.number().int().nonnegative().optional(),
});
export type SubmitQuizRequest = z.infer<typeof SubmitQuizRequest>;

/** Shape the API uses for error bodies; best-effort, never required. */
export const ApiErrorBody = z.object({
  message: z.string().optional(),
  error: z.string().optional(),
});
