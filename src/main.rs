use std::cell::{Cell, RefCell};
use std::env;
use std::io::Write;
use std::process::{Command, Stdio};
use std::rc::Rc;

use html5ever::tendril::TendrilSink;
use html5ever::{namespace_url, ns, parse_document, Attribute, LocalName, ParseOpts, QualName};
use markup5ever_rcdom::{Handle, Node, NodeData, RcDom, SerializableHandle};

/// Elements removed wholesale (tag plus all contents) before Markdown
/// conversion. These routinely leak CSS/JS/navigation noise into the output
/// because html2md has no option to skip them.
const REMOVE_TAGS: &[&str] = &[
    "style", "script", "title", "noscript", "meta", "link", "head", "nav", "button", "form",
    "iframe", "svg", "footer",
];

/// Pulls the HTML flavor of the current clipboard contents. arboard reads
/// NSPasteboardTypeHTML natively on macOS.
fn get_clipboard_html() -> Result<String, String> {
    let mut cb =
        arboard::Clipboard::new().map_err(|e| format!("failed to access clipboard: {e}"))?;
    cb.get()
        .html()
        .map_err(|e| format!("clipboard doesn't contain HTML (copy something rich-text first): {e}"))
}

/// Writes text back to the system clipboard via pbcopy.
fn set_clipboard_text(text: &str) -> Result<(), String> {
    let mut child = Command::new("pbcopy")
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to spawn pbcopy: {e}"))?;
    child
        .stdin
        .as_mut()
        .ok_or("failed to open pbcopy stdin")?
        .write_all(text.as_bytes())
        .map_err(|e| format!("failed to write to pbcopy: {e}"))?;
    let status = child
        .wait()
        .map_err(|e| format!("failed to wait for pbcopy: {e}"))?;
    if !status.success() {
        return Err(format!("pbcopy failed with exit status: {status}"));
    }
    Ok(())
}

/// Simulates Cmd+V so the converted markdown lands wherever the cursor is.
/// A short delay up front keeps the keystroke from merging with physical
/// modifiers the user may still be holding.
fn simulate_paste() -> Result<(), String> {
    let output = Command::new("osascript")
        .args([
            "-e",
            "delay 0.1",
            "-e",
            "tell application \"System Events\" to keystroke \"v\" using command down",
        ])
        .output()
        .map_err(|e| format!("failed to simulate paste: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "paste failed — grant Hammerspoon Accessibility (System Settings > Privacy & \
             Security > Accessibility) and Automation consent for System Events: {}",
            stderr.trim()
        ));
    }
    Ok(())
}

