//! Markdown → HTML for the preview, without anything that could run: raw
//! HTML is shown as text and script links are dropped.

use pulldown_cmark::{html, CowStr, Event, Options, Parser, Tag, TagEnd};

/// A rendered note.
pub struct Preview {
    pub html: String,
    /// Images from other sites shown as links instead.
    pub blocked_images: usize,
}

/// Renders a note as HTML that is safe to put into the page. Images from
/// other sites become links unless `external_images`: loading one tells that
/// site the reader's address and when they opened the note.
pub fn to_html(markdown: &str, external_images: bool) -> Preview {
    let options = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    // For each open image: whether it became a link.
    let mut images = Vec::new();
    let mut blocked_images = 0;
    let events = Parser::new_ext(markdown, options).flat_map(|event| match event {
        // Written out as text, so `<script>` is shown, not run.
        Event::Html(raw) | Event::InlineHtml(raw) => vec![Event::Text(raw)],
        Event::Start(Tag::Link { link_type, dest_url, title, id }) => {
            vec![Event::Start(Tag::Link { link_type, dest_url: safe_url(dest_url), title, id })]
        }
        Event::Start(Tag::Image { link_type, dest_url, title, id }) => {
            let dest_url = safe_url(dest_url);
            let blocked = !external_images && is_external(&dest_url);
            images.push(blocked);
            if blocked {
                blocked_images += 1;
                vec![Event::Start(Tag::Link { link_type, dest_url, title, id }), Event::Text("🖼 ".into())]
            } else {
                vec![Event::Start(Tag::Image { link_type, dest_url, title, id })]
            }
        }
        Event::End(TagEnd::Image) if images.pop() == Some(true) => vec![Event::End(TagEnd::Link)],
        other => vec![other],
    });
    let mut out = String::new();
    html::push_html(&mut out, events);
    Preview { html: out, blocked_images }
}

/// Whether a URL points to another site (relative ones are the app's own).
fn is_external(url: &str) -> bool {
    let url = url.trim_start().to_ascii_lowercase();
    url.starts_with("http:") || url.starts_with("https:") || url.starts_with("//")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn images_from_other_sites_become_links_unless_allowed() {
        let note = "![cat](https://tracker.example/cat.png) ![own](picture.png)";
        let preview = to_html(note, false);
        assert_eq!(preview.blocked_images, 1);
        assert!(!preview.html.contains("<img src=\"https://"), "{}", preview.html);
        assert!(preview.html.contains("<a href=\"https://tracker.example/cat.png\">🖼 cat</a>"), "{}", preview.html);
        assert!(preview.html.contains("<img src=\"picture.png\""), "{}", preview.html);

        let allowed = to_html(note, true);
        assert_eq!(allowed.blocked_images, 0);
        assert!(allowed.html.contains("<img src=\"https://tracker.example/cat.png\""), "{}", allowed.html);
    }

    #[test]
    fn raw_html_and_script_links_do_not_run() {
        let html = to_html("<script>x()</script>\n\n[a](javascript:alert(1))", false).html;
        assert!(!html.contains("<script>") && !html.contains("href=\"javascript:"), "{html}");
    }
}
