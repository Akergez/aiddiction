//! Syntax colours for a diff, from the toolkit's tree-sitter highlighter.
//!
//! A diff line is a fragment; parsed alone, a line inside a block comment or
//! a multi-line string comes out wrong. So the two versions of the file are
//! put back together from the diff — context and removed lines are the old
//! one, context and added lines the new one — each is parsed whole, and the
//! styles are handed back out line by line. With the whole file in the diff
//! (the default) that is the real file; with a compact diff it is the part
//! of it there is.

use std::ops::Range;

use diffreview::{FileDiff, LineKind};
use std::sync::Arc;

use gpui_kit::component::Theme;
use gpui_kit::{App, HighlightStyle};
use gpui_kit::component::Rope;
use gpui_kit::component::highlighter::{HighlightTheme, LanguageRegistry, SyntaxHighlighter};

/// Styles of one line, as byte ranges into its [`display_text`].
pub type LineStyles = Vec<(Range<usize>, HighlightStyle)>;

/// A file larger than this is drawn without colours rather than slowly.
const MAX_BYTES: usize = 2 << 20;

/// A line as it is drawn: tabs as four spaces.
pub fn display_text(text: &str) -> String {
    text.replace('\t', "    ")
}

/// The syntax colours for the window's mode. The toolkit keeps the theme's
/// highlight colours only when the theme brings some; a theme without them
/// (a colour scheme, say) leaves the previous ones in place, which can be the
/// other mode's — dark ink on a dark background. Those are passed over for
/// the toolkit's own palette of the right mode.
pub fn theme_for(cx: &App) -> Arc<HighlightTheme> {
    fitting(Theme::global(cx))
}

fn fitting(theme: &Theme) -> Arc<HighlightTheme> {
    if theme.highlight_theme.appearance == theme.mode {
        theme.highlight_theme.clone()
    } else if theme.mode.is_dark() {
        HighlightTheme::default_dark()
    } else {
        HighlightTheme::default_light()
    }
}

/// The grammar for a path: by file name for the few without an extension
/// that matters, by extension otherwise. `None` when there is none.
pub fn language_of(path: &str) -> Option<String> {
    let name = path.rsplit('/').next().unwrap_or(path);
    let language = match name {
        "Makefile" | "GNUmakefile" => "make".to_owned(),
        "CMakeLists.txt" => "cmake".to_owned(),
        "Cargo.lock" => "toml".to_owned(),
        _ => name.rsplit_once('.')?.1.to_lowercase(),
    };
    LanguageRegistry::singleton().language(&language).is_some().then_some(language)
}

/// One entry per line of `file`, empty for a line with nothing to colour.
pub fn highlight(file: &FileDiff, theme: &HighlightTheme) -> Vec<LineStyles> {
    let empty = || vec![Vec::new(); file.lines.len()];
    let Some(language) = language_of(&file.path) else { return empty() };

    #[derive(Clone, Copy)]
    enum Side {
        Old,
        New,
        Neither,
    }
    let (mut old, mut new) = (String::new(), String::new());
    let mut spans = Vec::with_capacity(file.lines.len());
    for line in &file.lines {
        let text = display_text(&line.text);
        let push = |into: &mut String| {
            let start = into.len();
            into.push_str(&text);
            into.push('\n');
            start..start + text.len()
        };
        spans.push(match line.kind {
            LineKind::Hunk => (Side::Neither, 0..0),
            LineKind::Remove => (Side::Old, push(&mut old)),
            LineKind::Add => (Side::New, push(&mut new)),
            LineKind::Context => {
                push(&mut old);
                (Side::New, push(&mut new))
            }
        });
        if old.len() + new.len() > MAX_BYTES {
            return empty();
        }
    }

    let old_styles = styles(&language, &old, theme);
    let new_styles = styles(&language, &new, theme);
    spans
        .into_iter()
        .map(|(side, span)| match side {
            Side::Old => slice(&old_styles, span),
            Side::New => slice(&new_styles, span),
            Side::Neither => Vec::new(),
        })
        .collect()
}

fn styles(language: &str, text: &str, theme: &HighlightTheme) -> LineStyles {
    if text.is_empty() {
        return Vec::new();
    }
    let mut highlighter = SyntaxHighlighter::new(language);
    highlighter.update(None, &Rope::from_str(text), None);
    highlighter.styles(&(0..text.len()), theme)
}

/// The styles that fall in `span`, moved to start at the line's start.
fn slice(styles: &LineStyles, span: Range<usize>) -> LineStyles {
    let first = styles.partition_point(|(range, _)| range.end <= span.start);
    styles[first..]
        .iter()
        .take_while(|(range, _)| range.start < span.end)
        .filter_map(|(range, style)| {
            let start = range.start.max(span.start);
            let end = range.end.min(span.end);
            (start < end && *style != HighlightStyle::default()).then(|| (start - span.start..end - span.start, *style))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn languages_by_name_and_extension() {
        assert_eq!(language_of("src/lib.rs").as_deref(), Some("rs"));
        assert_eq!(language_of("Makefile").as_deref(), Some("make"));
        assert_eq!(language_of("notes.unknownext"), None);
        assert_eq!(language_of("LICENSE"), None);
    }

    #[test]
    fn a_style_is_cut_to_each_line_it_covers() {
        let red = HighlightStyle { font_weight: Some(gpui_kit::FontWeight::BOLD), ..Default::default() };
        let styles = vec![(2..9, red)];
        assert_eq!(slice(&styles, 0..4), vec![(2..4, red)]);
        assert_eq!(slice(&styles, 5..12), vec![(0..4, red)]);
        assert!(slice(&styles, 10..12).is_empty());
    }
}
