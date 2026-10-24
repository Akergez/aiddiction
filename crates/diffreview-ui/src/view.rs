//! The review: a toolbar to pick what to review, the changed files, and the
//! diff of one with its annotations.

use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;

use diffreview::annotation::{self, Anchor, Annotation, Store};
use diffreview::{DiffRequest, DiffSource, FileDiff, FileStatus, LineKind, Refs};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::list::ListItem;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tree::{TreeItem, TreeState, tree};
use gpui_kit::component::{ActiveTheme, Icon, IconName, Selectable as _, Sizable as _, WindowExt, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, AppContext, ClipboardItem, Context, ElementId, Entity, EventEmitter, FocusHandle, Hsla, Render,
    ScrollStrategy, SharedString, StyledText, Subscription, UniformListScrollHandle, Window, div, uniform_list,
};

use crate::editor::AnnotationEditor;
use crate::highlight::{self, LineStyles};
use crate::rows::{self, Row};
use crate::{
    Annotate, AnnotateFile, CONTEXT, CursorDown, CursorUp, DeleteAnnotation, NextFile, NextHunk, PageDown, PageUp,
    PreviousFile, PreviousHunk, Reload, ReviewBackend, ReviewEvent, ReviewOptions, Submit, ToggleReviewed,
};

/// How far a page move goes.
const PAGE: usize = 20;

pub struct ReviewView {
    backend: Rc<dyn ReviewBackend>,
    options: ReviewOptions,
    request: DiffRequest,
    refs: Option<Refs>,
    files: Vec<FileDiff>,
    loading: bool,
    error: Option<SharedString>,
    /// The file on screen, by index into `files`.
    file: usize,
    rows: Vec<Row>,
    cursor: usize,
    store: Store,
    reviewed: HashSet<String>,
    scroll: UniformListScrollHandle,
    focus: FocusHandle,
    ref_input: Entity<InputState>,
    /// On a narrow window: the list of files instead of the diff.
    showing_files: bool,
    /// Bumped on every load, so an answer to an older one is dropped.
    generation: u64,
    /// The files as folders, in the toolkit's tree.
    tree: Entity<TreeState>,
    /// File indexes in the order the tree shows them.
    order: Vec<usize>,
    /// The colours of the open file's lines, until it, the diff or the theme
    /// changes.
    highlights: Option<Highlights>,
    _subscriptions: Vec<Subscription>,
}

struct Highlights {
    file: usize,
    generation: u64,
    theme: usize,
    lines: Rc<Vec<LineStyles>>,
}

impl EventEmitter<ReviewEvent> for ReviewView {}

impl ReviewView {
    pub fn new(
        backend: Rc<dyn ReviewBackend>,
        request: DiffRequest,
        options: ReviewOptions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut this = Self {
            backend,
            options,
            request,
            refs: None,
            files: Vec::new(),
            loading: false,
            error: None,
            file: 0,
            rows: Vec::new(),
            cursor: 0,
            store: Store::default(),
            reviewed: HashSet::new(),
            scroll: UniformListScrollHandle::new(),
            focus: cx.focus_handle(),
            ref_input: cx.new(|cx| InputState::new(window, cx).placeholder("ref, e.g. HEAD~3")),
            showing_files: true,
            generation: 0,
            tree: cx.new(|cx| TreeState::new(cx)),
            order: Vec::new(),
            highlights: None,
            _subscriptions: Vec::new(),
        };
        // A file chosen in the tree, by mouse or by keys, is opened.
        let tree = this.tree.clone();
        this._subscriptions.push(cx.observe(&tree, |this, tree, cx| {
            let chosen = tree.read(cx).selected_item().and_then(|item| item.id.strip_prefix("f:").map(str::to_owned));
            if let Some(index) = chosen.and_then(|path| this.files.iter().position(|file| file.path == path))
                && (index != this.file || this.showing_files) {
                    this.open_file(index, cx);
                }
        }));
        this.load(cx);
        this.load_refs(cx);
        this
    }

    pub fn focus_handle(&self) -> FocusHandle {
        self.focus.clone()
    }

