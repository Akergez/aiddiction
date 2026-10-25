//! A conversation with an agent: the thread, the plan, the agent's questions,
//! and the composer.

use std::collections::HashSet;

use aiddiction_api::ProjectId;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::text::TextView;
use gpui_kit::component::{ActiveTheme, Icon, IconName, h_flex, v_flex};
use gpui_kit::component::{Disableable as _, Selectable as _, Sizable as _};
use gpui_kit::Focusable as _;
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, AppContext, Context, ElementId, Entity, EventEmitter, FollowMode, ListAlignment, ListState,
    Render, SharedString, Subscription, Window, div, list, px,
};

use crate::session::{AgentSession, Phase};
use crate::thread::{Entry, ToolCall, ToolContent, ToolStatus};

/// Asks the shell to close this conversation.
pub struct Close;

/// Asks the shell for the review of this conversation's project.
pub struct OpenReview;

impl EventEmitter<Close> for ChatView {}
impl EventEmitter<OpenReview> for ChatView {}

pub struct ChatView {
    pub session: Entity<AgentSession>,
    pub project_id: ProjectId,
    composer: Entity<TextareaState>,
    list: ListState,
    /// Entries whose thoughts or tool output are unfolded.
    open: HashSet<usize>,
    subscriptions: Vec<Subscription>,
}

