import { useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "@tanstack/react-router";
import { useId, useState } from "react";
import { BusyMark, describeError } from "../components/ui";
import { api, uploadToS3 } from "../lib/api";
import { inspectPdf, MAX_PAGES } from "../lib/pdf";
import { queryKeys } from "../lib/queries";

type Phase = "inspecting" | "creating" | "uploading";

/**
 * Where one file has got to.
 *
 * `rejected` and `failed` are deliberately not the same state. A rejected file
 * never left the browser and cost nothing; a failed one may have a `pending`
 * document row behind it that the documents list will show. That is the
 * difference between "fix the file" and "go look at the list", and collapsing
 * both into one error state would lose it.
 */
type ItemStatus =
  | { kind: "waiting" }
  | { kind: "working"; phase: Phase }
  | { kind: "done" }
  | { kind: "rejected"; reason: string }
  | { kind: "failed"; error: unknown };

type Item = {
  /**
   * Position in the selection, and the React key.
   *
   * Not the filename: picking the same report out of two folders is an ordinary
   * thing to do, and duplicate keys would silently render one row where there
   * are two files in flight.
   */
  index: number;
  name: string;
  status: ItemStatus;
};

type UploadOutcome = { ok: true } | { ok: false; error: unknown };

/**
 * How many files are inspected and uploaded at once.
 *
 * Small, and each reason is a different resource:
 *
 *   - `inspectPdf` parses the whole document with pdf-lib on the main thread,
 *     holding an ArrayBuffer of the file while it does. Unbounded, choosing a
 *     folder of twenty reports is twenty simultaneous copies and a frozen tab.
 *   - `POST /docs` is one Lambda invocation and one `PutItem` per file, against
 *     a table provisioned at 5 WCU and an account concurrency limit of 10 that
 *     the api and generate functions share.
 *
 * The S3 events that follow are *not* bounded by this and do not need to be:
 * Lambda queues asynchronous invocations, and `DAILY_DOCUMENT_CAP` is what
 * bounds the spend — see the note on the generate function in `infra/lambda.tf`,
 * which names a burst of uploads as exactly the case that cap exists for.
 */
const UPLOAD_CONCURRENCY = 3;

/**
 * The two-step upload lives outside the component on purpose. React Compiler
 * bails on any component containing try/catch-with-throw, and this function is
 * the only place that needs one — keeping it at module scope means UploadScreen
 * still gets compiled.
 */
async function performUpload(
  file: File,
  pageCount: number,
  onPhase: (phase: Phase) => void,
): Promise<UploadOutcome> {
  try {
    onPhase("creating");
    const created = await api.createDocument({
      // A placeholder the server keeps only until the model supplies a real
      // title from the document's own cover.
      filename: file.name,
      pageCount,
      contentType: "application/pdf",
      sizeBytes: file.size,
    });

    // uploadUrl is null only on a retry, which this screen never issues. The
    // check keeps the nullable contract honest instead of asserting past it.
    if (!created.uploadUrl) {
      return {
        ok: false,
        error: new Error("The API did not return an upload URL for a new document."),
      };
    }

    onPhase("uploading");
    await uploadToS3(created.uploadUrl, file);
    return { ok: true };
  } catch (error) {
    return { ok: false, error };
  }
}

/**
 * Inspect and upload every file, at most [`UPLOAD_CONCURRENCY`] at a time,
 * reporting each one's status as it changes and returning where they all ended.
 *
 * **One file's failure never stops the others.** `performUpload` returns its
 * outcome rather than throwing and a rejected inspection is a result too, so
 * there is nothing here that can reject and abandon the rest of the batch. Ten
 * files where the fourth is corrupt uploads the other nine, which is the only
 * behaviour that makes selecting a folder worth doing.
 *
 * The terminal statuses are *returned* rather than read back out of component
 * state, because the caller needs them the instant this resolves and the
 * `setItems` updates it has been feeding have not committed by then.
 */
async function runBatch(
  files: readonly File[],
  onStatus: (index: number, status: ItemStatus) => void,
): Promise<ItemStatus[]> {
  const settled = Array.from(
    { length: files.length },
    (): ItemStatus => ({ kind: "waiting" }),
  );

  // A shared cursor rather than slicing the list into per-worker chunks, so a
  // batch of small files does not sit waiting on the one worker that happened
  // to draw the ninety-page report. Incrementing it needs no lock: `next++` is
  // synchronous and nothing yields between its read and its write.
  let next = 0;

  // `no-await-in-loop` is off for this function, and its advice is exactly
  // backwards here: awaiting one file before drawing the next is *how* the
  // batch stays bounded. The parallelism is the fixed pool of workers below,
  // not a `Promise.all` over every file at once — which is the unbounded
  // version this whole function exists to avoid.
  // oxlint-disable no-await-in-loop
  async function worker(): Promise<void> {
    for (;;) {
      const index = next++;
      const file = files[index];
      if (!file) return;

      // The page check runs before anything is sent, so an over-long PDF costs
      // no API call. It is a courtesy, not a guard — `generate` counts the pages
      // again from the bytes, because this number comes from the client.
      onStatus(index, { kind: "working", phase: "inspecting" });
      const checked = await inspectPdf(file);

      if (!checked.ok) {
        const rejected: ItemStatus = { kind: "rejected", reason: checked.reason };
        settled[index] = rejected;
        onStatus(index, rejected);
        continue;
      }

      const outcome = await performUpload(file, checked.pageCount, (phase) => {
        onStatus(index, { kind: "working", phase });
      });

      const status: ItemStatus = outcome.ok
        ? { kind: "done" }
        : { kind: "failed", error: outcome.error };
      settled[index] = status;
      onStatus(index, status);
    }
  }
  // oxlint-enable no-await-in-loop

  await Promise.all(
    Array.from({ length: Math.min(UPLOAD_CONCURRENCY, files.length) }, () => worker()),
  );

  return settled;
}

const PHASE_LABEL: Record<Phase, string> = {
  inspecting: "checking the page count…",
  creating: "requesting upload…",
  uploading: "uploading…",
};

/** The one line describing where a file got to. */
function statusLabel(status: ItemStatus): string {
  switch (status.kind) {
    case "waiting":
      return "waiting";
    case "working":
      return PHASE_LABEL[status.phase];
    case "done":
      return "uploaded";
    case "rejected":
      return status.reason;
    case "failed":
      return describeError(status.error);
  }
}

/**
 * Pick PDFs. That is the whole screen.
 *
 * It used to ask for a title and at least one topic before it would accept the
 * file. Both are now read from the document by the model, which knows what the
 * document is called and what it is about far better than someone who has not
 * read it yet — and it removes the form entirely. There is nothing to submit
 * except the files, so there is no form: choosing PDFs starts the uploads.
 *
 * It also used to take exactly one file, which made a habit of reading in
 * batches — a publisher's quarter, a run of monthly reports — into one trip
 * through the file picker per document. Nothing about the pipeline was
 * single-file; only this screen was.
 */
export function UploadScreen() {
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const fileId = useId();

  const [items, setItems] = useState<readonly Item[]>([]);
  const [busy, setBusy] = useState(false);

  async function onFilesChange(selected: readonly File[]) {
    if (selected.length === 0) return;

    setItems(
      selected.map((file, index) => ({
        index,
        name: file.name,
        status: { kind: "waiting" },
      })),
    );
    setBusy(true);

    const settled = await runBatch(selected, (index, status) => {
      setItems((current) =>
        current.map((item) => (item.index === index ? { ...item, status } : item)),
      );
    });

    setBusy(false);

    const uploaded = settled.filter((status) => status.kind === "done").length;

    // One invalidation for the batch rather than one per file. Generation is
    // kicked off by the S3 object-created event, so there is nothing further to
    // call, and the list polls itself from here — ten invalidations would be ten
    // list calls competing for the concurrency the generate function is using.
    if (uploaded > 0) {
      await queryClient.invalidateQueries({ queryKey: queryKeys.documents });
    }

    // Leaving is only right when nothing on this screen is still worth reading.
    // A rejected or failed file's reason exists nowhere else — the documents
    // list never heard about it — so navigating away on a partial batch would
    // discard the only account of what went wrong.
    if (uploaded === settled.length) {
      await navigate({ to: "/docs" });
    }
  }

  const uploaded = items.filter((item) => item.status.kind === "done").length;

  return (
    <section>
      <h1>Upload PDFs</h1>
      <p>
        Up to {MAX_PAGES} pages each. The title and topics are read from each
        document, and ten questions are generated from it — that takes 30–90
        seconds after the upload finishes. Choose as many as you like: they are
        uploaded a few at a time, and anything past the day&apos;s generation
        limit comes back as a failed document you can retry tomorrow.
      </p>

      <p>
        <label htmlFor={fileId}>PDFs</label>
        <br />
        <input
          id={fileId}
          type="file"
          accept="application/pdf,.pdf"
          multiple
          disabled={busy}
          onChange={(event) => void onFilesChange(Array.from(event.target.files ?? []))}
        />
      </p>

      {items.length > 0 ? (
        <>
          <p aria-live="polite">
            {busy ? (
              <>
                <BusyMark label="Uploading" />{" "}
              </>
            ) : null}
            {uploaded} of {items.length} uploaded
            {busy ? "…" : "."}
          </p>

          {/*
            An ordered list, in the order they were chosen, so a row keeps its
            place while its status changes underneath it. Sorting finished files
            to the top would move the one you were reading about.
          */}
          <ol>
            {items.map((item) => (
              <li key={item.index}>
                {item.name} — {statusLabel(item.status)}
              </li>
            ))}
          </ol>
        </>
      ) : null}
    </section>
  );
}
