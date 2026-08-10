//! Line ending handling shared by the tools that write files.
//!
//! The `read` tool hands the model text through `str::lines`, which drops
//! the carriage return of every CRLF line. So the model only ever sees LF
//! text, and every string it sends back carries LF. On a CRLF file that
//! made `edit` fail: `old_string` was compared byte for byte against a
//! source whose newlines were `\r\n`, so any needle spanning more than one
//! line matched nothing, and the tool reported the text as missing while
//! it sat in the file in plain view. A real session lost six edits to
//! this, all on the one CRLF file it touched.
//!
//! The rule these helpers implement: match on LF, write back whatever the
//! file already used. A file with mixed endings counts as CRLF and comes
//! out wholly CRLF, since there is no earlier state worth preserving once
//! the two are already mixed.

/// Whether this text uses Windows line endings anywhere.
pub fn has_crlf(source: &str) -> bool {
    source.contains("\r\n")
}

/// The same text with every CRLF reduced to a bare LF.
pub fn to_lf(source: &str) -> String {
    source.replace("\r\n", "\n")
}

/// The same text with every line ending written as CRLF. Normalizes first,
/// so text that is already CRLF passes through unchanged rather than
/// gaining a second carriage return per line.
pub fn to_crlf(source: &str) -> String {
    to_lf(source).replace('\n', "\r\n")
}