impl ChatView {
    pub fn new(session: Entity<AgentSession>, project_id: ProjectId, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 8)
                // Enter sends; Shift+Enter inserts a newline.
                .submit_on_enter(true)
                .placeholder("Ask the agent…")
        });
        let list = ListState::new(0, ListAlignment::Bottom, px(400.));
        list.set_follow_mode(FollowMode::Tail);

        let subscriptions = vec![
            cx.observe(&session, |this, session, cx| {
                let (count, dirty) = session.update(cx, |session, _| (session.thread.entries.len(), session.thread.take_dirty()));
                let shown = this.list.item_count();
                if let Some(from) = dirty.or((count != shown).then_some(shown.min(count))) {
                    let from = from.min(shown);
                    this.list.splice(from..shown, count - from);
                }
                cx.notify();
            }),
            cx.subscribe_in(&composer, window, |this, _, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { shift: false, .. } = event {
                    this.send(window, cx);
                }
            }),
        ];
        Self { session, project_id, composer, list, open: HashSet::new(), subscriptions }
    }

    /// Where typing goes: the composer.
    pub fn composer_focus(&self, cx: &App) -> gpui_kit::FocusHandle {
        self.composer.read(cx).focus_handle(cx)
    }

    /// Keeps a subscription for as long as this view lives.
    pub fn keep(&mut self, subscription: Subscription) {
        self.subscriptions.push(subscription);
    }

    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.composer.read(cx).value().to_string();
        let (ready, busy) = {
            let session = self.session.read(cx);
            (session.is_ready(), session.busy)
        };
        if text.trim().is_empty() || !ready || busy {
            return;
        }
        self.session.update(cx, |session, cx| session.prompt(text, cx));
        self.composer.update(cx, |composer, cx| composer.set_value("", window, cx));
        self.list.scroll_to_end();
    }

    fn toggle(&mut self, index: usize, cx: &mut Context<Self>) {
        if !self.open.remove(&index) {
            self.open.insert(index);
        }
        self.list.remeasure_items(index..index + 1);
        cx.notify();
    }

    fn render_entry(&mut self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let entry = self.session.read(cx).thread.entries.get(index).cloned();
        let Some(entry) = entry else {
            return div().into_any_element();
        };
        let open = self.open.contains(&index);
        let body = match entry {
            Entry::User { text } => h_flex()
                .justify_end()
                .child(
                    div()
                        .max_w(gpui_kit::relative(0.85))
                        .px_3()
                        .py_2()
                        .rounded(theme.radius * 2.)
                        .bg(theme.secondary)
                        .text_color(theme.secondary_foreground)
                        .child(text),
                )
                .into_any_element(),
            Entry::Assistant { text, thought } => {
                let mut column = v_flex().gap_1();
                if !thought.is_empty() {
                    column = column.child(
                        v_flex()
                            .id(ElementId::Name(format!("thought-{index}").into()))
                            .cursor_pointer()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .on_click(cx.listener(move |this, _, _, cx| this.toggle(index, cx)))
                            .child(
                                h_flex()
                                    .gap_1()
                                    .child(Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight }).xsmall())
                                    .child("Thinking"),
                            )
                            .when(open, |this| this.child(div().pl_4().child(thought))),
                    );
                }
                if !text.is_empty() {
                    column = column.child(TextView::markdown(ElementId::Name(format!("message-{index}").into()), text)
                            .selectable(true)
                            .on_link_click(|url, _, _, cx| super::open_url(url, cx)));
                }
                column.into_any_element()
            }
            Entry::Tool(call) => self.render_tool(index, call, open, cx),
            Entry::Notice { text, error } => div()
                .w_full()
                .text_sm()
                .text_center()
                .text_color(if error { theme.danger } else { theme.muted_foreground })
                .child(text)
                .into_any_element(),
        };
        div().w_full().px_4().py_1p5().child(body).into_any_element()
    }

    fn render_tool(&self, index: usize, call: ToolCall, open: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let status: AnyElement = match call.status {
            ToolStatus::Pending => Icon::new(IconName::Ellipsis).small().text_color(theme.muted_foreground).into_any_element(),
            ToolStatus::InProgress => Spinner::new().small().into_any_element(),
            ToolStatus::Completed => Icon::new(IconName::CircleCheck).small().text_color(theme.success).into_any_element(),
            ToolStatus::Failed => Icon::new(IconName::CircleX).small().text_color(theme.danger).into_any_element(),
        };
        let icon = match call.kind.as_str() {
            "edit" | "delete" | "move" => IconName::FileText,
            "execute" => IconName::SquareTerminal,
            "search" => IconName::Search,
            "fetch" => IconName::Globe,
            "think" => IconName::Bot,
            _ => IconName::Settings2,
        };
        let title = if call.title.is_empty() { call.kind.clone() } else { call.title.clone() };
        let has_content = !call.content.is_empty() || !call.locations.is_empty();
        let mut card = v_flex()
            .border_1()
            .border_color(theme.border)
            .rounded(theme.radius)
            .child(
                h_flex()
                    .id(ElementId::Name(format!("tool-{index}").into()))
                    .gap_2()
                    .px_3()
                    .py_2()
                    .when(has_content, |row| row.cursor_pointer().on_click(cx.listener(move |this, _, _, cx| this.toggle(index, cx))))
                    .child(Icon::new(icon).small().text_color(theme.muted_foreground))
                    .child(div().flex_1().min_w_0().text_sm().truncate().child(title))
                    .child(status),
            );
        if open {
            let mut content = v_flex().gap_2().px_3().pb_3();
            for path in &call.locations {
                content = content.child(div().text_xs().text_color(theme.muted_foreground).truncate().child(path.clone()));
            }
            for item in call.content {
                content = content.child(match item {
                    ToolContent::Text(text) => div().text_sm().child(text).into_any_element(),
                    ToolContent::Diff { path, old, new } => diff_view(&path, old.as_deref(), &new, cx),
                    ToolContent::Terminal(id) => div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(format!("Terminal {id}"))
                        .into_any_element(),
                });
            }
            card = card.child(content);
        }
        card.into_any_element()
    }

    fn render_plan(&self, cx: &App) -> Option<AnyElement> {
        let theme = cx.theme();
        let plan = &self.session.read(cx).thread.plan;
        if plan.is_empty() {
            return None;
        }
        let mut column = v_flex().gap_1().px_4().py_2().border_t_1().border_color(theme.border);
        for entry in plan {
            let (icon, color) = match entry.status.as_str() {
                "completed" => (IconName::CircleCheck, theme.success),
                "in_progress" => (IconName::LoaderCircle, theme.primary),
                _ => (IconName::Dash, theme.muted_foreground),
            };
            column = column.child(
                h_flex()
                    .gap_2()
                    .text_sm()
                    .child(Icon::new(icon).xsmall().text_color(color))
                    .child(div().truncate().child(entry.content.clone())),
            );
        }
        Some(column.into_any_element())
    }

    fn render_permissions(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = cx.theme().clone();
        let permissions = self.session.read(cx).thread.permissions.clone();
        if permissions.is_empty() {
            return None;
        }
        let mut column = v_flex().gap_2().px_4().py_2();
        for permission in permissions {
            let mut buttons = h_flex().gap_2().flex_wrap();
            for option in permission.options {
                let request = permission.request_id.clone();
                let option_id = option.id.clone();
                let button = Button::new(ElementId::Name(format!("perm-{}-{}", permission.request_id, option.id).into()))
                    .small()
                    .label(option.name.clone())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let (request, option_id) = (request.clone(), option_id.clone());
                        this.session.update(cx, |session, cx| session.answer_permission(request, option_id, cx));
                    }));
                buttons = buttons.child(if option.kind.starts_with("allow") { button.primary() } else { button });
            }
            column = column.child(
                v_flex()
                    .gap_2()
                    .p_3()
                    .rounded(theme.radius)
                    .border_1()
                    .border_color(theme.warning)
                    .bg(theme.warning.opacity(0.08))
                    .child(
                        h_flex()
                            .gap_2()
                            .text_sm()
                            .child(Icon::new(IconName::TriangleAlert).small().text_color(theme.warning))
                            .child(div().flex_1().min_w_0().child(permission.title.clone())),
                    )
                    .child(buttons),
            );
        }
        Some(column.into_any_element())
    }

    fn render_status(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = cx.theme().clone();
        let (text, error, can_retry): (SharedString, bool, bool) = match &self.session.read(cx).phase {
            Phase::Ready => return None,
            Phase::Connecting => ("Connecting to the agent…".into(), false, false),
            Phase::Reconnecting(reason) => (format!("{reason}; connecting again…").into(), false, true),
            Phase::Closed(reason) => (reason.clone().into(), true, true),
        };
        Some(
            h_flex()
                .gap_2()
                .px_4()
                .py_2()
                .text_sm()
                .bg(if error { theme.danger.opacity(0.1) } else { theme.muted })
                .child(if error { Icon::new(IconName::CircleAlert).small().text_color(theme.danger).into_any_element() } else { Spinner::new().small().into_any_element() })
                .child(div().flex_1().min_w_0().child(text))
                .when(can_retry, |row| {
                    row.child(Button::new("retry").xsmall().label("Retry").on_click(cx.listener(|this, _, _, cx| {
                        this.session.update(cx, |session, cx| session.retry(cx));
                    })))
                })
                .into_any_element(),
        )
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let session = self.session.read(cx);
        let mut bar = h_flex().gap_1().px_3().py_1().border_b_1().border_color(theme.border).flex_wrap();
        for mode in &session.thread.modes {
            let selected = session.thread.current_mode.as_deref() == Some(mode.id.as_str());
            let id = mode.id.clone();
            bar = bar.child(
                Button::new(ElementId::Name(format!("mode-{}", mode.id).into()))
                    .xsmall()
                    .ghost()
                    .selected(selected)
                    .label(mode.name.clone())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let id = id.clone();
                        this.session.update(cx, |session, cx| session.set_mode(id, cx));
                    })),
            );
        }
        bar.child(div().flex_1())
            .child(
                Button::new("review-changes")
                    .xsmall()
                    .ghost()
                    .icon(IconName::FileText)
                    .label("Review changes")
                    .tooltip("Annotate the diff and send it to the agent")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(OpenReview))),
            )
            .child(
            Button::new("close-chat")
                .xsmall()
                .ghost()
                .icon(IconName::Close)
                .tooltip("Close conversation")
                .on_click(cx.listener(|_, _, _, cx| cx.emit(Close))),
        )
        .into_any_element()
    }

    fn render_commands(&self, cx: &App) -> Option<AnyElement> {
        let typed = self.composer.read(cx).value();
        let prefix = typed.strip_prefix('/')?;
        if prefix.contains(char::is_whitespace) {
            return None;
        }
        let theme = cx.theme();
        let commands: Vec<_> = self
            .session
            .read(cx)
            .thread
            .commands
            .iter()
            .filter(|command| command.name.starts_with(prefix))
            .take(8)
            .cloned()
            .collect();
        if commands.is_empty() {
            return None;
        }
        let mut column = v_flex().px_4().py_1().gap_0p5().border_t_1().border_color(theme.border);
        for command in commands {
            column = column.child(
                h_flex()
                    .gap_2()
                    .text_sm()
                    .child(div().font_family(theme.mono_font_family.clone()).child(format!("/{}", command.name)))
                    .child(div().truncate().text_color(theme.muted_foreground).child(command.description)),
            );
        }
        Some(column.into_any_element())
    }
}

