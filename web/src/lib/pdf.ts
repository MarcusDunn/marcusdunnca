/** Backend hard limit. Checked here so an over-long PDF costs zero API calls. */
export const MAX_PAGES = 100;

export type PdfInspection =
  | { ok: true; pageCount: number }
  | { ok: false; reason: string };

/**
 * Counts pages in the browser before we ask the API for anything.
 *
 * pdf-lib parses the whole document, which is slower than scraping the page tree
 * by hand, but it is the difference between "this is a real PDF with 42 pages"
 * and "this file starts with %PDF". Catching a corrupt file here is worth the
 * second it costs, because the alternative is a presigned upload, an S3 PUT, a
 * Bedrock invocation and a failed document to clean up.
 */
export async function inspectPdf(file: File): Promise<PdfInspection> {
  if (file.type && file.type !== "application/pdf") {
    return { ok: false, reason: "That doesn't look like a PDF." };
  }

  let pageCount: number;
  try {
    // pdf-lib is ~400 kB and only the upload screen ever needs it. Importing it
    // here keeps it out of the initial bundle: the login and document-list
    // screens on a phone shouldn't pay for a parser they never call. The check is
    // already async and fires on file selection, so the extra fetch is invisible.
    const { PDFDocument } = await import("pdf-lib");
    const bytes = await file.arrayBuffer();
    const pdf = await PDFDocument.load(bytes, {
      // Encrypted-but-openable PDFs are common enough (bank statements, gov
      // reports with permissions flags) that refusing them outright would be
      // annoying. The backend extracts text server-side and will fail loudly if
      // the encryption is real.
      ignoreEncryption: true,
      updateMetadata: false,
    });
    pageCount = pdf.getPageCount();
  } catch {
    return { ok: false, reason: "Couldn't read that PDF — it may be corrupt." };
  }

  if (pageCount === 0) return { ok: false, reason: "That PDF has no pages." };
  if (pageCount > MAX_PAGES) {
    return {
      ok: false,
      reason: `${pageCount} pages — the limit is ${MAX_PAGES}. Split it and upload the part you want to study.`,
    };
  }

  return { ok: true, pageCount };
}