    pub fn annotations(&self) -> &Store {
        &self.store
    }

    /// Starts from annotations made before (revdiff's `--annotations`).
    pub fn set_annotations(&mut self, store: Store, cx: &mut Context<Self>) {
        self.store = store;
        self.rebuild_rows(cx);
    }

    pub fn request(&self) -> &DiffRequest {
        &self.request
    }

    pub fn set_request(&mut self, request: DiffRequest, cx: &mut Context<Self>) {
        self.request = request;
        self.load(cx);
    }

    /// Asks for the diff again. Annotations stay: they are by file and line,
    /// as revdiff keeps them across a reload.
    pub fn load(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        self.loading = true;
        self.error = None;
        cx.notify();
        let this = cx.entity().downgrade();
        let backend = self.backend.clone();
        backend.diff(
            self.request.clone(),
            cx,
            Box::new(move |result, cx| {
                let _ = this.update(cx, |view, cx| {
                    if view.generation != generation {
                        return;
                    }
                    view.loading = false;
                    match result {
                        Ok(text) => view.show(diffreview::diff::parse(&text), cx),
                        Err(error) => view.error = Some(error.into()),
                    }
                    cx.notify();
                });
            }),
        );
    }

    fn load_refs(&mut self, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        let backend = self.backend.clone();
        backend.refs(
            cx,
            Box::new(move |result, cx| {
                let _ = this.update(cx, |view, cx| {
                    view.refs = result.ok();
                    cx.notify();
                });
            }),
        );
    }

    /// Takes a new list of files, staying on the file that was open.
    fn show(&mut self, files: Vec<FileDiff>, cx: &mut Context<Self>) {
        let open = self.files.get(self.file).map(|file| file.path.clone());
        self.files = files;
        let nodes = diffreview::tree::build(&self.files);
        self.order = diffreview::tree::file_order(&nodes);
        self.file = open
            .and_then(|path| self.files.iter().position(|file| file.path == path))
            .or_else(|| self.order.first().copied())
            .unwrap_or(0);
        let items: Vec<TreeItem> = nodes.iter().map(tree_item).collect();
        self.tree.update(cx, |tree, cx| tree.set_items(items, cx));
        self.sync_tree(cx);
        self.rebuild_rows(cx);
        self.cursor = self.current().map_or(0, |file| rows::first_change(file, &self.rows));
        self.scroll.scroll_to_item(self.cursor, ScrollStrategy::Center);
    }

    fn current(&self) -> Option<&FileDiff> {
        self.files.get(self.file)
    }

    fn rebuild_rows(&mut self, cx: &mut Context<Self>) {
        self.rows = self.current().map(|file| rows::rows(file, &self.store)).unwrap_or_default();
        self.cursor = self.cursor.min(self.rows.len().saturating_sub(1));
        cx.notify();
    }

    /// Shows file `index`, its first change under the cursor.
    fn open_file(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.files.len() {
            return;
        }
        self.file = index;
        self.showing_files = false;
        self.rebuild_rows(cx);
        self.cursor = self.current().map_or(0, |file| rows::first_change(file, &self.rows));
        self.scroll.scroll_to_item(self.cursor, ScrollStrategy::Center);
    }

    /// Opens a file from a key or a button, and keeps the tree's selection
    /// on it.
    fn select_file(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.open_file(index, cx);
        self.sync_tree(cx);
        self.focus.focus(window, cx);
    }

    /// The next (or previous) file in the tree's order.
    fn step_file(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let at = self.order.iter().position(|index| *index == self.file);
        let next = match (at, forward) {
            (Some(at), true) => self.order.get(at + 1),
            (Some(at), false) => at.checked_sub(1).and_then(|at| self.order.get(at)),
            (None, _) => self.order.first(),
        };
        if let Some(next) = next.copied() {
            self.select_file(next, window, cx);
        }
    }

