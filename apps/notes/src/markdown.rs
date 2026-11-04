//! Markdown → HTML for the preview, without anything that could run: raw
//! HTML is shown as text and script links are dropped.

use pulldown_cmark::{html, CowStr, Event, Options, Parser, Tag};

/// Renders a note as HTML that is safe to put into the page.
pub fn to_html(markdown: &str) -> String {
    let options = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let events = Parser::new_ext(markdown, options).map(|event| match event {
        // Written out as text, so `<script>` is shown, not run.
        Event::Html(raw) | Event::InlineHtml(raw) => Event::Text(raw),
        Event::Start(Tag::Link { link_type, dest_url, title, id }) => {
            Event::Start(Tag::Link { link_type, dest_url: safe_url(dest_url), title, id })
        }
        Event::Start(Tag::Image { link_type, dest_url, title, id }) => {
            Event::Start(Tag::Image { link_type, dest_url: safe_url(dest_url), title, id })
        }
        other => other,
    });
    let mut out = String::new();
    html::push_html(&mut out, events);
    out
}

/// The URL, or `#` for schemes that run code (`javascript:` and the like).
fn safe_url(url: CowStr<'_>) -> CowStr<'_> {
    let scheme = url.trim_start().split(':').next().unwrap_or_default().to_ascii_lowercase();
    let has_scheme = url.contains(':') && !scheme.contains('/');
    if has_scheme && !matches!(scheme.as_str(), "http" | "https" | "mailto") {
        CowStr::Borrowed("#")
    } else {
        url
    }
}
