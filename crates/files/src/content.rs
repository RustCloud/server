//! Sending file content over HTTP: `Range` requests, ETags and safe headers.

use std::future::Future;

use axum::body::Body;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use rustcloud_core::{ByteRange, Result};

use crate::service::{Download, Node};

/// What a `Range` header asks for, checked against the file size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeRequest {
    /// No (usable) range: send the whole file.
    Full,
    /// Send only these bytes (`206 Partial Content`).
    Partial(ByteRange),
    /// The range starts beyond the end of the file (`416`).
    Unsatisfiable,
}

/// Interprets a `Range` header for a file of `size` bytes.
///
/// Supports a single range in the forms `bytes=a-b`, `bytes=a-` and
/// `bytes=-n` (last n bytes). Anything else — several ranges, other units,
/// malformed values — is ignored and the whole file is sent, as RFC 9110 allows.
pub fn parse_range(header: Option<&str>, size: u64) -> RangeRequest {
    let Some(spec) = header.and_then(|h| h.trim().strip_prefix("bytes=")) else {
        return RangeRequest::Full;
    };
    if size == 0 || spec.contains(',') {
        return RangeRequest::Full;
    }
    let Some((start, end)) = spec.split_once('-') else {
        return RangeRequest::Full;
    };
    let (start, end) = (start.trim(), end.trim());

    let range = if start.is_empty() {
        // Suffix range: the last `end` bytes.
        match end.parse::<u64>() {
            Ok(0) => return RangeRequest::Unsatisfiable,
            Ok(n) => ByteRange { start: size.saturating_sub(n), end: size - 1 },
            Err(_) => return RangeRequest::Full,
        }
    } else {
        let Ok(start) = start.parse::<u64>() else {
            return RangeRequest::Full;
        };
        let end = match end {
            "" => size - 1,
            end => match end.parse::<u64>() {
                Ok(end) if end >= start => end.min(size - 1),
                _ => return RangeRequest::Full,
            },
        };
        if start >= size {
            return RangeRequest::Unsatisfiable;
        }
        ByteRange { start, end }
    };
    RangeRequest::Partial(range)
}

/// Answers a content request for `node`: handles `If-None-Match` and `Range`,
/// then calls `open` to get the bytes.
///
/// `attachment` forces a download instead of showing the file in the browser.
pub async fn serve<F, Fut>(node: Node, request: &HeaderMap, attachment: bool, open: F) -> Result<Response>
where
    F: FnOnce(Option<ByteRange>) -> Fut,
    Fut: Future<Output = Result<Download>>,
{
    let etag = node.etag.as_deref().map(|e| format!("\"{e}\""));
    if let (Some(etag), Some(if_none_match)) = (&etag, request.get(header::IF_NONE_MATCH)) {
        if etag_matches(if_none_match, etag) {
            return Ok((StatusCode::NOT_MODIFIED, [(header::ETAG, etag.clone())]).into_response());
        }
    }

    let size = u64::try_from(node.size).unwrap_or(0);
    let range = match parse_range(request.get(header::RANGE).and_then(|v| v.to_str().ok()), size) {
        RangeRequest::Full => None,
        RangeRequest::Partial(range) => Some(range),
        RangeRequest::Unsatisfiable => {
            let content_range = format!("bytes */{size}");
            return Ok((StatusCode::RANGE_NOT_SATISFIABLE, [(header::CONTENT_RANGE, content_range)]).into_response());
        }
    };

    let download = open(range).await?;
    let mime = download.node.mime_type.clone().unwrap_or_else(|| "application/octet-stream".into());
    let mut headers = HeaderMap::new();
    let status = if range.is_some() { StatusCode::PARTIAL_CONTENT } else { StatusCode::OK };

    match range {
        Some(range) => {
            set(&mut headers, header::CONTENT_LENGTH, range.byte_count().to_string());
            set(&mut headers, header::CONTENT_RANGE, format!("bytes {}-{}/{size}", range.start, range.end));
        }
        None => set(&mut headers, header::CONTENT_LENGTH, size.to_string()),
    }
    let disposition = if attachment || !is_safe_inline(&mime) { "attachment" } else { "inline" };
    set(&mut headers, header::CONTENT_DISPOSITION, content_disposition(disposition, &download.node.name));
    if mime != "application/pdf" {
        // Even if a browser renders the file, it gets no scripts and no access to
        // our origin. (Browsers refuse to show PDFs inside a sandbox, so PDFs —
        // which run in the browser's own isolated viewer — are exempt.)
        set(
            &mut headers,
            header::CONTENT_SECURITY_POLICY,
            "sandbox; default-src 'none'; img-src 'self'; media-src 'self'".into(),
        );
    }
    set(&mut headers, header::CONTENT_TYPE, mime);
    set(&mut headers, header::ACCEPT_RANGES, "bytes".into());
    set(&mut headers, header::CACHE_CONTROL, "private, no-cache".into());
    if let Some(etag) = etag {
        set(&mut headers, header::ETAG, etag);
    }
    Ok((status, headers, Body::from_stream(download.body)).into_response())
}