    fn sync_tree(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.current().map(|file| SharedString::from(format!("f:{}", file.path))) else { return };
        self.tree.update(cx, |tree, cx| {
            let index = tree.index_of(&path);
            if index.is_some() && tree.selected_index() != index {
                tree.set_selected_index(index, cx);
                if let Some(index) = index {
                    tree.scroll_to_item(index, ScrollStrategy::Nearest);
                }
            }
        });
    }

    /// Colours the open file, unless what is cached is still right.
    fn ensure_highlights(&mut self, cx: &mut Context<Self>) {
        let theme = highlight::theme_for(cx);
        let key = Arc::as_ptr(&theme) as usize;
        let fresh = self.highlights.as_ref().is_some_and(|cached| {
            cached.file == self.file && cached.generation == self.generation && cached.theme == key
        });
        if fresh {
            return;
        }
        let lines = self.current().map(|file| highlight::highlight(file, &theme)).unwrap_or_default();
        self.highlights = Some(Highlights { file: self.file, generation: self.generation, theme: key, lines: Rc::new(lines) });
    }

    fn move_cursor(&mut self, to: usize, cx: &mut Context<Self>) {
        if self.rows.is_empty() {
            return;
        }
        self.cursor = to.min(self.rows.len() - 1);
        self.scroll.scroll_to_item(self.cursor, ScrollStrategy::Nearest);
        cx.notify();
    }

    fn move_hunk(&mut self, forward: bool, cx: &mut Context<Self>) {
        let Some(file) = self.current() else { return };
        if let Some(row) = rows::hunk_row(file, &self.rows, self.cursor, forward) {
            self.cursor = row;
            self.scroll.scroll_to_item(row, ScrollStrategy::Center);
            cx.notify();
        }
    }

    fn set_source(&mut self, source: DiffSource, cx: &mut Context<Self>) {
        self.request.source = source;
        self.load(cx);
    }

    /// The line under the cursor (a note row stands for its line).
    fn cursor_line(&self) -> Option<usize> {
        match self.rows.get(self.cursor)? {
            Row::Line(index) | Row::Note(index) => Some(*index),
            Row::FileNote => None,
        }
    }

