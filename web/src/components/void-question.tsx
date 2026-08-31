import { useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { api } from "../lib/api";
import { queryKeys } from "../lib/queries";
import { ErrorNotice } from "./ui";

/**
 * Withdraw a question that cannot be answered correctly.
 *
 * # Why it lives on the results screen
 *
 * Because that is the first moment the reader can tell. The key and the
 * explanation are hidden until then by design, and a question with two
 * defensible options looks perfectly reasonable while you are answering it —
 * the whole problem is that you pick the defensible one and find out
 * afterwards.
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
}: {
  documentId: string;
  questionId: string;
}) {
  const queryClient = useQueryClient();
  const [open, setOpen] = useState(false);
  const [reason, setReason] = useState("");

  const voidIt = useMutation({
    mutationFn: () =>
      api.voidQuestion(documentId, {
        questionId,
        ...(reason.trim() === "" ? {} : { reason: reason.trim() }),
      }),
    onSuccess: () => {
      // The question is gone from the quiz, from the schedule, and from every
      // rate derived from past attempts — so everything that reads any of
      // those is stale.
      void queryClient.invalidateQueries({ queryKey: queryKeys.quiz(documentId) });
      void queryClient.invalidateQueries({ queryKey: queryKeys.reviewQueue });
      void queryClient.invalidateQueries({ queryKey: queryKeys.history });
      void queryClient.invalidateQueries({ queryKey: queryKeys.documents });
    },
  });

  if (voidIt.isSuccess) {
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
        disabled={voidIt.isPending}
        onChange={(event) => setReason(event.target.value)}
      />{" "}
      <button type="button" disabled={voidIt.isPending} onClick={() => voidIt.mutate()}>
        {voidIt.isPending ? "Voiding…" : "Void"}
      </button>{" "}
      <button type="button" disabled={voidIt.isPending} onClick={() => setOpen(false)}>
        Cancel
      </button>
      {voidIt.error ? <ErrorNotice error={voidIt.error} /> : null}
    </div>
  );
}
