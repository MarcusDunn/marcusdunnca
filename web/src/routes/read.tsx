import { useEffect, useRef } from "react";
import { useForm } from "@tanstack/react-form";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useParams } from "@tanstack/react-router";
import { Busy, BusyMark, ErrorNotice } from "../components/ui";
import { api } from "../lib/api";
import { formatFigure, formatTolerance, parseFigure } from "../lib/figures";
import { documentUrlQuery, queryKeys, quizQuery } from "../lib/queries";
import { ConfidenceSlider, signed } from "../components/confidence";
import { VoidQuestion } from "../components/void-question";
import {
  CHANCE_FLOOR_PERCENT,
  CONFIDENCE_LABELS,
  MAX_PERCENT,
  scoreBits,
  SKILL_LABELS,
  topicLabel,
  type AttemptResult,
  type QuestionFormat,
  type GradedQuestion,
  type Quiz,
  type QuizQuestion,
} from "../lib/schemas";

const OPTION_LETTERS = ["A", "B", "C", "D"] as const;

/**
 * One question's state: what was answered, and how sure of it.
 *
 * The two are one unit because neither is submittable without the other. A
 * confidence with no answer prices nothing, and an answer with no confidence is
 * exactly the pre-calibration data this change exists to stop collecting.
 *
 * `answer` is a string for both formats — an option id, or the raw text of a
 * typed figure. Parsing happens at the boundary, not in form state, so what the
 * reader sees in the box is always what they typed.
 */
type Entry = { answer: string; confidencePercent: number | null };

const EMPTY: Entry = { answer: "", confidencePercent: null };

