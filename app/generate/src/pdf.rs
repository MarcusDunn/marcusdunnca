//! PDF inspection, used only for the page guard.
//!
//! This does not extract text — Bedrock does that, and does it far better than
//! anything that would fit here. All this module answers is "how many pages",
//! because that is the only cheap local proxy for how much a generation will
//! cost.

use trainer_core::error::Error;

/// Count pages, or explain why we will not process this file.
///
/// Every failure is an `Error::Invalid`, never an `Error::Aws`: a PDF that
/// cannot be parsed is the document's problem, and the right outcome is
/// `status: failed` with a message the reader can act on, not a retried Lambda
/// invocation that will fail identically three times.
///
/// The parse happens *before* the Bedrock call and is the reason the guard is
/// worth having at all. Sending a 400-page document and discovering it was too
/// big from the bill is the failure mode being avoided.
pub fn page_count(bytes: &[u8]) -> Result<usize, Error> {
    // Cheap structural check first. lopdf's error for a non-PDF is accurate but
    // unhelpfully phrased, and this is the likeliest real-world case: the
    // upload's Content-Type was pinned to application/pdf by the presigned URL,
    // but nothing verified the *bytes* matched.
    if !bytes.starts_with(b"%PDF-") {
        return Err(Error::Invalid(
            "this file is not a PDF (missing %PDF header)".into(),
        ));
    }

    let doc = lopdf::Document::load_mem(bytes)
        .map_err(|e| Error::Invalid(format!("could not read this PDF: {e}")))?;

    // An encrypted PDF may parse structurally and then yield nothing useful to
    // the model, producing ten questions about a blank document. Refusing here
    // turns a confusing quiz into a clear message.
    if doc.is_encrypted() {
        return Err(Error::Invalid(
            "this PDF is password-protected; remove the protection and re-upload".into(),
        ));
    }

    let pages = doc.get_pages().len();
    if pages == 0 {
        return Err(Error::Invalid("this PDF has no pages".into()));
    }

    Ok(pages)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_pdf_bytes_are_rejected_as_a_document_fault() {
        // Notably an `Invalid`, not an `Aws` — the distinction decides whether
        // the invocation is retried or the document is marked failed.
        assert!(matches!(
            page_count(b"not a pdf at all"),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(page_count(&[]), Err(Error::Invalid(_))));
    }

    #[test]
    fn truncated_pdf_is_rejected_rather_than_panicking() {
        // A PDF header with nothing behind it is what a cancelled upload looks
        // like. lopdf must return an error here, not panic — a panic in a
        // Lambda handler is an invocation error with no `status: failed` row,
        // which strands the document.
        assert!(page_count(b"%PDF-1.7\n").is_err());
    }
}