/// Percent-decodes a URL component. Returns None on malformed input so the
/// caller can conservatively keep the original value.
fn percent_decode(s: &str) -> Option<String> {
    fn hex_val(b: u8) -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        }
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return None;
            }
            out.push(hex_val(bytes[i + 1])? << 4 | hex_val(bytes[i + 2])?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn make_attr(name: &str, value: &str) -> Attribute {
    Attribute {
        name: QualName::new(None, ns!(), local_name_from(name)),
        value: value.into(),
    }
}

fn local_name_from(name: &str) -> LocalName {
    LocalName::from(name.to_string())
}

/// Rebuilds an element under a new tag name, keeping attributes and children.
fn rename_element(el: &Handle, new_name: &str) -> Handle {
    if let NodeData::Element {
        name,
        attrs,
        template_contents,
        mathml_annotation_xml_integration_point,
    } = &el.data
    {
        Rc::new(Node {
            parent: Cell::new(None),
            children: RefCell::new(el.children.borrow().clone()),
            data: NodeData::Element {
                name: QualName {
                    prefix: name.prefix.clone(),
                    ns: name.ns.clone(),
                    local: local_name_from(new_name),
                },
                attrs: RefCell::new(attrs.borrow().clone()),
                template_contents: RefCell::new(template_contents.borrow().clone()),
                mathml_annotation_xml_integration_point: *mathml_annotation_xml_integration_point,
            },
        })
    } else {
        el.clone()
    }
}

/// Keeps only src and alt on <img> (alt only if non-empty) so html2md emits
/// ![alt](src) instead of a raw HTML tag. Falls back to data-src when src is
/// missing/empty.
fn normalize_img(attrs: &RefCell<Vec<Attribute>>) {
    let get = |name: &str| -> Option<String> {
        attrs
            .borrow()
            .iter()
            .find(|a| a.name.local.as_ref() == name)
            .map(|a| a.value.trim().to_string())
    };
    let mut src = get("src").unwrap_or_default();
    if src.is_empty() {
        src = get("data-src").unwrap_or_default();
    }
    let alt = get("alt").unwrap_or_default();
    let mut new_attrs = Vec::new();
    if !src.is_empty() {
        new_attrs.push(make_attr("src", &src));
    }
    if !alt.is_empty() {
        new_attrs.push(make_attr("alt", &alt));
    }
    *attrs.borrow_mut() = new_attrs;
}

/// Google Docs wraps outbound links in www.google.com/url?q=<dest>. Rewrites
/// such hrefs to the decoded destination; leaves the href untouched on any
/// parsing/decoding failure.
fn unwrap_google_redirect(attrs: &RefCell<Vec<Attribute>>) {
    let href = {
        let borrowed = attrs.borrow();
        match borrowed.iter().find(|a| a.name.local.as_ref() == "href") {
            Some(a) => a.value.to_string(),
            None => return,
        }
    };
    if !href.to_ascii_lowercase().contains("google.com/url?") {
        return;
    }
    let Some(query) = href.splitn(2, '?').nth(1) else {
        return;
    };
    let Some(encoded) = query.split('&').find_map(|p| p.strip_prefix("q=")) else {
        return;
    };
    if let Some(decoded) = percent_decode(encoded) {
        if !decoded.is_empty() {
            let mut borrowed = attrs.borrow_mut();
            if let Some(attr) = borrowed.iter_mut().find(|a| a.name.local.as_ref() == "href") {
                attr.value = decoded.into();
            }
        }
    }
}

/// Recursively sanitizes the children of `parent` in place.
fn sanitize_children(parent: &Handle, in_blockquote: bool) {
    let mut kept: Vec<Handle> = Vec::new();
    for child in parent.children.borrow().iter() {
        let mut child = child.clone();
        if let NodeData::Element { name, attrs, .. } = &child.data {
            let tag = name.local.to_string();
            let child_in_blockquote = in_blockquote || tag == "blockquote";
            if REMOVE_TAGS.contains(&tag.as_str()) {
                // Inside a blockquote, <footer> carries attribution — demote it
                // to a paragraph instead of dropping the text.
                if in_blockquote && tag == "footer" {
                    child = rename_element(&child, "p");
                } else {
                    continue;
                }
            } else if tag == "u" {
                // Google Docs underlines via <u>; html2md drops it. Bold is the
                // closest Markdown-native emphasis.
                child = rename_element(&child, "b");
            } else if in_blockquote && tag == "cite" {
                child = rename_element(&child, "p");
            } else if tag == "img" {
                normalize_img(attrs);
            } else if tag == "a" {
                unwrap_google_redirect(attrs);
            }
            sanitize_children(&child, child_in_blockquote);
        }
        kept.push(child);
    }
    *parent.children.borrow_mut() = kept;
}

/// Parses HTML into a real DOM, removes/renames/normalizes noisy elements,
/// and serializes back to HTML for html2md.
fn sanitize_html(html: &str) -> String {
    let dom = parse_document(RcDom::default(), ParseOpts::default())
        .from_utf8()
        .read_from(&mut html.as_bytes())
        .expect("reading from an in-memory byte slice cannot fail");
    sanitize_children(&dom.document, false);
    let mut out = Vec::new();
    html5ever::serialize::serialize(
        &mut out,
        &SerializableHandle::from(dom.document),
        html5ever::serialize::SerializeOpts::default(),
    )
    .expect("serializing into an in-memory buffer cannot fail");
    String::from_utf8_lossy(&out).to_string()
}

/// Full conversion pipeline: sanitize the clipboard HTML, then convert to
/// Markdown.
fn convert(html: &str) -> String {
    html2md::parse_html(&sanitize_html(html))
}

fn print_usage() {
    eprintln!(
        "mdpaste\n\n\
         Usage:\n  \
         mdpaste            Convert clipboard HTML -> Markdown, copy it back, and paste.\n  \
         mdpaste --dry-run  Same conversion, but only copies to clipboard (no paste).\n  \
         mdpaste --test FILE.html   Convert an HTML file and print Markdown to stdout.\n\
         \n(--test works on any OS; the other modes need macOS.)"
    );
}

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() == 3 && args[1] == "--test" {
        let html = match std::fs::read_to_string(&args[2]) {
            Ok(h) => h,
            Err(e) => {
                eprintln!("failed to read {}: {e}", args[2]);
                std::process::exit(1);
            }
        };
        println!("{}", convert(&html));
        return;
    }

    if args.len() > 2 || (args.len() == 2 && args[1] != "--dry-run") {
        print_usage();
        std::process::exit(1);
    }

    let dry_run = args.len() == 2;

    let html = match get_clipboard_html() {
        Ok(h) => h,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };

    let markdown = convert(&html);

    if let Err(e) = set_clipboard_text(&markdown) {
        eprintln!("{e}");
        std::process::exit(1);
    }

    if !dry_run {
        if let Err(e) = simulate_paste() {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const APPLE_NOTES: &str = include_str!("../tests/fixtures/apple-notes.html");
    const GOOGLE_DOCS: &str = include_str!("../tests/fixtures/google-docs.html");
    const TABLE_AND_CODE: &str = include_str!("../tests/fixtures/table-and-code.html");
    const WEBPAGE: &str = include_str!("../tests/fixtures/webpage-chrome.html");
    const NOTION: &str = include_str!("../tests/fixtures/notion.html");
    const SLACK: &str = include_str!("../tests/fixtures/slack.html");

    #[test]
    fn strips_style_and_css_text() {
        for html in [APPLE_NOTES, GOOGLE_DOCS, NOTION, SLACK, WEBPAGE] {
            let md = convert(html);
            assert!(!md.contains('{'), "CSS rule leaked: {md}");
            assert!(!md.contains("font-family"), "style text leaked: {md}");
        }
        let md = convert(WEBPAGE);
        assert!(!md.contains("window.analytics"), "JS leaked: {md}");
        assert!(!md.contains("- Example Blog"), "<title> text leaked: {md}");
    }

    #[test]
    fn strips_nav_and_button() {
        let md = convert(WEBPAGE);
        assert!(!md.contains("☰ Menu"), "nav button leaked: {md}");
        assert!(!md.contains("[Home](/)"), "nav links leaked: {md}");
    }

    #[test]
    fn underline_becomes_bold() {
        let md = convert(GOOGLE_DOCS);
        assert!(md.contains("**paid acquisition**"), "got: {md}");
    }

    #[test]
    fn google_redirect_is_unwrapped() {
        let md = convert(GOOGLE_DOCS);
        assert!(md.contains("https://example.com/brief"), "got: {md}");
        assert!(!md.contains("google.com/url"), "redirect href leaked: {md}");
    }

    #[test]
    fn img_becomes_markdown_image() {
        let md = convert(WEBPAGE);
        assert!(
            md.contains(
                "![Diagram showing Rust ownership rules](https://blog.example.com/images/ownership-diagram.png)"
            ),
            "got: {md}"
        );
    }

    #[test]
    fn blockquote_footer_and_cite_are_kept_as_paragraphs() {
        let md = convert(TABLE_AND_CODE);
        assert!(md.contains("Anonymous"), "footer text lost: {md}");
        assert!(md.contains("Converter Authors"), "cite text lost: {md}");
        assert!(md.contains('>'), "expected blockquote output: {md}");
    }

    #[test]
    fn table_code_and_nested_list_survive() {
        let md = convert(TABLE_AND_CODE);
        assert!(md.contains("| Tag |"), "table broken: {md}");
        assert!(md.contains("```"), "code fence broken: {md}");
        assert!(md.contains("cargo build --release"), "code body broken: {md}");
        assert!(md.contains("* Unit tests pass"), "nested list broken: {md}");
    }

    #[test]
    fn unicode_survives() {
        let md = convert(APPLE_NOTES);
        assert!(md.contains("☕"), "emoji lost: {md}");
        assert!(md.contains("—"), "em dash lost: {md}");
    }

    #[test]
    fn percent_decode_works() {
        assert_eq!(
            percent_decode("https%3A%2F%2Fexample.com%2Fbrief"),
            Some("https://example.com/brief".to_string())
        );
        assert_eq!(percent_decode("plain"), Some("plain".to_string()));
        assert_eq!(percent_decode("bad%zz"), None);
        assert_eq!(percent_decode("truncated%2"), None);
    }

    #[test]
    fn sanitizer_is_idempotent_enough_on_plain_html() {
        let md = convert("<p>Hello <b>world</b></p>");
        assert!(md.contains("Hello"), "got: {md}");
        assert!(md.contains("**world**"), "got: {md}");
    }
}