export function ReadScreen() {
  const { documentId } = useParams({ from: "/_auth/docs/$documentId" });
  const queryClient = useQueryClient();

  // Two requests in flight at once, and that is the app's ceiling. The api and
  // generate Lambdas share an account concurrency limit of 10, so nothing here
  // fans out per-question or per-document: the document list is one polled call,
  // and this screen is these two.
  const url = useQuery(documentUrlQuery(documentId));
  const quiz = useQuery(quizQuery(documentId));

  // Both of these are refs rather than memos because crypto.randomUUID() and
  // Date.now() are impure and must not run during render — a re-render would
  // silently produce a different id, which is exactly the property that has to
  // hold. The React Compiler's purity lint catches this; it was right.
  //
  // The id is minted on the FIRST submit and reused by every retry of that
  // submit, so a double-tap on a flaky connection is recognised server-side as
  // a duplicate instead of writing a second attempt and skewing every rate.
  const attemptIdRef = useRef<string | null>(null);
  const startedAtRef = useRef<number | null>(null);

  // Effects may be impure. The clock starts when the questions actually appear,
  // not when the route mounts, so a slow quiz fetch is not counted as reading.
  useEffect(() => {
    if (quiz.data && startedAtRef.current === null) {
      startedAtRef.current = Date.now();
    }
  }, [quiz.data]);

  const submit = useMutation({
    mutationFn: (entries: Record<string, Entry>) => {
      // Written long-hand rather than with `??=`. React Compiler does not yet
      // support logical assignment and bails on the *whole component* when it
      // meets one — silently, as a build note — so this one operator was
      // opting the quiz screen out of compilation entirely.
      if (attemptIdRef.current === null) {
        attemptIdRef.current = crypto.randomUUID();
      }
      const startedAt = startedAtRef.current;
      const questions = quiz.data?.questions ?? [];

      return api.submitQuiz(documentId, {
        attemptId: attemptIdRef.current,
        ...(startedAt === null ? {} : { durationMs: Date.now() - startedAt }),
        // Built from the questions rather than from the entries, because the
        // shape of each answer depends on the question's format and the server
        // rejects the wrong one rather than ignoring it.
        answers: questions.map((question) => {
          const entry = entries[question.id] ?? EMPTY;
          const questionId = question.id;
          // The form refuses to submit with any band unset, so the fallback is
          // unreachable — and is the honest one if it ever is reached.
          const confidencePercent =
            entry.confidencePercent ?? CHANCE_FLOOR_PERCENT[question.format];

          // Two whole literals rather than one with a conditional spread. The
          // server rejects the wrong key for a question's format rather than
          // ignoring it, so which key is present is the meaningful part and it
          // should be readable as such.
          return question.format === "multiple_choice"
            ? { questionId, optionId: entry.answer, confidencePercent }
            : { questionId, value: entry.answer, confidencePercent };
        }),
      });
    },
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: queryKeys.history });
      void queryClient.invalidateQueries({ queryKey: queryKeys.documents });
    },
  });

  return (
    <section>
      <h1>Read &amp; quiz</h1>
      <p>
        <Link to="/docs">All documents</Link>
      </p>

      <h2>Document</h2>
      {url.isPending ? (
        <Busy label="Fetching the document" />
      ) : url.isError ? (
        <ErrorNotice error={url.error} onRetry={() => void url.refetch()} />
      ) : (
        <>
          {/*
            An <embed> hands the file to the browser's own PDF viewer. Shipping
            pdf.js would mean owning text layers, worker bundling and rendering
            bugs for a reader that already exists on every target device.

            width/height are HTML presentational attributes, not CSS — without
            them an <embed> defaults to 300x150, which isn't a reader.

            The src is a presigned URL that expires; we don't refresh it on a
            timer because changing src remounts the plugin and dumps the reader
            back to page one mid-quiz. The link and reload button below are the
            escape hatches.
          */}
          <embed src={url.data.url} type="application/pdf" width="100%" height="600" />
          <p>
            <a href={url.data.url} target="_blank" rel="noreferrer">
              Open in a new tab
            </a>{" "}
            — iOS Safari often won&apos;t scroll an inline PDF.
          </p>
          <p>
            <button
              type="button"
              onClick={() => {
                void queryClient.invalidateQueries({
                  queryKey: queryKeys.documentUrl(documentId),
                });
              }}
            >
              Reload document
            </button>{" "}
            if it stops loading — the link is short-lived.
          </p>
        </>
      )}

      <h2>Questions</h2>
      {quiz.isPending ? (
        <Busy label="Loading questions" />
      ) : quiz.isError ? (
        <ErrorNotice error={quiz.error} onRetry={() => void quiz.refetch()} />
      ) : submit.data ? (
        <Results result={submit.data} />
      ) : (
        <QuizForm
          quiz={quiz.data}
          submitting={submit.isPending}
          error={submit.error}
          onSubmit={(entries) => submit.mutate(entries)}
        />
      )}
    </section>
  );
}

/**
 * The scoring rule, stated once at the top of the quiz.
 *
 * On the page rather than behind a help link on purpose. A price revealed only
 * afterwards teaches nothing about the decision that was made — and the slider
 * shows the live figure beside every question, so this is the explanation of a
 * number the reader is already watching move.
 */
function ScoringNote() {
  const mc: QuestionFormat = "multiple_choice";
  const rows = [25, 50, 75, 90, MAX_PERCENT];

  return (
    <details>
      <summary>How the confidence slider is scored</summary>
      <p>
        Every question asks for a probability as well as an answer, and the
        score moves continuously with it — there are no bands and no cliffs.
        The rule is arranged so the best score comes from stating what you
        actually believe: overstating and understating both cost you, so there
        is nothing to be gained by playing the scale.
      </p>
      <p>
        The unit is <strong>bits of information over chance</strong>. Saying 25%
        on a four-option question scores exactly zero whichever way it goes —
        that is what guessing is worth, and admitting it is free. Everything
        above that is what your confidence added; everything below is what it
        cost.
      </p>
      <table>
        <caption>
          A four-option question. A typed figure is worth more, because there is
          more uncertainty to remove.
        </caption>
        <thead>
          <tr>
            <th scope="col">You say</th>
            <th scope="col">If right</th>
            <th scope="col">If wrong</th>
          </tr>
        </thead>
        <tbody>
          {rows.map((percent) => (
            <tr key={percent}>
              <th scope="row">{percent}%</th>
              <td>{signed(scoreBits(percent, true, mc))}</td>
              <td>{signed(scoreBits(percent, false, mc))}</td>
            </tr>
          ))}
        </tbody>
      </table>
      <p>
        Notice the asymmetry at the bottom of the table: being sure and wrong
        costs about three times what being sure and right earns. That is not a
        thumb on the scale — it falls out of the arithmetic, and it is the
        reason the rule is worth taking seriously. A confident error is the one
        that would have been said out loud.
      </p>
      <p>
        The slider stops at {MAX_PERCENT}%. Certainty about a figure you read
        once is never warranted, and a scale that offered it would invite a
        claim nobody should make.
      </p>
    </details>
  );
}

