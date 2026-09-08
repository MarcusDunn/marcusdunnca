import { Link } from "@tanstack/react-router";
import { formatFigure, formatTolerance } from "../lib/figures";
import { signed } from "./confidence";
import { VoidQuestion } from "./void-question";
import {
  CONFIDENCE_LABELS,
  SKILL_LABELS,
  topicLabel,
  type AttemptResult,
  type GradedQuestion,
} from "../lib/schemas";

export const OPTION_LETTERS = ["A", "B", "C", "D"] as const;

/**
 * A graded attempt: what was asked, what you said, what the document says.
 *
 * Shared by the screen you land on after submitting and the one that reads an
 * old attempt back, because they are the same view of the same thing at two
 * moments — and because a results screen that drifted between them would mean
 * the answers you reread in a month were laid out differently from the ones you
 * saw on the day.
 *
 * The one thing that legitimately differs is a withdrawn question. A fresh
 * submission cannot contain one, so `voided` is always false there; the
 * historical view shows them, marked, because that is where a void gets judged
 * and reversed.
 */
export function Results({ result }: { result: AttemptResult }) {
  const confidentErrors = result.questions.filter(
    (q) => q.confidence === "certain" && !q.correct && !q.voided,
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
            <GradedCard graded={graded} documentId={result.documentId} />
          </li>
        ))}
      </ol>
    </>
  );
}

function GradedCard({
  graded,
  documentId,
}: {
  graded: GradedQuestion;
  documentId: string;
}) {
  return (
    <>
      <h4>{graded.prompt}</h4>

      {/*
        Stated before the verdict, not after. A withdrawn question's "Incorrect"
        is not a fact about the reader, and reading it first is the thing this
        notice exists to prevent.
      */}
      {graded.voided ? <VoidNotice graded={graded} /> : null}

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

      <VoidQuestion
        documentId={documentId}
        questionId={graded.questionId}
        voided={graded.voided}
      />
    </>
  );
}

/**
 * Why this question no longer counts, and — if one was given — the note the
 * reader left themselves.
 *
 * The note is the entire reason voiding asks for a reason. Recording it and
 * never showing it back would make the audit trail theoretical.
 */
function VoidNotice({ graded }: { graded: GradedQuestion }) {
  return (
    <p role="note">
      <strong>Voided</strong>
      {graded.voidedAt ? ` on ${graded.voidedAt.slice(0, 10)}` : ""} — excluded
      from this attempt&apos;s score and from every rate in history, and not
      asked again.
      {graded.voidReason ? ` Reason given: “${graded.voidReason}”` : " No reason was recorded."}
    </p>
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
