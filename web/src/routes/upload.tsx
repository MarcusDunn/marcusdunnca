import { useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "@tanstack/react-router";
import { useId, useState } from "react";
import { BusyMark, ErrorNotice } from "../components/ui";
import { api, uploadToS3 } from "../lib/api";
import { inspectPdf, MAX_PAGES, type PdfInspection } from "../lib/pdf";
import { queryKeys } from "../lib/queries";

type Phase = "idle" | "inspecting" | "creating" | "uploading";

type UploadOutcome = { ok: true } | { ok: false; error: unknown };

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
 * Pick a PDF. That is the whole screen.
 *
 * It used to ask for a title and at least one topic before it would accept the
 * file. Both are now read from the document by the model, which knows what the
 * document is called and what it is about far better than someone who has not
 * read it yet — and it removes the form entirely. There is nothing to submit
 * except the file, so there is no form: choosing a PDF starts the upload.
 */
export function UploadScreen() {
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const fileId = useId();

  const [phase, setPhase] = useState<Phase>("idle");
  const [inspection, setInspection] = useState<PdfInspection | null>(null);
  const [submitError, setSubmitError] = useState<unknown>(null);

  async function onFileChange(selected: File | null) {
    setInspection(null);
    setSubmitError(null);
    if (!selected) return;

    // The page check runs before anything is sent, so an over-long PDF costs no
    // API call. It is a courtesy, not a guard — `generate` counts the pages
    // again from the bytes, because this number comes from the client.
    setPhase("inspecting");
    const checked = await inspectPdf(selected);
    setInspection(checked);

    if (!checked.ok) {
      setPhase("idle");
      return;
    }

    const outcome = await performUpload(selected, checked.pageCount, setPhase);

    if (!outcome.ok) {
      setSubmitError(outcome.error);
      setPhase("idle");
      return;
    }

    // Generation is kicked off by the S3 object-created event, so there is
    // nothing further to call. The document appears as `pending` under its
    // filename and the list polls it from there.
    await queryClient.invalidateQueries({ queryKey: queryKeys.documents });
    await navigate({ to: "/docs" });
  }

  const busy = phase !== "idle";

  return (
    <section>
      <h1>Upload a PDF</h1>
      <p>
        Up to {MAX_PAGES} pages. The title and topics are read from the document, and
        ten questions are generated from it — that takes 30–90 seconds after the
        upload finishes.
      </p>

      <p>
        <label htmlFor={fileId}>PDF</label>
        <br />
        <input
          id={fileId}
          type="file"
          accept="application/pdf,.pdf"
          disabled={busy}
          onChange={(event) => void onFileChange(event.target.files?.[0] ?? null)}
        />
      </p>

      {phase === "inspecting" ? (
        <p>
          <progress aria-label="Checking the page count" /> Checking the page count…
        </p>
      ) : null}

      {inspection && !inspection.ok ? <p role="alert">{inspection.reason}</p> : null}

      {phase === "creating" || phase === "uploading" ? (
        <p aria-live="polite">
          <BusyMark label="Uploading" />{" "}
          {phase === "creating" ? "Requesting upload…" : "Uploading…"}
        </p>
      ) : null}

      {submitError ? <ErrorNotice error={submitError} /> : null}
    </section>
  );
}