function QuizForm({
  quiz,
  submitting,
  error,
  onSubmit,
}: {
  quiz: Quiz;
  submitting: boolean;
  error: unknown;
  onSubmit: (entries: Record<string, Entry>) => void;
}) {
  const form = useForm({
    defaultValues: Object.fromEntries(
      quiz.questions.map((question) => [question.id, EMPTY]),
    ) as Record<string, Entry>,
    validators: {
      // Validated on the whole form, not per question: the quiz is submitted in
      // one shot, so a half-finished form is one error ("3 unanswered"), not ten.
      onSubmit: ({ value }) => {
        const problem = describeProblems(quiz, value);
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
      <ScoringNote />

      <ol>
        {quiz.questions.map((question) => (
          <li key={question.id}>
            <form.Field name={question.id}>
              {(field) => (
                <QuestionCard
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
          {submitting ? "Grading…" : `Submit all ${quiz.questions.length} answers`}
        </button>{" "}
        {submitting ? <BusyMark label="Grading" /> : null}
      </p>
    </form>
  );
}

/**
 * The one sentence to show above the submit button, or null if there is none.
 *
 * Ordered by what the reader should fix first. Three separate messages would
 * be three things to read at the bottom of a long form; one at a time is the
 * shape that actually gets acted on.
 */
function describeProblems(quiz: Quiz, entries: Record<string, Entry>): string | null {
  const unanswered = quiz.questions.filter(
    (q) => (entries[q.id]?.answer ?? "").trim() === "",
  ).length;
  if (unanswered > 0) {
    return `${unanswered} question${unanswered === 1 ? "" : "s"} still unanswered.`;
  }

  // Checked before the confidence, because retyping a figure is a smaller
  // interruption than reconsidering how sure you were.
  const unreadable = quiz.questions.filter(
    (q) => q.format === "numeric" && parseFigure(entries[q.id]?.answer ?? "") === null,
  );
  if (unreadable.length > 0) {
    return `${unreadable.length} typed answer${
      unreadable.length === 1 ? "" : "s"
    } couldn't be read as a number. Enter a figure like -4.2 — a range or a word won't score.`;
  }

  const unrated = quiz.questions.filter(
    (q) => (entries[q.id]?.confidencePercent ?? null) === null,
  ).length;
  if (unrated > 0) {
    return `${unrated} question${
      unrated === 1 ? "" : "s"
    } still need${unrated === 1 ? "s" : ""} a confidence.`;
  }

  return null;
}

function QuestionCard({
  question,
  entry,
  onChange,
  disabled,
}: {
  question: QuizQuestion;
  entry: Entry;
  onChange: (next: Entry) => void;
  disabled: boolean;
}) {
  return (
    // One fieldset per question, with the prompt as the legend. This is what makes
    // the radio group navigable: the legend is announced with every option, so the
    // reader always knows which question the choices belong to.
    <fieldset>
      <legend>
        {question.prompt} ({SKILL_LABELS[question.skill]} ·{" "}
        {question.topics.map(topicLabel).join(", ")})
      </legend>

      {question.format === "multiple_choice" ? (
        question.options.map((option, optionIndex) => {
          const inputId = `${question.id}-${option.id}`;
          return (
            <div key={option.id}>
              <input
                type="radio"
                id={inputId}
                name={question.id}
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
          <label htmlFor={`${question.id}-value`}>
            Figure{question.unit ? ` (${question.unit})` : ""}
          </label>{" "}
          <input
            // Not `type="number"`. A number input silently discards what it
            // cannot parse as you type, so "(1.2)" and "-4.0%" — the shapes the
            // document actually prints — vanish keystroke by keystroke.
            // `inputMode` still brings up the numeric keyboard on a phone.
            type="text"
            inputMode="decimal"
            id={`${question.id}-value`}
            name={question.id}
            value={entry.answer}
            disabled={disabled}
            autoComplete="off"
            onChange={(event) => onChange({ ...entry, answer: event.target.value })}
          />{" "}
          <span>
            within {formatTolerance(question.tolerance, question.unit)} — the
            trend matters, not the decimal
          </span>
        </div>
      )}

      <ConfidenceSlider
        idPrefix={question.id}
        format={question.format}
        percent={entry.confidencePercent}
        onChange={(confidencePercent) => onChange({ ...entry, confidencePercent })}
        disabled={disabled}
      />
    </fieldset>
  );
}

function Results({ result }: { result: AttemptResult }) {
  const confidentErrors = result.questions.filter(
    (q) => q.confidence === "certain" && !q.correct,
  );

  return (
    <>
      <h3 role="status">
        {result.correct} of {result.total} correct · {signed(result.scoreBits)} of{" "}
        {result.maxScoreBits.toFixed(2)} bits
      </h3>
      <p>
        Two numbers because they answer two questions. The first is how much you
        knew; the second is how well you knew what you knew — it drops when you
        were sure and wrong, and it barely moves when you admit a guess.
      </p>
      <p>
        One attempt is a fact, not a rate — the <Link to="/history">history view</Link>{" "}
        is where the rates live, and it won&apos;t quote one until there are enough
        observations behind it.
      </p>

      {confidentErrors.length > 0 ? (
        <>
          <h4>Sure, and wrong</h4>
          <p>
            {confidentErrors.length === 1
              ? "One answer you"
              : `${confidentErrors.length} answers you`}{" "}
            would have stated on the record. These are the ones worth rereading
            — and, as it happens, the ones that stick once corrected.
          </p>
          <ul>
            {confidentErrors.map((graded) => (
              <li key={graded.questionId}>{graded.prompt}</li>
            ))}
          </ul>
        </>
      ) : null}

      <ol>
        {result.questions.map((graded) => (
          <li key={graded.questionId}>
            <h4>{graded.prompt}</h4>
            <p>
              {graded.correct ? "Correct" : "Incorrect"}
              {graded.confidencePercent === null
                ? graded.confidence
                  ? ` after saying ${CONFIDENCE_LABELS[graded.confidence].toLowerCase()}`
                  : ""
                : ` after saying ${graded.confidencePercent}%`}{" "}
              ({signed(graded.scoreBits)} bits) — {SKILL_LABELS[graded.skill]} ·{" "}
              {graded.topics.map(topicLabel).join(", ")}
            </p>
            {graded.format === "multiple_choice" ? (
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
            ) : (
              <NumericVerdict graded={graded} />
            )}
            {graded.explanation ? <p>{graded.explanation}</p> : null}
            <VoidQuestion documentId={result.documentId} questionId={graded.questionId} />
          </li>
        ))}
      </ol>
    </>
  );
}

function NumericVerdict({ graded }: { graded: GradedQuestion }) {
  const unit = graded.unit ? ` ${graded.unit}` : "";
  return (
    <p>
      You said {graded.selectedValue === null ? "nothing" : graded.selectedValue}
      {unit}. The document says{" "}
      {graded.correctValue === null ? "—" : formatFigure(graded.correctValue)}
      {unit}
      {graded.tolerance === null
        ? ""
        : `, and anything within ${formatTolerance(graded.tolerance, graded.unit ?? "")} counted`}
      .
    </p>
  );
}