    fn annotate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.rows.get(self.cursor), Some(Row::FileNote)) {
            return self.annotate_file(window, cx);
        }
        let Some(file) = self.current() else { return };
        let Some(index) = self.cursor_line() else { return };
        let line = &file.lines[index];
        let Some(anchor) = Anchor::of(line) else { return };
        let Anchor::Line { line: number, change } = anchor else { return };
        let context = format!("{}:{number} {} {}", file.path, change.marker(), line.text.trim());
        let path = file.path.clone();
        let existing = self.store.get(&path, anchor).map(|annotation| annotation.comment.clone()).unwrap_or_default();
        let hunk_end = annotation::hunk_end(&file.lines, index);
        self.edit(path, anchor, hunk_end, context.into(), existing, window, cx);
    }

    fn annotate_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(file) = self.current() else { return };
        let path = file.path.clone();
        let existing = self.store.get(&path, Anchor::File).map(|annotation| annotation.comment.clone()).unwrap_or_default();
        self.edit(path.clone(), Anchor::File, None, format!("{path} (the whole file)").into(), existing, window, cx);
    }

    /// The dialog: what is written replaces the annotation, nothing removes it.
    #[allow(clippy::too_many_arguments)]
    fn edit(
        &mut self,
        path: String,
        anchor: Anchor,
        hunk_end: Option<u32>,
        context: SharedString,
        existing: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let editor = cx.new(|cx| AnnotationEditor::new(context, &existing, window, cx));
        let focus = editor.read(cx).focus(cx);
        let this = cx.entity().downgrade();
        let title = if anchor == Anchor::File { "File annotation" } else { "Annotation" };
        window.open_dialog(cx, move |dialog, _, _| {
            let (editor, this, path) = (editor.clone(), this.clone(), path.clone());
            dialog
                .title(title)
                .child(editor.clone())
                .footer(
                    DialogFooter::new()
                        .child(DialogClose::new().child(Button::new("cancel").label("Cancel")))
                        .child(DialogAction::new().child(Button::new("ok").label("Save").primary())),
                )
                .on_ok(move |_, window, cx| {
                    let comment = editor.read(cx).value(cx);
                    let _ = this.update(cx, |view, cx| {
                        view.save(&path, anchor, hunk_end, comment, cx);
                        view.focus.focus(window, cx);
                    });
                    true
                })
        });
        focus.focus(window, cx);
    }

    fn save(&mut self, path: &str, anchor: Anchor, hunk_end: Option<u32>, comment: String, cx: &mut Context<Self>) {
        if comment.is_empty() {
            self.store.remove(path, anchor);
        } else {
            let end_line = hunk_end.filter(|_| annotation::mentions_hunk(&comment));
            self.store.add(Annotation { file: path.to_owned(), anchor, end_line, comment });
        }
        self.rebuild_rows(cx);
    }

    fn delete_annotation(&mut self, cx: &mut Context<Self>) {
        let Some(file) = self.current() else { return };
        let path = file.path.clone();
        let anchor = match self.rows.get(self.cursor) {
            Some(Row::FileNote) => Some(Anchor::File),
            Some(Row::Line(index) | Row::Note(index)) => Anchor::of(&file.lines[*index]),
            None => None,
        };
        if let Some(anchor) = anchor {
            self.store.remove(&path, anchor);
            self.rebuild_rows(cx);
        }
    }

    fn toggle_reviewed(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.current().map(|file| file.path.clone()) else { return };
        if !self.reviewed.remove(&path) {
            self.reviewed.insert(path);
        }
        cx.notify();
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        cx.emit(ReviewEvent::Submit(self.store.format()));
    }

    fn copy(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(self.store.format()));
        window.push_notification("Annotations copied", cx);
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let source = &self.request.source;
        let source_button = |id: &'static str, label: SharedString, value: DiffSource, cx: &mut Context<Self>| {
            let selected = *source == value;
            Button::new(id)
                .xsmall()
                .label(label)
                .selected(selected)
                .when(!selected, |button| button.ghost())
                .on_click(cx.listener(move |this, _, _, cx| this.set_source(value.clone(), cx)))
        };
        let mut bar = h_flex()
            .flex_wrap()
            .gap_1()
            .px_2()
            .py_1p5()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(source_button("uncommitted", "Uncommitted".into(), DiffSource::Uncommitted, cx))
            .child(source_button("staged", "Staged".into(), DiffSource::Staged, cx))
            .child(source_button(
                "last-commit",
                "Last commit".into(),
                DiffSource::Between { from: "HEAD~1".into(), to: "HEAD".into() },
                cx,
            ));
        if let Some(refs) = &self.refs
            && let Some(base) = refs.default_branch.clone().filter(|base| refs.current_branch.as_ref() != Some(base)) {
                let label: SharedString = format!("Branch vs {base}").into();
                bar = bar.child(source_button("branch", label, DiffSource::Since { base, head: "HEAD".into() }, cx));
            }
        let full = self.request.context.is_none();
        let untracked = self.request.untracked;
        bar.child(div().w_32().child(Input::new(&self.ref_input).xsmall()))
            .child(Button::new("against").xsmall().ghost().label("Diff").on_click(cx.listener(|this, _, _, cx| {
                let base = this.ref_input.read(cx).value().trim().to_owned();
                if !base.is_empty() {
                    this.set_source(DiffSource::Against { base }, cx);
                }
            })))
            .child(div().w_2())
            .child(Button::new("full").xsmall().label(if full { "Full files" } else { "Compact" }).ghost().on_click(
                cx.listener(move |this, _, _, cx| {
                    this.request.context = if full { Some(3) } else { None };
                    this.load(cx);
                }),
            ))
            .child(Button::new("untracked").xsmall().ghost().selected(untracked).label("Untracked").on_click(cx.listener(
                move |this, _, _, cx| {
                    this.request.untracked = !untracked;
                    this.load(cx);
                },
            )))
            .child(Button::new("reload").xsmall().ghost().icon(IconName::RefreshCw).tooltip("Reload (R)").on_click(
                cx.listener(|this, _, _, cx| this.load(cx)),
            ))
            .child(div().flex_1())
            .child(div().text_xs().text_color(cx.theme().muted_foreground).child(match self.store.count() {
                0 => "No annotations".to_owned(),
                1 => "1 annotation".to_owned(),
                count => format!("{count} annotations"),
            }))
            .child(Button::new("copy").xsmall().ghost().icon(IconName::Copy).tooltip("Copy annotations").on_click(
                cx.listener(|this, _, window, cx| this.copy(window, cx)),
            ))
            .when_some(self.options.discard_label.clone(), |bar, label| {
                bar.child(Button::new("discard").xsmall().label(label).on_click(cx.listener(|_, _, _, cx| cx.emit(ReviewEvent::Discard))))
            })
            .child(
                Button::new("submit")
                    .xsmall()
                    .primary()
                    .label(self.options.submit_label.clone())
                    .tooltip("Ctrl+Enter")
                    .on_click(cx.listener(|this, _, _, cx| this.submit(cx))),
            )
    }

    fn render_files(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        if self.files.is_empty() {
            let text = if self.loading { "" } else { "No changes" };
            return div().p_3().text_sm().text_color(theme.muted_foreground).child(text).into_any_element();
        }
        let view = cx.entity().downgrade();
        tree(&self.tree, move |index, entry, selected, _window, cx| {
            let item = entry.item();
            let indent = gpui_kit::rems(0.75 * entry.depth() as f32);
            let mut row = h_flex().w_full().gap_1p5().pl(indent).text_sm();
            match item.id.strip_prefix("f:") {
                None => {
                    let icon = if entry.is_expanded() { IconName::FolderOpen } else { IconName::FolderClosed };
                    row = row
                        .child(Icon::new(icon).xsmall().text_color(theme.muted_foreground))
                        .child(div().flex_1().min_w_0().truncate().text_color(theme.muted_foreground).child(item.label.clone()));
                }
                Some(path) => {
                    let view = view.upgrade();
                    let view = view.as_ref().map(|view| view.read(cx));
                    let file = view.and_then(|view| view.files.iter().find(|file| file.path == path));
                    let notes = view.map_or(0, |view| view.store.count_in(path));
                    let reviewed = view.is_some_and(|view| view.reviewed.contains(path));
                    let (letter, color, added, removed) = match file {
                        Some(file) => (file.status.letter(), status_color(file.status, &theme), file.added, file.removed),
                        None => (' ', theme.muted_foreground, 0, 0),
                    };
                    row = row
                        .child(div().w_3().flex_none().text_xs().font_family(theme.mono_font_family.clone()).text_color(color).child(letter.to_string()))
                        .child(div().flex_1().min_w_0().truncate().when(reviewed, |name| name.opacity(0.5)).child(item.label.clone()))
                        .when(notes > 0, |row| row.child(div().text_xs().text_color(theme.warning).child(format!("●{notes}"))))
                        .child(
                            h_flex()
                                .flex_none()
                                .gap_1()
                                .text_xs()
                                .font_family(theme.mono_font_family.clone())
                                .child(div().text_color(theme.success).child(format!("+{added}")))
                                .child(div().text_color(theme.danger).child(format!("-{removed}"))),
                        );
                }
            }
            ListItem::new(index).selected(selected).py_0p5().child(row)
        })
        .into_any_element()
    }

    fn render_row(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let Some(file) = self.current() else { return div().into_any_element() };
        let selected = index == self.cursor;
        let base = h_flex()
            .id(ElementId::Name(format!("review-row-{index}").into()))
            .h_6()
            .w_full()
            .text_xs()
            .font_family(theme.mono_font_family.clone())
            .border_l_2()
            .border_color(if selected { theme.primary } else { Hsla::transparent_black() })
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, window, cx| {
                this.focus.focus(window, cx);
                if this.cursor == index {
                    this.annotate(window, cx);
                } else {
                    this.move_cursor(index, cx);
                }
            }));
        // A note is one row: its first line, and a mark when there is more.
        let note = match self.rows[index] {
            Row::FileNote => Some(("› file: ", self.store.get(&file.path, Anchor::File))),
            Row::Note(line) => Some(("› ", Anchor::of(&file.lines[line]).and_then(|anchor| self.store.get(&file.path, anchor)))),
            Row::Line(_) => None,
        };
        if let Some((prefix, annotation)) = note {
            let text = annotation.map(|annotation| annotation.comment.as_str()).unwrap_or_default();
            let first = text.lines().next().unwrap_or_default();
            let more = if text.lines().count() > 1 { " …" } else { "" };
            return base
                .bg(theme.warning.opacity(0.12))
                .pl(gpui_kit::rems(5.5))
                .child(div().truncate().text_color(theme.foreground).child(format!("{prefix}{first}{more}")))
                .into_any_element();
        }
        match self.rows[index] {
            Row::FileNote | Row::Note(_) => unreachable!("drawn above"),
            Row::Line(line_index) => {
                let line = &file.lines[line_index];
                let number = |number: Option<u32>| {
                    div()
                        .w_10()
                        .flex_none()
                        .pr_1()
                        .text_right()
                        .text_color(theme.muted_foreground)
                        .child(number.map(|number| number.to_string()).unwrap_or_default())
                };
                let (background, marker) = match line.kind {
                    LineKind::Add => (Some(theme.success.opacity(0.14)), "+"),
                    LineKind::Remove => (Some(theme.danger.opacity(0.14)), "-"),
                    LineKind::Hunk => (Some(theme.info.opacity(0.10)), ""),
                    LineKind::Context => (None, " "),
                };
                let text = highlight::display_text(&line.text);
                let styles = match &self.highlights {
                    Some(cached) if cached.file == self.file => cached.lines.get(line_index).cloned().unwrap_or_default(),
                    _ => Vec::new(),
                };
                base.when_some(background, |row, background| row.bg(background))
                    .when(selected, |row| row.bg(theme.accent))
                    .child(number(line.old))
                    .child(number(line.new))
                    .child(div().w_4().flex_none().text_color(theme.muted_foreground).child(marker))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .when(line.kind == LineKind::Hunk, |text| text.text_color(theme.muted_foreground))
                            .child(StyledText::new(text).with_highlights(styles)),
                    )
                    .into_any_element()
            }
        }
    }

    fn render_diff(&self, phone: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let Some(file) = self.current() else {
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(theme.muted_foreground)
                .child(if self.loading { "Reading the diff…" } else { "Nothing to review" })
                .into_any_element();
        };
        let reviewed = self.reviewed.contains(&file.path);
        let title: SharedString = match &file.old_path {
            Some(old) => format!("{old} → {}", file.path).into(),
            None => file.path.clone().into(),
        };
        let header = h_flex()
            .gap_1()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(theme.border)
            .when(phone, |header| {
                header.child(Button::new("files").xsmall().ghost().icon(IconName::ArrowLeft).on_click(cx.listener(|this, _, _, cx| {
                    this.showing_files = true;
                    cx.notify();
                })))
            })
            .child(div().flex_1().min_w_0().truncate().text_sm().child(title))
            .child(Button::new("prev-file").xsmall().ghost().icon(IconName::ChevronUp).tooltip("Previous file (p)").on_click(
                cx.listener(|this, _, window, cx| this.step_file(false, window, cx)),
            ))
            .child(Button::new("next-file").xsmall().ghost().icon(IconName::ChevronDown).tooltip("Next file (n)").on_click(
                cx.listener(|this, _, window, cx| this.step_file(true, window, cx)),
            ))
            .child(Button::new("file-note").xsmall().ghost().label("File note").tooltip("Shift+A").on_click(
                cx.listener(|this, _, window, cx| this.annotate_file(window, cx)),
            ))
            .child(Button::new("reviewed").xsmall().ghost().selected(reviewed).icon(IconName::Check).label("Reviewed").tooltip("Space").on_click(
                cx.listener(|this, _, _, cx| this.toggle_reviewed(cx)),
            ));
        let body = if file.binary {
            div().p_4().text_sm().text_color(theme.muted_foreground).child("Binary file").into_any_element()
        } else {
            uniform_list(
                "review-rows",
                self.rows.len(),
                cx.processor(|this, range: std::ops::Range<usize>, _window, cx| {
                    range.map(|index| this.render_row(index, cx)).collect::<Vec<_>>()
                }),
            )
            .track_scroll(&self.scroll)
            .size_full()
            .into_any_element()
        };
        v_flex().size_full().child(header).child(div().flex_1().min_h_0().child(body)).into_any_element()
    }
}

