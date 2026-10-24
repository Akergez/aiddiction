//! The dialog an annotation is written in.

use gpui_kit::component::input::{Textarea, TextareaState};
use gpui_kit::component::{ActiveTheme, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{App, AppContext, Context, Entity, FocusHandle, Focusable, Render, SharedString, Window, div};

pub struct AnnotationEditor {
    pub text: Entity<TextareaState>,
    context: SharedString,
}

impl AnnotationEditor {
    /// `context` says what is being annotated (the line, quoted).
    pub fn new(context: SharedString, existing: &str, window: &mut Window, cx: &mut App) -> Self {
        let existing = existing.to_owned();
        let text = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(3, 12)
                .placeholder("What should change here? A question can end in ??")
                .default_value(existing)
        });
        Self { text, context }
    }

    pub fn focus(&self, cx: &App) -> FocusHandle {
        self.text.read(cx).focus_handle(cx)
    }

    pub fn value(&self, cx: &App) -> String {
        self.text.read(cx).value().trim().to_owned()
    }
}

impl Render for AnnotationEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        v_flex()
            .gap_2()
            .child(
                div()
                    .text_xs()
                    .font_family(theme.mono_font_family.clone())
                    .text_color(theme.muted_foreground)
                    .truncate()
                    .child(self.context.clone()),
            )
            .child(Textarea::new(&self.text))
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("Leave it empty to remove the annotation. Mention “hunk” to cover the whole run of changes."),
            )
    }
}
