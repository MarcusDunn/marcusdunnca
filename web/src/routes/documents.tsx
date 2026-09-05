import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { Busy, BusyMark, ErrorNotice } from "../components/ui";
import { api } from "../lib/api";
import { documentsQuery, queryKeys } from "../lib/queries";
import { isUnsettled, topicLabel, type DocumentSummary } from "../lib/schemas";

const STATUS_TEXT: Record<DocumentSummary["status"], string> = {
  pending: "Queued",
  processing: "Writing questions",
  ready: "Ready",
  failed: "Failed",
};

export function DocumentsScreen() {
  const queryClient = useQueryClient();
  const documents = useQuery(documentsQuery());

  const retry = useMutation({
    // Retry re-runs generation against the PDF already in S3. It does not create a
    // second document, which is the entire point: without this the only recovery
    // is uploading the same file again and the failed row stays forever.
    mutationFn: (documentId: string) => api.createDocument({ retryOf: documentId }),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: queryKeys.documents }),
  });

  if (documents.isPending) return <Busy label="Loading documents" />;
  if (documents.isError) {
    return <ErrorNotice error={documents.error} onRetry={() => void documents.refetch()} />;
  }

  const rows = documents.data.toSorted(
    (a, b) => Date.parse(b.createdAt) - Date.parse(a.createdAt),
  );
  const polling = rows.some((doc) => isUnsettled(doc.status));

  return (
    <section>
      <h1>Documents</h1>
      <p>
        <Link to="/upload">Upload PDFs</Link>
      </p>

      {polling ? (
        <p aria-live="polite">
          <progress aria-label="Generating questions" /> Generating questions —
          usually 30–90 seconds. This list refreshes itself; you can leave the page.
        </p>
      ) : null}

      {retry.isError ? <ErrorNotice error={retry.error} /> : null}

      {rows.length === 0 ? (
        <p>Nothing here yet. Upload a PDF or two to get started.</p>
      ) : (
        <ul>
          {rows.map((doc) => (
            <li key={doc.id}>
              <DocumentRow
                doc={doc}
                onRetry={() => retry.mutate(doc.id)}
                retrying={retry.isPending && retry.variables === doc.id}
              />
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

function DocumentRow({
  doc,
  onRetry,
  retrying,
}: {
  doc: DocumentSummary;
  onRetry: () => void;
  retrying: boolean;
}) {
  const unsettled = isUnsettled(doc.status);

  return (
    <>
      <h2>{doc.title || "Untitled"}</h2>

      {/* aria-live only while something is actually moving; a settled row
          shouldn't keep talking to a screen reader on every poll. */}
      <p aria-live={unsettled ? "polite" : "off"}>
        Status: {STATUS_TEXT[doc.status]}{" "}
        {unsettled ? <BusyMark label={STATUS_TEXT[doc.status]} /> : null}
      </p>

      <p>
        {doc.pageCount} page{doc.pageCount === 1 ? "" : "s"} ·{" "}
        {new Date(doc.createdAt).toLocaleDateString()} · {doc.attemptCount}{" "}
        attempt{doc.attemptCount === 1 ? "" : "s"}
        {doc.topics.length > 0
          ? ` · ${doc.topics.map(topicLabel).join(", ")}`
          : ""}
      </p>

      {doc.status === "failed" ? (
        <>
          <p>{doc.error ?? "Question generation failed for an unrecorded reason."}</p>
          <p>
            <button type="button" onClick={onRetry} disabled={retrying}>
              {retrying ? "Retrying…" : "Retry generation"}
            </button>{" "}
            {retrying ? <BusyMark label="Retrying" /> : null}
          </p>
        </>
      ) : null}

      {doc.status === "ready" ? (
        <p>
          <Link to="/docs/$documentId" params={{ documentId: doc.id }}>
            Read &amp; quiz
          </Link>
        </p>
      ) : null}
    </>
  );
}
