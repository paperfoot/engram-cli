//! PDF extraction.
//!
//! Uses `pdf-extract` to get raw text from a scientific paper and returns it
//! to the ingest pipeline. Scientific papers have a predictable shape
//! (abstract, intro, methods, results, discussion, references) so the
//! section-aware chunker downstream produces meaningful breadcrumbs.

use crate::error::IngestError;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;

/// Extract text from a PDF file. Returns plain UTF-8 on success.
///
/// `pdf-extract` can panic (e.g. `unwrap()` on malformed font tables in some
/// scientific PDFs). We isolate it with `catch_unwind` so one bad file becomes
/// a recoverable error instead of crashing the entire ingest batch.
pub fn extract_text(path: &Path) -> Result<String, IngestError> {
    catch_unwind(AssertUnwindSafe(|| pdf_extract::extract_text(path)))
        .map_err(|_| IngestError::Invalid("pdf extract: parser panicked".to_string()))?
        .map_err(|e| IngestError::Invalid(format!("pdf extract: {e}")))
}

/// Best-effort extraction for a PDF byte slice.
pub fn extract_text_from_bytes(bytes: &[u8]) -> Result<String, IngestError> {
    catch_unwind(AssertUnwindSafe(|| pdf_extract::extract_text_from_mem(bytes)))
        .map_err(|_| IngestError::Invalid("pdf extract: parser panicked".to_string()))?
        .map_err(|e| IngestError::Invalid(format!("pdf extract: {e}")))
}

/// Is this path a PDF?
pub fn is_pdf(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("pdf"))
        .unwrap_or(false)
}
