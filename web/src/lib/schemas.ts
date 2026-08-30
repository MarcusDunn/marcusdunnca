import { z } from "zod";

/* ------------------------------------------------------------------ *
 * Closed tag vocabulary
 *
 * These three enums mirror the backend's vocabulary exactly. They live in one
 * file on purpose: a tag the generator emits that the client doesn't know about
 * must fail parsing loudly, because a silently-dropped tag corrupts every rate
 * in the history view without ever rendering an error.
 * ------------------------------------------------------------------ */

export const QuestionFormat = z.enum(["multiple_choice"]);
export type QuestionFormat = z.infer<typeof QuestionFormat>;

export const Skill = z.enum([
  "figure_recall",
  "relational",
  "definitional",
  "causal",
  "scope",
]);
export type Skill = z.infer<typeof Skill>;

export const Topic = z.enum([
  "international_economics",
  "fiscal",
  "energy",
  "municipal",
  "regulatory",
  "audit",
  "monetary",
  "trade",
]);
export type Topic = z.infer<typeof Topic>;

export const SKILLS = Skill.options;
export const TOPICS = Topic.options;
export const FORMATS = QuestionFormat.options;

/** Display labels. Kept next to the enums so adding a tag breaks in one place. */
export const SKILL_LABELS: Record<Skill, string> = {
  figure_recall: "Figure recall",
  relational: "Relational",
  definitional: "Definitional",
  causal: "Causal",
  scope: "Scope",
};

export const TOPIC_LABELS: Record<Topic, string> = {
  international_economics: "Int'l economics",
  fiscal: "Fiscal",
  energy: "Energy",
  municipal: "Municipal",
  regulatory: "Regulatory",
  audit: "Audit",
  monetary: "Monetary",
  trade: "Trade",
};

export const FORMAT_LABELS: Record<QuestionFormat, string> = {
  multiple_choice: "Multiple choice",
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

/**
 * `GET /docs/:id/quiz` deliberately omits the answer key. There is no optional
 * `answer` field to fall back on — if one ever appeared here it would be a
 * backend leak, and this schema strips it rather than letting UI code read it.
 */
export const QuizQuestion = z.object({
  id: z.string().min(1),
  format: QuestionFormat,
  skill: Skill,
  topics: z.array(Topic).min(1),
  prompt: z.string().min(1),
  // Exactly four is a rendering invariant, not a preference: the option letters
  // (A–D) and the grid are built against it. A quiz with three options is a
  // generator bug we want to see immediately.
  options: z.array(QuestionOption).length(4),
});
export type QuizQuestion = z.infer<typeof QuizQuestion>;

export const Quiz = z.object({
  documentId: z.string().min(1),
  // Ten questions is the product target, but a short quiz is still answerable —
  // don't blank the screen over it. The option count above is the hard invariant.
  questions: z.array(QuizQuestion).min(1),
});
export type Quiz = z.infer<typeof Quiz>;

/** `POST /docs/:id/submit` — graded, with the key revealed for the first time. */
export const GradedQuestion = z.object({
  questionId: z.string().min(1),
  format: QuestionFormat,
  skill: Skill,
  topics: z.array(Topic).min(1),
  prompt: z.string().min(1),
  options: z.array(QuestionOption).length(4),
  selectedOptionId: z.string().nullable().default(null),
  correctOptionId: z.string().min(1),
  correct: z.boolean(),
  explanation: z.string().default(""),
});
export type GradedQuestion = z.infer<typeof GradedQuestion>;

export const AttemptResult = z.object({
  attemptId: z.string().min(1),
  documentId: z.string().min(1),
  submittedAt: z.iso.datetime(),
  correct: z.number().int().nonnegative(),
  total: z.number().int().positive(),
  questions: z.array(GradedQuestion),
});
export type AttemptResult = z.infer<typeof AttemptResult>;

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
 *   - new upload: title/topics/pageCount, response carries a presigned PUT URL
 *   - retry:      `retryOf: <failed document id>`, response carries uploadUrl: null
 *
 * Folding retry into `POST /docs` keeps the endpoint list as specified. The
 * alternative — re-uploading the same PDF to a fresh document — is precisely the
 * zombie-accumulation the retry button exists to prevent.
 */
export const CreateDocumentRequest = z.union([
  z.object({
    title: z.string().min(1).max(200),
    topics: z.array(Topic).min(1),
    pageCount: z.number().int().positive(),
    contentType: z.literal("application/pdf"),
  }),
  z.object({ retryOf: z.string().min(1) }),
]);
export type CreateDocumentRequest = z.infer<typeof CreateDocumentRequest>;

export const SubmitQuizRequest = z.object({
  answers: z.array(
    z.object({
      questionId: z.string().min(1),
      optionId: z.string().min(1),
    }),
  ),
});
export type SubmitQuizRequest = z.infer<typeof SubmitQuizRequest>;

/** Shape the API uses for error bodies; best-effort, never required. */
export const ApiErrorBody = z.object({
  message: z.string().optional(),
  error: z.string().optional(),
});
