import { ApiError, SchemaError } from "../lib/api";

/**
 * An indeterminate `<progress>` is the browser's own busy indicator: it spins (or
 * bars) natively, it's announced as a progress bar by screen readers, and it
 * costs no CSS. A CSS-animated div would be a worse version of it.
 */
export function Busy({ label }: { label: string }) {
  return (
    <p>
      <progress aria-label={label} /> {label}
    </p>
  );
}

/** Inline variant for use next to a control, without the surrounding paragraph. */
export function BusyMark({ label }: { label: string }) {
  return <progress aria-label={label} />;
}

/**
 * The one sentence to show for a thrown value.
 *
 * Exported because errors are reported in two shapes: `ErrorNotice` below, and
 * a bare line of text where a whole section would be too much — one row of a
 * multi-file upload, say. Both must say the same thing about the same error, so
 * neither reaches for `.message` itself.
 */
export function describeError(error: unknown): string {
  return error instanceof ApiError || error instanceof Error
    ? error.message
    : "Something went wrong.";
}

/**
 * A schema mismatch is not transient, so it says so plainly instead of offering a
 * retry that will fail identically. Everything else gets a retry button.
 */
export function ErrorNotice({
  error,
  onRetry,
}: {
  error: unknown;
  onRetry?: (() => void) | undefined;
}) {
  const isSchema = error instanceof SchemaError;
  const message = describeError(error);

  return (
    <section role="alert">
      <h3>{isSchema ? "Unexpected response from the server" : "Error"}</h3>
      <p>{message}</p>
      {isSchema ? (
        <p>
          The API sent a shape this build doesn&apos;t understand. Retrying
          won&apos;t help — the client and the API are out of step.
        </p>
      ) : null}
      {onRetry && !isSchema ? (
        <p>
          <button type="button" onClick={onRetry}>
            Try again
          </button>
        </p>
      ) : null}
    </section>
  );
}
