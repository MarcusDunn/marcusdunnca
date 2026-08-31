import { useForm } from "@tanstack/react-form";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { Busy, BusyMark, ErrorNotice } from "../components/ui";
import { api } from "../lib/api";
import { formatFigure, formatTolerance, parseFigure } from "../lib/figures";
import { queryKeys, reviewQueueQuery } from "../lib/queries";
import { ConfidenceSlider } from "../components/confidence";
import {
  CHANCE_FLOOR_PERCENT,
  CONFIDENCE_LABELS,
  SKILL_LABELS,
  type ReviewQuestion,
  type ReviewQueue,
  type ReviewResult,
  type ReviewSubmitResult,
} from "../lib/schemas";

const OPTION_LETTERS = ["A", "B", "C", "D"] as const;

type Entry = { answer: string; confidencePercent: number | null };
const EMPTY: Entry = { answer: "", confidencePercent: null };

export function ReviewScreen() {
  const queryClient = useQueryClient();
  const queue = useQuery(reviewQueueQuery());

  const submit = useMutation({
    mutationFn: (entries: Record<string, Entry>) => {
      const questions = queue.data?.questions ?? [];
      return api.submitReview({
        answers: questions.map((question) => {
          const entry = entries[question.questionId] ?? EMPTY;
          const documentId = question.documentId;
          const questionId = question.questionId;
          // Unreachable: the form refuses to submit with any slider untouched.
          const confidencePercent =
            entry.confidencePercent ?? CHANCE_FLOOR_PERCENT[question.format];

          // Two whole literals rather than a conditional spread. Which key is
          // present is the meaningful part — the server rejects the wrong one
          // for a question's format — so it should be readable as such.
          return question.format === "multiple_choice"
            ? { documentId, questionId, optionId: entry.answer, confidencePercent }
            : { documentId, questionId, value: entry.answer, confidencePercent };
        }),
      });
    },
    onSuccess: () => {
      // The schedule has moved, so the queue this screen was built from is
      // stale by definition. History is untouched — reviews deliberately do not
      // write attempts.
      void queryClient.invalidateQueries({ queryKey: queryKeys.reviewQueue });
    },
  });

  return (
    <section>
      <h1>Review</h1>

      {queue.isPending ? (
        <Busy label="Checking what's due" />
      ) : queue.isError ? (
        <ErrorNotice error={queue.error} onRetry={() => void queue.refetch()} />
      ) : submit.data ? (
        <Results result={submit.data} onAgain={() => submit.reset()} />
      ) : queue.data.questions.length === 0 ? (
        <NothingDue queue={queue.data} />
      ) : (
        <>
          <p>
            {describeQueue(queue.data)} No document on screen — that is the
            point. Answering from memory is what moves the schedule; reading the
            answer off the page would not.
          </p>
          <ReviewForm
            queue={queue.data}
            submitting={submit.isPending}
            error={submit.error}
            onSubmit={(entries) => submit.mutate(entries)}
          />
        </>
      )}
    </section>
  );
}

/**
 * How old the source document is, when that is old enough to matter.
 *
 * Shown because a question is not wrong just because its document is stale — it
 * is still a correct statement about what that report said — but answering it
 * without knowing the report is two years old is how a forecast gets remembered
 * as a fact. The prompt itself names the period; this is the second line of
 * defence, and it costs a string.
 */
function describeAge(sourceDatedAt: string): string {
  const read = Date.parse(sourceDatedAt);
  if (Number.isNaN(read)) return "";

  const months = Math.floor((Date.now() - read) / (30 * 24 * 60 * 60 * 1000));
  if (months < 6) return "";
  if (months < 24) return `, read ${months} months ago`;
  return `, read ${Math.floor(months / 12)} years ago`;
}

function describeQueue(queue: ReviewQueue): string {
  const showing = queue.questions.length;
  const retired =
    queue.retiredTotal === 0
      ? ""
      : `, ${queue.retiredTotal} retired as out of date`;
  const held = `${queue.scheduledTotal} question${queue.scheduledTotal === 1 ? "" : "s"} scheduled${retired}`;

  if (queue.dueTotal > showing) {
    return `${showing} of ${queue.dueTotal} due, oldest first — ${held}. Finishing this session shortens the next one.`;
  }
  return `${showing} due — ${held}.`;
}

