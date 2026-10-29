//! Export an entry as a Letter-size PDF with 1-inch margins, like the macOS app.

use std::path::Path;

use genpdf::elements::{Break, Paragraph};
use genpdf::fonts::{FontData, FontFamily};
use genpdf::style::{Color, Style};
use genpdf::{Document, Element as _, Margins, SimplePageDecorator, Size};

/// `font` is the editor's font file; a single face is used for every style.
pub fn export(text: &str, font: Vec<u8>, font_size: f32, path: &Path) -> Result<(), String> {
    let face = FontData::new(font, None).map_err(|e| e.to_string())?;
    let family = FontFamily {
        regular: face.clone(),
        bold: face.clone(),
        italic: face.clone(),
        bold_italic: face,
    };

    let mut doc = Document::new(family);
    doc.set_title(title_for(text));
    doc.set_paper_size(Size::new(215.9, 279.4));
    doc.set_font_size(font_size.round().clamp(8.0, 72.0) as u8);
    doc.set_line_spacing(1.5);
    let mut decorator = SimplePageDecorator::new();
    decorator.set_margins(Margins::all(25.4));
    doc.set_page_decorator(decorator);

    let style = Style::new().with_color(Color::Rgb(51, 51, 51));
    for line in text.trim().lines() {
        if line.trim().is_empty() {
            doc.push(Break::new(1));
        } else {
            doc.push(Paragraph::new(line).styled(style));
        }
    }

    doc.render_to_file(path).map_err(|e| e.to_string())
}

/// The first four words, hyphenated: "what-am-i-working.pdf".
pub fn title_for(text: &str) -> String {
    let words: Vec<String> = text
        .split_whitespace()
        .map(|word| {
            word.trim_matches(|c: char| ".,!?;:\"'()[]{}<>".contains(c))
                .to_lowercase()
        })
        .filter(|word| !word.is_empty())
        .take(4)
        .collect();
    if words.is_empty() {
        "Entry".to_string()
    } else {
        words.join("-")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_use_the_first_four_words() {
        assert_eq!(
            title_for("\n\nWhat am I working on today?"),
            "what-am-i-working"
        );
        assert_eq!(title_for("\n\nHi."), "hi");
        assert_eq!(title_for("\n\n  "), "Entry");
    }

    #[test]
    fn renders_a_pdf() {
        let path = std::env::temp_dir().join(format!("freewrite-{}.pdf", uuid::Uuid::new_v4()));
        let text = "\n\nfirst line\n\nsecond paragraph that is long enough to wrap ".repeat(40);
        export(&text, crate::fonts::LATO.to_vec(), 18.0, &path).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.starts_with(b"%PDF"));
        std::fs::remove_file(path).unwrap();
    }
}