/// The new text as added lines and the old as removed: enough to see what
/// an edit did, without a diff algorithm of its own yet.
fn diff_view(path: &str, old: Option<&str>, new: &str, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let mut lines = v_flex().font_family(theme.mono_font_family.clone()).text_xs();
    if let Some(old) = old {
        for line in old.lines().take(200) {
            lines = lines.child(div().bg(theme.danger.opacity(0.12)).child(format!("- {line}")));
        }
    }
    for line in new.lines().take(400) {
        lines = lines.child(div().bg(theme.success.opacity(0.12)).child(format!("+ {line}")));
    }
    v_flex()
        .gap_1()
        .child(div().text_xs().text_color(theme.muted_foreground).truncate().child(path.to_owned()))
        .child(lines.p_2().rounded(theme.radius).bg(theme.muted))
        .into_any_element()
}

impl Render for ChatView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let this = cx.entity().downgrade();
        let rows = list(self.list.clone(), move |index, _, cx| {
            this.update(cx, |view, cx| view.render_entry(index, cx)).unwrap_or_else(|_| div().into_any_element())
        })
        .size_full()
        .py_2();

        let (busy, ready, empty) = {
            let session = self.session.read(cx);
            (session.busy, session.is_ready(), session.thread.entries.is_empty())
        };
        let action = if busy {
            Button::new("stop")
                .icon(IconName::Pause)
                .tooltip("Stop")
                .on_click(cx.listener(|this, _, _, cx| this.session.update(cx, |session, cx| session.cancel(cx))))
        } else {
            Button::new("send")
                .primary()
                .icon(IconName::ArrowUp)
                .tooltip("Send")
                .disabled(!ready)
                .on_click(cx.listener(|this, _, window, cx| this.send(window, cx)))
        };
        let bottom = super::insets().1;

        v_flex()
            .size_full()
            .child(self.render_toolbar(cx))
            .children(self.render_status(cx))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .when(empty && ready, |area| {
                        area.child(
                            v_flex()
                                .absolute()
                                .inset_0()
                                .items_center()
                                .justify_center()
                                .text_color(theme.muted_foreground)
                                .child("Ask the agent to do something in this project."),
                        )
                    })
                    .child(div().size_full().child(rows).vertical_scrollbar(&self.list)),
            )
            .children(self.render_plan(cx))
            .children(self.render_permissions(cx))
            .children(self.render_commands(cx))
            .child(
                h_flex()
                    .items_end()
                    .gap_2()
                    .px_3()
                    .pt_2()
                    .pb(px(8.) + bottom)
                    .border_t_1()
                    .border_color(theme.border)
                    .child(div().flex_1().min_w_0().child(Textarea::new(&self.composer)))
                    .child(action),
            )
    }
}