function NothingDue({ queue }: { queue: ReviewQueue }) {
  if (queue.scheduledTotal === 0) {
    return (
      <p>
        Nothing scheduled yet. Questions enter the queue when you take a
        document&apos;s quiz — the first sitting is the first repetition. Start
        with <Link to="/docs">a document</Link>.
      </p>
    );
  }

  return (
    <p>
      Nothing due. {queue.scheduledTotal} question
      {queue.scheduledTotal === 1 ? " is" : "s are"} scheduled
      {queue.nextDueAt
        ? `, and the next comes back ${new Date(queue.nextDueAt).toLocaleDateString()}`
        : ""}
      . Coming back early is not useful — a review you would have passed easily
      teaches almost nothing, which is why the interval exists.
    </p>
  );
}

function ReviewForm({
  queue,
  submitting,
  error,
  onSubmit,
}: {
  queue: ReviewQueue;
  submitting: boolean;
  error: unknown;
  onSubmit: (entries: Record<string, Entry>) => void;
}) {
  const form = useForm({
    defaultValues: Object.fromEntries(
      queue.questions.map((question) => [question.questionId, EMPTY]),
    ) as Record<string, Entry>,
    validators: {
      onSubmit: ({ value }) => {
        const problem = describeProblems(queue, value);
        return problem ? { form: problem, fields: {} } : null;
      },
    },
    onSubmit: ({ value }) => onSubmit(value),
  });

  return (
    <form
      onSubmit={(event) => {
        event.preventDefault();
        void form.handleSubmit();
      }}
    >
      <ol>
        {queue.questions.map((question) => (
          <li key={`${question.documentId}-${question.questionId}`}>
            <form.Field name={question.questionId}>
              {(field) => (
                <ReviewCard
                  question={question}
                  entry={field.state.value}
                  onChange={(next) => field.handleChange(next)}
                  disabled={submitting}
                />
              )}
            </form.Field>
          </li>
        ))}
      </ol>

      {error ? <ErrorNotice error={error} /> : null}

      <form.Subscribe selector={(state) => state.errorMap.onSubmit}>
        {(formError) =>
          formError ? (
            <p role="alert">
              {typeof formError === "object" && formError !== null && "form" in formError
                ? String((formError as { form: unknown }).form)
                : String(formError)}
            </p>
          ) : null
        }
      </form.Subscribe>

      <p>
        <button type="submit" disabled={submitting}>
          {submitting ? "Grading…" : `Submit ${queue.questions.length} answers`}
        </button>{" "}
        {submitting ? <BusyMark label="Grading" /> : null}
      </p>
    </form>
  );
}

function describeProblems(queue: ReviewQueue, entries: Record<string, Entry>): string | null {
  const unanswered = queue.questions.filter(
    (q) => (entries[q.questionId]?.answer ?? "").trim() === "",
  ).length;
  if (unanswered > 0) {
    return `${unanswered} question${unanswered === 1 ? "" : "s"} still unanswered.`;
  }

  const unreadable = queue.questions.filter(
    (q) =>
      q.format === "numeric" && parseFigure(entries[q.questionId]?.answer ?? "") === null,
  ).length;
  if (unreadable > 0) {
    return `${unreadable} typed answer${
      unreadable === 1 ? "" : "s"
    } couldn't be read as a number. Enter a figure like -4.2.`;
  }

  const unrated = queue.questions.filter(
    (q) => (entries[q.questionId]?.confidencePercent ?? null) === null,
  ).length;
  if (unrated > 0) {
    return `${unrated} question${unrated === 1 ? "" : "s"} still need a confidence.`;
  }

  return null;
}