impl Render for ReviewView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_highlights(cx);
        let theme = cx.theme().clone();
        let phone = window.viewport_size().width / window.rem_size() < 44.0;
        let status = if self.loading {
            Some(h_flex().gap_2().px_3().py_1().text_xs().child(Spinner::new().xsmall()).child(format!("{}…", self.request.source.label())).into_any_element())
        } else {
            self.error.clone().map(|error| div().px_3().py_1().text_xs().text_color(theme.danger).child(error).into_any_element())
        };
        let body = if phone {
            if self.showing_files { self.render_files(cx) } else { self.render_diff(true, cx) }
        } else {
            h_flex()
                .size_full()
                .items_start()
                .child(div().w_64().flex_none().h_full().border_r_1().border_color(theme.border).child(self.render_files(cx)))
                .child(div().flex_1().min_w_0().h_full().child(self.render_diff(false, cx)))
                .into_any_element()
        };
        v_flex()
            .size_full()
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &CursorDown, _, cx| this.move_cursor(this.cursor + 1, cx)))
            .on_action(cx.listener(|this, _: &CursorUp, _, cx| this.move_cursor(this.cursor.saturating_sub(1), cx)))
            .on_action(cx.listener(|this, _: &PageDown, _, cx| this.move_cursor(this.cursor + PAGE, cx)))
            .on_action(cx.listener(|this, _: &PageUp, _, cx| this.move_cursor(this.cursor.saturating_sub(PAGE), cx)))
            .on_action(cx.listener(|this, _: &NextFile, window, cx| this.step_file(true, window, cx)))
            .on_action(cx.listener(|this, _: &PreviousFile, window, cx| this.step_file(false, window, cx)))
            .on_action(cx.listener(|this, _: &NextHunk, _, cx| this.move_hunk(true, cx)))
            .on_action(cx.listener(|this, _: &PreviousHunk, _, cx| this.move_hunk(false, cx)))
            .on_action(cx.listener(|this, _: &Annotate, window, cx| this.annotate(window, cx)))
            .on_action(cx.listener(|this, _: &AnnotateFile, window, cx| this.annotate_file(window, cx)))
            .on_action(cx.listener(|this, _: &DeleteAnnotation, _, cx| this.delete_annotation(cx)))
            .on_action(cx.listener(|this, _: &ToggleReviewed, _, cx| this.toggle_reviewed(cx)))
            .on_action(cx.listener(|this, _: &Reload, _, cx| this.load(cx)))
            .on_action(cx.listener(|this, _: &Submit, _, cx| this.submit(cx)))
            .child(self.render_toolbar(cx))
            .children(status)
            .child(div().flex_1().min_h_0().child(body))
    }
}

/// A node of the file tree as the toolkit's tree item: files by `f:` and
/// folders by `d:` and their path, every folder open.
fn tree_item(node: &diffreview::tree::Node) -> TreeItem {
    let kind = if node.file.is_some() { "f" } else { "d" };
    TreeItem::new(format!("{kind}:{}", node.path), node.name.clone())
        .expanded(true)
        .children(node.children.iter().map(tree_item))
}

fn status_color(status: FileStatus, theme: &gpui_kit::component::Theme) -> Hsla {
    match status {
        FileStatus::Added => theme.success,
        FileStatus::Deleted => theme.danger,
        FileStatus::Renamed => theme.info,
        FileStatus::Modified => theme.warning,
    }
}
