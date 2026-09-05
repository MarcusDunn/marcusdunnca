import { useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { api } from "../lib/api";
import { queryKeys } from "../lib/queries";
import { ErrorNotice } from "./ui";

/**
 * Withdraw a question that cannot be answered correctly, or put one back.
 *
 * # Why it lives on the results screen
 *
 * Because that is the first moment the reader can tell. The key and the
 * explanation are hidden until then by design, and a question with two
 * defensible options looks perfectly reasonable while you are answering it —
 * the whole problem is that you pick the defensible one and find out
 * afterwards.
 *
 * It is not the *only* moment, which is why this also renders on a past
 * attempt. Noticing on the day requires having noticed on the day; more often
 * the tell is a question that keeps coming back in review feeling wrong, and by
 * then the sitting it came from is weeks old.
 *
 * # Why restoring is offered at all
 *
 * Because voiding is a judgement made in the minute after being marked wrong,
 * which is the worst available moment to make it. Without a way back, a void
 * decided in irritation is permanent and — since a voided question is dropped
 * from the quiz, the schedule and every rate — invisible from then on. The
 * server has always supported `voided: false`; nothing had ever asked for it.
 *
 * # Why there is no confirmation step
 *
 * Voiding is reversible, single-user, and costs nothing to get wrong. A
 * confirm dialog on a reversible action is friction that trains people to
 * click through dialogs. What the form *does* ask for is a reason, optionally —
 * not as a gate, but because "two defensible options, the other one is in the
 * Highlights" is the note that makes a void auditable six months later.
 */
export function VoidQuestion({
  documentId,
  questionId,
  voided = false,
}: {
  documentId: string;
  questionId: string;
  /**
   * Whether the question is *currently* withdrawn.
   *
   * Defaulted to false so the submit-results screen, where a voided question
   * can never appear, needs no change.
   */
  voided?: boolean;
}) {
  const queryClient = useQueryClient();
  const [open, setOpen] = useState(false);
  const [reason, setReason] = useState("");

  const change = useMutation({
    mutationFn: (next: { voided: boolean; reason?: string }) =>
      api.voidQuestion(documentId, {
        questionId,
        voided: next.voided,
        ...(next.reason === undefined ? {} : { reason: next.reason }),
      }),
    onSuccess: () => {
      // The question's presence in the quiz, in the schedule and in every rate
      // derived from past attempts all turn on this flag — so everything that
      // reads any of those is stale, in both directions.
      void queryClient.invalidateQueries({ queryKey: queryKeys.quiz(documentId) });
      void queryClient.invalidateQueries({ queryKey: queryKeys.reviewQueue });
      void queryClient.invalidateQueries({ queryKey: queryKeys.history });
      void queryClient.invalidateQueries({ queryKey: queryKeys.documents });
      // Scores are rewritten across every attempt that counted this question,
      // so the list of sittings and the one on screen are both out of date.
      void queryClient.invalidateQueries({ queryKey: queryKeys.attempts(documentId) });
    },
  });

  if (voided) {
    return (
      <p>
        <button
          type="button"
          disabled={change.isPending}
          onClick={() => change.mutate({ voided: false })}
        >
          {change.isPending ? "Restoring…" : "Restore this question"}
        </button>{" "}
        <small>
          It goes back into the quiz, back into every rate, and is rescheduled by
          the next attempt.
        </small>
        {change.error ? <ErrorNotice error={change.error} /> : null}
      </p>
    );
  }

  if (change.isSuccess) {
    return (
      <p role="status">
        Voided. It has been dropped from this attempt&apos;s score, from the
        review schedule, and from every rate in history — and it will not be
        asked again.
      </p>
    );
  }

  if (!open) {
    return (
      <p>
        <button type="button" onClick={() => setOpen(true)}>
          This question was unanswerable — void it
        </button>
      </p>
    );
  }

  return (
    <div>
      <label htmlFor={`void-${questionId}`}>
        Why? (optional — but the note is what makes this auditable later)
      </label>{" "}
      <input
        type="text"
        id={`void-${questionId}`}
        value={reason}
        maxLength={500}
        disabled={change.isPending}
        onChange={(event) => setReason(event.target.value)}
      />{" "}
      <button
        type="button"
        disabled={change.isPending}
        onClick={() =>
          change.mutate({
            voided: true,
            ...(reason.trim() === "" ? {} : { reason: reason.trim() }),
          })
        }
      >
        {change.isPending ? "Voiding…" : "Void"}
      </button>{" "}
      <button type="button" disabled={change.isPending} onClick={() => setOpen(false)}>
        Cancel
      </button>
      {change.error ? <ErrorNotice error={change.error} /> : null}
    </div>
  );
}