fn set(headers: &mut HeaderMap, name: header::HeaderName, value: String) {
    if let Ok(value) = HeaderValue::try_from(value) {
        headers.insert(name, value);
    }
}

fn etag_matches(if_none_match: &HeaderValue, etag: &str) -> bool {
    let Ok(value) = if_none_match.to_str() else {
        return false;
    };
    value.split(',').map(str::trim).any(|candidate| candidate == "*" || candidate.trim_start_matches("W/") == etag)
}

/// Types a browser may display directly. Everything else (HTML, SVG,
/// JavaScript…) could run scripts and is always downloaded instead.
fn is_safe_inline(mime: &str) -> bool {
    matches!(
        mime,
        "image/png"
            | "image/jpeg"
            | "image/gif"
            | "image/webp"
            | "image/avif"
            | "image/bmp"
            | "text/plain"
            | "application/pdf"
    ) || mime.starts_with("video/")
        || mime.starts_with("audio/")
}

/// `Content-Disposition` with an ASCII fallback name and the exact UTF-8 name (RFC 6266).
fn content_disposition(kind: &str, name: &str) -> String {
    let fallback: String = name
        .chars()
        .map(|c| if c == ' ' || (c.is_ascii_graphic() && c != '"' && c != '\\') { c } else { '_' })
        .collect();
    let mut encoded = String::new();
    for byte in name.bytes() {
        let plain = byte.is_ascii_alphanumeric() || b"!#$&+-.^_`|~".contains(&byte);
        if plain {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    format!("{kind}; filename=\"{fallback}\"; filename*=UTF-8''{encoded}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn partial(start: u64, end: u64) -> RangeRequest {
        RangeRequest::Partial(ByteRange { start, end })
    }

    #[test]
    fn parses_single_ranges() {
        assert_eq!(parse_range(Some("bytes=0-99"), 1000), partial(0, 99));
        assert_eq!(parse_range(Some("bytes=500-"), 1000), partial(500, 999));
        assert_eq!(parse_range(Some("bytes=-100"), 1000), partial(900, 999));
        assert_eq!(parse_range(Some("bytes=900-5000"), 1000), partial(900, 999), "end is clamped");
        assert_eq!(parse_range(Some("bytes=-5000"), 1000), partial(0, 999), "suffix longer than the file");
    }

    #[test]
    fn ranges_outside_the_file_are_unsatisfiable() {
        assert_eq!(parse_range(Some("bytes=1000-"), 1000), RangeRequest::Unsatisfiable);
        assert_eq!(parse_range(Some("bytes=-0"), 1000), RangeRequest::Unsatisfiable);
    }

    #[test]
    fn unusable_ranges_mean_the_whole_file() {
        for header in
            [None, Some("items=0-1"), Some("bytes=0-1,5-6"), Some("bytes=5-1"), Some("bytes=x-y"), Some("bytes=")]
        {
            assert_eq!(parse_range(header, 1000), RangeRequest::Full, "{header:?}");
        }
        assert_eq!(parse_range(Some("bytes=0-10"), 0), RangeRequest::Full, "empty file");
    }

    #[test]
    fn disposition_keeps_unicode_names() {
        assert_eq!(
            content_disposition("attachment", "отчёт 1.pdf"),
            "attachment; filename=\"_____ 1.pdf\"; filename*=UTF-8''%D0%BE%D1%82%D1%87%D1%91%D1%82%201.pdf"
        );
        assert_eq!(
            content_disposition("inline", "a\"b.txt"),
            "inline; filename=\"a_b.txt\"; filename*=UTF-8''a%22b.txt"
        );
    }

    #[test]
    fn scripts_are_never_shown_inline() {
        assert!(is_safe_inline("image/png"));
        assert!(is_safe_inline("video/mp4"));
        assert!(!is_safe_inline("text/html"));
        assert!(!is_safe_inline("image/svg+xml"));
        assert!(!is_safe_inline("application/javascript"));
    }

    #[test]
    fn etag_comparison_accepts_lists_and_weak_tags() {
        let etag = "\"abc\"";
        assert!(etag_matches(&HeaderValue::from_static("\"abc\""), etag));
        assert!(etag_matches(&HeaderValue::from_static("\"x\", W/\"abc\""), etag));
        assert!(etag_matches(&HeaderValue::from_static("*"), etag));
        assert!(!etag_matches(&HeaderValue::from_static("\"abd\""), etag));
    }
}