function ReviewCard({
  question,
  entry,
  onChange,
  disabled,
}: {
  question: ReviewQuestion;
  entry: Entry;
  onChange: (next: Entry) => void;
  disabled: boolean;
}) {
  const inputName = `${question.documentId}-${question.questionId}`;

  return (
    <fieldset>
      <legend>
        {question.prompt} ({SKILL_LABELS[question.skill]} · {question.documentTitle}
        {describeAge(question.sourceDatedAt)} ·{" "}
        {question.reps === 0 ? "first review" : `review ${question.reps + 1}`})
      </legend>

      {question.format === "multiple_choice" ? (
        // Rendered in the order the server sent, which is this repetition's
        // order and not the stored one. Sorting or re-lettering here would
        // silently grade every answer against the wrong option.
        question.options.map((option, optionIndex) => {
          const inputId = `${inputName}-${option.id}`;
          return (
            <div key={option.id}>
              <input
                type="radio"
                id={inputId}
                name={inputName}
                value={option.id}
                checked={entry.answer === option.id}
                disabled={disabled}
                onChange={() => onChange({ ...entry, answer: option.id })}
              />
              <label htmlFor={inputId}>
                {OPTION_LETTERS[optionIndex] ?? "?"}. {option.text}
              </label>
            </div>
          );
        })
      ) : (
        <div>
          <label htmlFor={`${inputName}-value`}>
            Figure{question.unit ? ` (${question.unit})` : ""}
          </label>{" "}
          <input
            type="text"
            inputMode="decimal"
            id={`${inputName}-value`}
            name={inputName}
            value={entry.answer}
            disabled={disabled}
            autoComplete="off"
            onChange={(event) => onChange({ ...entry, answer: event.target.value })}
          />{" "}
          {question.tolerance === null ? null : (
            <span>within {formatTolerance(question.tolerance, question.unit ?? "")}</span>
          )}
        </div>
      )}

      <ConfidenceSlider
        idPrefix={inputName}
        format={question.format}
        percent={entry.confidencePercent}
        onChange={(confidencePercent) => onChange({ ...entry, confidencePercent })}
        disabled={disabled}
      />
    </fieldset>
  );
}

function Results({
  result,
  onAgain,
}: {
  result: ReviewSubmitResult;
  onAgain: () => void;
}) {
  return (
    <>
      <h2 role="status">
        {result.correct} of {result.total} correct · {result.points} of{" "}
        {result.maxPoints} points
      </h2>
      <p>
        Each answer moved that question&apos;s schedule. The interval beside each
        one is what it bought — a confident right answer pushes it further out
        than a hesitant one, and forgetting pulls it back.
      </p>
      <p>
        <button type="button" onClick={onAgain}>
          Check for more
        </button>
      </p>

      <ol>
        {result.results.map((graded) => (
          <li key={`${graded.documentId}-${graded.questionId}`}>
            <h3>{graded.prompt}</h3>
            <p>
              {graded.correct ? "Correct" : "Incorrect"}
              {graded.confidencePercent === null
                ? graded.confidence
                  ? ` after saying ${CONFIDENCE_LABELS[graded.confidence].toLowerCase()}`
                  : ""
                : ` after saying ${graded.confidencePercent}%`}{" "}
              ({graded.points >= 0 ? "+" : ""}
              {graded.points}) — back in {graded.intervalDays} day
              {graded.intervalDays === 1 ? "" : "s"}, on{" "}
              {new Date(graded.nextDueAt).toLocaleDateString()}
            </p>
            <Answer graded={graded} />
            {graded.explanation ? <p>{graded.explanation}</p> : null}
          </li>
        ))}
      </ol>
    </>
  );
}

function Answer({ graded }: { graded: ReviewResult }) {
  if (graded.correctValue !== null) {
    const unit = graded.unit ? ` ${graded.unit}` : "";
    return (
      <p>
        You said {graded.selectedValue === null ? "nothing" : graded.selectedValue}
        {unit}. The document says {formatFigure(graded.correctValue)}
        {unit}.
      </p>
    );
  }

  return (
    <ul>
      {graded.options.map((option, optionIndex) => {
        const isAnswer = option.id === graded.correctOptionId;
        const isChosen = option.id === graded.selectedOptionId;
        return (
          <li key={option.id}>
            {OPTION_LETTERS[optionIndex] ?? "?"}. {option.text}
            {isAnswer ? " — correct answer" : ""}
            {isChosen && !isAnswer ? " — you picked this" : ""}
          </li>
        );
      })}
    </ul>
  );
}
