//! Media (M4.1): opaque blob upload/download over the server's media API.
//!
//! The homeserver is a dumb opaque-blob store: `POST /_pigeon/media/v1/upload`
//! takes raw bytes (the `Content-Type` header is preserved and echoed back on
//! download) and returns a `pigeon://{server}/{media_id}` content URI;
//! `GET /_pigeon/media/v1/download/{server}/{media_id}` returns the bytes
//! verbatim. Encrypted media (M4.2) reuses the exact same path — the client
//! encrypts before upload; the server never interprets the bytes (Gotcha #9).
//!
//! This module owns the `pigeon://` URI parsing and the size guard; the transfer
//! verbs live in [`crate::api`] and the FFI (`upload_media`/`download_media`/
//! `send_image`) in [`crate::rooms`].

use crate::CoreError;

/// The server's upload cap (50 MiB — `MAX_UPLOAD_BYTES` server-side). We reject
/// an oversize upload client-side with a clear typed error rather than hitting
/// the server's raw `413` (which carries no `P_*` error envelope).
pub(crate) const MAX_UPLOAD_BYTES: usize = 50 * 1024 * 1024;

/// The content-URI scheme. A media reference is `pigeon://{server}/{media_id}`.
const SCHEME: &str = "pigeon://";

/// Parse a `pigeon://{server}/{media_id}` content URI into `(server, media_id)`.
/// Mirrors the server's `ContentUri` parsing: both parts non-empty, and the
/// media id contains no `/`.
pub(crate) fn parse_content_uri(uri: &str) -> Result<(String, String), CoreError> {
    let rest = uri
        .strip_prefix(SCHEME)
        .ok_or_else(|| CoreError::Protocol {
            reason: format!("not a pigeon:// content URI: {uri}"),
        })?;
    let (server, media_id) = rest.split_once('/').ok_or_else(|| CoreError::Protocol {
        reason: format!("content URI missing media id: {uri}"),
    })?;
    if server.is_empty() || media_id.is_empty() || media_id.contains('/') {
        return Err(CoreError::Protocol {
            reason: format!("malformed content URI: {uri}"),
        });
    }
    Ok((server.to_owned(), media_id.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_valid_content_uri() {
        let (server, id) = parse_content_uri("pigeon://pigeon.example/AbC123").unwrap();
        assert_eq!(server, "pigeon.example");
        assert_eq!(id, "AbC123");
    }

    #[test]
    fn rejects_malformed_uris() {
        for bad in [
            "https://pigeon.example/x",    // wrong scheme
            "pigeon://pigeon.example",     // no media id
            "pigeon:///AbC123",            // empty server
            "pigeon://pigeon.example/",    // empty media id
            "pigeon://pigeon.example/a/b", // slash in media id
        ] {
            assert!(
                matches!(parse_content_uri(bad), Err(CoreError::Protocol { .. })),
                "expected {bad} to be rejected"
            );
        }
    }
}
