import { useForm } from "@tanstack/react-form";
import { useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "@tanstack/react-router";
import { useId, useState } from "react";
import { BusyMark, ErrorNotice } from "../components/ui";
import { api, uploadToS3 } from "../lib/api";
import { inspectPdf, MAX_PAGES, type PdfInspection } from "../lib/pdf";
import { queryKeys } from "../lib/queries";
import { TOPICS, TOPIC_LABELS, type Topic } from "../lib/schemas";

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
  title: string,
  topics: readonly Topic[],
  sizeBytes: number,
  onPhase: (phase: Phase) => void,
): Promise<UploadOutcome> {
  try {
    onPhase("creating");
    const created = await api.createDocument({
      title,
      topics: [...topics],
      pageCount,
      contentType: "application/pdf",
      sizeBytes,
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

export function UploadScreen() {
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const fileId = useId();

  // The file and its inspection sit outside the form: a File isn't a form value
  // we ever want to serialize, and the page check has to run on selection rather
  // than on submit so an over-long PDF is rejected before the user types a title.
  const [file, setFile] = useState<File | null>(null);
  const [inspection, setInspection] = useState<PdfInspection | null>(null);
  const [phase, setPhase] = useState<Phase>("idle");
  const [submitError, setSubmitError] = useState<unknown>(null);

  async function onFileChange(selected: File | null) {
    setFile(selected);
    setInspection(null);
    setSubmitError(null);
    if (!selected) return;
    setPhase("inspecting");
    setInspection(await inspectPdf(selected));
    setPhase("idle");
  }

  const form = useForm({
    defaultValues: { title: "", topics: [] as Topic[] },
    onSubmit: async ({ value }) => {
      setSubmitError(null);
      if (!file || !inspection?.ok) return;

      const outcome = await performUpload(
        file,
        inspection.pageCount,
        value.title.trim(),
        value.topics,
        file.size,
        setPhase,
      );

      if (!outcome.ok) {
        setSubmitError(outcome.error);
        setPhase("idle");
        return;
      }

      // Generation is kicked off by the S3 object-created event, so there is
      // nothing further to call. The document appears as `pending` and the list
      // polls it from there.
      await queryClient.invalidateQueries({ queryKey: queryKeys.documents });
      await navigate({ to: "/docs" });
    },
  });

  const busy = phase === "creating" || phase === "uploading";

  return (
    <section>
      <h1>Upload a PDF</h1>
      <p>
        Up to {MAX_PAGES} pages. Ten questions are generated from the text; it takes
        30–90 seconds after the upload finishes.
      </p>

      <form
        onSubmit={(event) => {
          event.preventDefault();
          void form.handleSubmit();
        }}
      >
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
            <progress aria-label="Checking the page count" /> Checking the page
            count…
          </p>
        ) : null}
        {inspection?.ok ? (
          <p>
            {inspection.pageCount} page{inspection.pageCount === 1 ? "" : "s"} — good to
            go.
          </p>
        ) : null}
        {inspection && !inspection.ok ? <p role="alert">{inspection.reason}</p> : null}

        <form.Field
          name="title"
          validators={{
            onSubmit: ({ value }) =>
              value.trim().length === 0
                ? "Give it a title you'll recognise later."
                : undefined,
          }}
        >
          {(field) => (
            <p>
              <label htmlFor={field.name}>Title</label>
              <br />
              <input
                id={field.name}
                name={field.name}
                type="text"
                value={field.state.value}
                disabled={busy}
                aria-invalid={field.state.meta.errors.length > 0}
                onBlur={field.handleBlur}
                onChange={(event) => field.handleChange(event.target.value)}
              />
              {field.state.meta.errors.length > 0 ? (
                <>
                  <br />
                  <span role="alert">{field.state.meta.errors.join(" ")}</span>
                </>
              ) : null}
            </p>
          )}
        </form.Field>

        <form.Field
          name="topics"
          validators={{
            onSubmit: ({ value }) =>
              value.length === 0 ? "Pick at least one topic." : undefined,
          }}
        >
          {(field) => (
            // A real fieldset/legend around real checkboxes: the browser groups
            // them for keyboard and screen-reader navigation without any help
            // from us, which a row of styled toggle buttons would not.
            <fieldset>
              <legend>Topics</legend>
              <p>
                Closed vocabulary — these are the same tags the history breakdown
                segments on, so a free-text tag would be a hole in the analysis.
              </p>
              {TOPICS.map((topic) => (
                <div key={topic}>
                  <input
                    type="checkbox"
                    id={`topic-${topic}`}
                    name="topics"
                    value={topic}
                    checked={field.state.value.includes(topic)}
                    disabled={busy}
                    onChange={(event) =>
                      field.handleChange(
                        event.target.checked
                          ? [...field.state.value, topic]
                          : field.state.value.filter((t) => t !== topic),
                      )
                    }
                  />
                  <label htmlFor={`topic-${topic}`}>{TOPIC_LABELS[topic]}</label>
                </div>
              ))}
              {field.state.meta.errors.length > 0 ? (
                <p role="alert">{field.state.meta.errors.join(" ")}</p>
              ) : null}
            </fieldset>
          )}
        </form.Field>

        {submitError ? <ErrorNotice error={submitError} /> : null}

        <form.Subscribe selector={(state) => state.isSubmitting}>
          {(isSubmitting) => (
            <p>
              <button type="submit" disabled={isSubmitting || busy || !inspection?.ok}>
                {phase === "creating"
                  ? "Requesting upload…"
                  : phase === "uploading"
                    ? "Uploading…"
                    : "Upload"}
              </button>{" "}
              {busy ? <BusyMark label="Uploading" /> : null}
            </p>
          )}
        </form.Subscribe>
      </form>
    </section>
  );
}
