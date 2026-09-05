import { useQuery } from "@tanstack/react-query";
import { Link, useParams } from "@tanstack/react-router";
import { Results } from "../components/results";
import { Busy, ErrorNotice } from "../components/ui";
import { signed } from "../components/confidence";
import { attemptQuery, attemptsQuery } from "../lib/queries";
import type { AttemptSummary } from "../lib/schemas";

/**
 * Every sitting of one document, newest first.
 *
 * # Why this hangs off the document rather than off history
 *
 * They answer different questions and mixing them would spoil both. The history
 * screen is deliberately a *rate* view — a skill × topic matrix that refuses to
 * quote a number until there are enough observations behind it — and one
 * sitting is a fact, not a rate. "How did I do on this report" is a question
 * about a document, and it is asked from the document.
 */
export function AttemptsScreen() {
  const { documentId } = useParams({ from: "/_auth/docs/$documentId/attempts" });
  const attempts = useQuery(attemptsQuery(documentId));

  if (attempts.isPending) return <Busy label="Loading attempts" />;
  if (attempts.isError) {
    return <ErrorNotice error={attempts.error} onRetry={() => void attempts.refetch()} />;
  }

  const rows = attempts.data.attempts;

  return (
    <section>
      <h1>{attempts.data.documentTitle}</h1>
      <p>
        <Link to="/docs/$documentId" params={{ documentId }}>
          Read it again
        </Link>{" "}
        · <Link to="/docs">All documents</Link>
      </p>

      {rows.length === 0 ? (
        <p>
          No attempts yet.{" "}
          <Link to="/docs/$documentId" params={{ documentId }}>
            Take the quiz
          </Link>{" "}
          and this is where it will be.
        </p>
      ) : (
        <>
          <p>
            Newest first. Open one to see the questions, what you said, and the
            answer key — and to void a question you can now tell was
            unanswerable.
          </p>
          <ol>
            {rows.map((attempt) => (
              <li key={attempt.attemptId}>
                <AttemptRow documentId={documentId} attempt={attempt} />
              </li>
            ))}
          </ol>
        </>
      )}
    </section>
  );
}

function AttemptRow({
  documentId,
  attempt,
}: {
  documentId: string;
  attempt: AttemptSummary;
}) {
  return (
    <>
      <Link
        to="/docs/$documentId/attempts/$attemptId"
        params={{ documentId, attemptId: attempt.attemptId }}
      >
        {new Date(attempt.submittedAt).toLocaleString()}
      </Link>{" "}
      — {attempt.correct} of {attempt.total} correct · {signed(attempt.scoreBits)} of{" "}
      {attempt.maxScoreBits.toFixed(2)} bits
      {attempt.voided > 0 ? (
        // Said here rather than left to be inferred from the denominator: an
        // attempt reading 7 of 9 was taken as a ten-question quiz, and without
        // this line the two numbers look like a different sitting entirely.
        <>
          {" "}
          <small>
            ({attempt.voided} {attempt.voided === 1 ? "question" : "questions"}{" "}
            voided since, and no longer counted)
          </small>
        </>
      ) : null}
    </>
  );
}

/**
 * One past sitting, graded, with the key.
 *
 * The same [`Results`] the submit screen renders. That is the point: the
 * answers you reread in a month are laid out exactly as they were on the day,
 * and the void control is in the same place — except that here it can also put
 * a question back.
 */
export function AttemptScreen() {
  const { documentId, attemptId } = useParams({
    from: "/_auth/docs/$documentId/attempts/$attemptId",
  });
  const attempt = useQuery(attemptQuery(documentId, attemptId));

  if (attempt.isPending) return <Busy label="Loading the attempt" />;
  if (attempt.isError) {
    return <ErrorNotice error={attempt.error} onRetry={() => void attempt.refetch()} />;
  }

  return (
    <section>
      <h1>{new Date(attempt.data.submittedAt).toLocaleString()}</h1>
      <p>
        <Link to="/docs/$documentId/attempts" params={{ documentId }}>
          All attempts at this document
        </Link>{" "}
        ·{" "}
        <Link to="/docs/$documentId" params={{ documentId }}>
          Read it again
        </Link>
      </p>
      <Results result={attempt.data} />
    </section>
  );
}
