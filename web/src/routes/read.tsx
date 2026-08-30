import { useForm } from "@tanstack/react-form";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useParams } from "@tanstack/react-router";
import { Busy, BusyMark, ErrorNotice } from "../components/ui";
import { api } from "../lib/api";
import { documentUrlQuery, queryKeys, quizQuery } from "../lib/queries";
import {
  SKILL_LABELS,
  TOPIC_LABELS,
  type AttemptResult,
  type Quiz,
  type QuizQuestion,
} from "../lib/schemas";

const OPTION_LETTERS = ["A", "B", "C", "D"] as const;

export function ReadScreen() {
  const { documentId } = useParams({ from: "/_auth/docs/$documentId" });
  const queryClient = useQueryClient();

  // Two requests in flight at once, and that is the app's ceiling. The api and
  // generate Lambdas share an account concurrency limit of 10, so nothing here
  // fans out per-question or per-document: the document list is one polled call,
  // and this screen is these two.
  const url = useQuery(documentUrlQuery(documentId));
  const quiz = useQuery(quizQuery(documentId));

  const submit = useMutation({
    mutationFn: (answers: Record<string, string>) =>
      api.submitQuiz(documentId, {
        answers: Object.entries(answers).map(([questionId, optionId]) => ({
          questionId,
          optionId,
        })),
      }),
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
          onSubmit={(answers) => submit.mutate(answers)}
        />
      )}
    </section>
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
  onSubmit: (answers: Record<string, string>) => void;
}) {
  const form = useForm({
    defaultValues: Object.fromEntries(
      quiz.questions.map((question) => [question.id, ""]),
    ) as Record<string, string>,
    validators: {
      // Validated on the whole form, not per question: the quiz is submitted in
      // one shot, so a half-finished form is one error ("3 unanswered"), not ten.
      onSubmit: ({ value }) => {
        const unanswered = quiz.questions.filter((q) => !value[q.id]).length;
        return unanswered > 0
          ? {
              form: `${unanswered} question${unanswered === 1 ? "" : "s"} still unanswered.`,
              fields: {},
            }
          : null;
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
        {quiz.questions.map((question) => (
          <li key={question.id}>
            <form.Field name={question.id}>
              {(field) => (
                <QuestionCard
                  question={question}
                  selectedOptionId={field.state.value}
                  onSelect={(optionId) => field.handleChange(optionId)}
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

function QuestionCard({
  question,
  selectedOptionId,
  onSelect,
  disabled,
}: {
  question: QuizQuestion;
  selectedOptionId: string;
  onSelect: (optionId: string) => void;
  disabled: boolean;
}) {
  return (
    // One fieldset per question, with the prompt as the legend. This is what makes
    // the radio group navigable: the legend is announced with every option, so the
    // reader always knows which question the choices belong to.
    <fieldset>
      <legend>
        {question.prompt} ({SKILL_LABELS[question.skill]} ·{" "}
        {question.topics.map((topic) => TOPIC_LABELS[topic]).join(", ")})
      </legend>
      {question.options.map((option, optionIndex) => {
        const inputId = `${question.id}-${option.id}`;
        return (
          <div key={option.id}>
            <input
              type="radio"
              id={inputId}
              name={question.id}
              value={option.id}
              checked={selectedOptionId === option.id}
              disabled={disabled}
              onChange={() => onSelect(option.id)}
            />
            <label htmlFor={inputId}>
              {OPTION_LETTERS[optionIndex] ?? "?"}. {option.text}
            </label>
          </div>
        );
      })}
    </fieldset>
  );
}

function Results({ result }: { result: AttemptResult }) {
  return (
    <>
      <h3 role="status">
        {result.correct} of {result.total} correct
      </h3>
      <p>
        One attempt is a fact, not a rate — the <Link to="/history">history view</Link>{" "}
        is where the rates live, and it won&apos;t quote one until there are enough
        observations behind it.
      </p>

      <ol>
        {result.questions.map((graded) => (
          <li key={graded.questionId}>
            <h4>{graded.prompt}</h4>
            <p>
              {graded.correct ? "Correct" : "Incorrect"} — {SKILL_LABELS[graded.skill]} ·{" "}
              {graded.topics.map((topic) => TOPIC_LABELS[topic]).join(", ")}
            </p>
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
            {graded.explanation ? <p>{graded.explanation}</p> : null}
          </li>
        ))}
      </ol>
    </>
  );
}
