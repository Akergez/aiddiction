//! A conversation with an agent: the thread, the agent's questions, and the
//! composer with the plan and the agent's mode on it. Tool calls in a row are
//! one line that unfolds, so a long turn reads as what was said, not as a
//! wall of steps. An agent that is not signed in gets a card for signing it
//! in where its questions go.

use std::collections::HashSet;

use aiddiction_api::ProjectId;
use gpui_kit::Focusable as _;
use gpui_kit::base::InteractiveElementExt as _;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::text::TextView;
use gpui_kit::component::{ActiveTheme, Icon, IconName, h_flex, v_flex};
use gpui_kit::component::{Disableable as _, Sizable as _, StyledExt as _};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, AppContext, Context, ElementId, Entity, EventEmitter, FollowMode,
    ListAlignment, ListState, Render, SharedString, Subscription, Window, div, list, px, rems,
};

use crate::login::MethodKind;
use crate::session::{AgentSession, Phase};
use crate::thread::{Entry, ToolCall, ToolContent, ToolStatus};

/// How wide the conversation reads at most.
const READING_WIDTH: f32 = 48.;

/// Asks the shell for the review of this conversation's project.
pub struct OpenReview;

impl EventEmitter<OpenReview> for ChatView {}

pub struct ChatView {
    pub session: Entity<AgentSession>,
    pub project_id: ProjectId,
    composer: Entity<TextareaState>,
    /// The API key a sign-in asks for.
    key_input: Entity<InputState>,
    /// The code a sign-in page gave.
    code_input: Entity<InputState>,
    list: ListState,
    /// Entries whose thoughts or tool output are unfolded (a run of tool
    /// calls by its first).
    open: HashSet<usize>,
    plan_open: bool,
    subscriptions: Vec<Subscription>,
}

impl ChatView {
    pub fn new(
        session: Entity<AgentSession>,
        project_id: ProjectId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 8)
                // Enter sends; Shift+Enter inserts a newline.
                .submit_on_enter(true)
                .placeholder("Ask the agent…")
        });
        super::keyboard_for(&composer, gpui_android_glue::keyboard::Purpose::Message, cx);
        let key_input = cx.new(|cx| InputState::new(window, cx).placeholder("API key"));
        super::keyboard_for(
            &key_input,
            gpui_android_glue::keyboard::Purpose::Password,
            cx,
        );
        let code_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("The code the page gives"));
        super::keyboard_for(&code_input, gpui_android_glue::keyboard::Purpose::Plain, cx);
        let list = ListState::new(0, ListAlignment::Bottom, px(400.));
        list.set_follow_mode(FollowMode::Tail);

        let subscriptions = vec![
            cx.observe(&session, |this, session, cx| {
                let (count, dirty) = session.update(cx, |session, _| {
                    (session.thread.entries.len(), session.thread.take_dirty())
                });
                let shown = this.list.item_count();
                if let Some(from) = dirty.or((count != shown).then_some(shown.min(count))) {
                    // A tool call is drawn by the first of its run.
                    let from = session.read(cx).thread.run_start(from).min(shown);
                    this.list.splice(from..shown, count - from);
                }
                cx.notify();
            }),
            cx.subscribe_in(
                &composer,
                window,
                |this, _, event: &InputEvent, window, cx| {
                    if let InputEvent::PressEnter { shift: false, .. } = event {
                        this.send(window, cx);
                    }
                },
            ),
            cx.subscribe_in(
                &key_input,
                window,
                |this, _, event: &InputEvent, window, cx| {
                    if let InputEvent::PressEnter { .. } = event {
                        this.submit_sign_in(window, cx);
                    }
                },
            ),
            cx.subscribe_in(
                &code_input,
                window,
                |this, _, event: &InputEvent, window, cx| {
                    if let InputEvent::PressEnter { .. } = event {
                        this.submit_sign_in(window, cx);
                    }
                },
            ),
        ];
        Self {
            session,
            project_id,
            composer,
            key_input,
            code_input,
            list,
            open: HashSet::new(),
            plan_open: false,
            subscriptions,
        }
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
        self.session
            .update(cx, |session, cx| session.prompt(text, cx));
        self.composer
            .update(cx, |composer, cx| composer.set_value("", window, cx));
        self.list.scroll_to_end();
    }

    /// The key or the code typed into the sign-in card.
    fn submit_sign_in(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let kind = self
            .session
            .read(cx)
            .sign_in
            .method
            .as_ref()
            .map(|method| method.kind.clone());
        let input = match kind {
            Some(MethodKind::ApiKey) => self.key_input.clone(),
            Some(MethodKind::Terminal) => self.code_input.clone(),
            _ => return,
        };
        let text = input.read(cx).value().to_string();
        if text.trim().is_empty() {
            return;
        }
        self.session.update(cx, |session, cx| match kind {
            Some(MethodKind::ApiKey) => session.submit_key(&text, cx),
            _ => session.type_into_sign_in(&text, cx),
        });
        input.update(cx, |input, cx| input.set_value("", window, cx));
    }

    fn toggle(&mut self, index: usize, cx: &mut Context<Self>) {
        if !self.open.remove(&index) {
            self.open.insert(index);
        }
        let row = self.session.read(cx).thread.run_start(index);
        self.list.remeasure_items(row..row + 1);
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
                        .px_4()
                        .py_2p5()
                        .rounded(px(18.))
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
                                    .child(
                                        Icon::new(if open {
                                            IconName::ChevronDown
                                        } else {
                                            IconName::ChevronRight
                                        })
                                        .xsmall(),
                                    )
                                    .child("Thinking"),
                            )
                            .when(open, |this| this.child(div().pl_4().child(thought))),
                    );
                }
                if !text.is_empty() {
                    column = column.child(
                        TextView::markdown(
                            ElementId::Name(format!("message-{index}").into()),
                            text,
                        )
                        .selectable(true)
                        .on_link_click(|url, _, _, cx| super::open_url(url, cx)),
                    );
                }
                column.into_any_element()
            }
            Entry::Tool(_) => {
                let thread = &self.session.read(cx).thread;
                if thread.run_start(index) != index {
                    // Drawn by the first of its run.
                    return div().into_any_element();
                }
                let run: Vec<(usize, ToolCall)> = thread
                    .run(index)
                    .into_iter()
                    .cloned()
                    .enumerate()
                    .map(|(at, call)| (index + at, call))
                    .collect();
                self.render_run(index, run, open, cx)
            }
            Entry::Notice { text, error } => div()
                .w_full()
                .text_sm()
                .text_center()
                .text_color(if error {
                    theme.danger
                } else {
                    theme.muted_foreground
                })
                .child(text)
                .into_any_element(),
        };
        h_flex()
            .w_full()
            .justify_center()
            .child(
                div()
                    .w_full()
                    .max_w(rems(READING_WIDTH))
                    .px_4()
                    .py_1p5()
                    .child(body),
            )
            .into_any_element()
    }

    /// A run of tool calls: one line that sums them up and unfolds into
    /// them. A single call is drawn as itself.
    fn render_run(
        &self,
        start: usize,
        run: Vec<(usize, ToolCall)>,
        open: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if run.len() == 1 {
            let (index, call) = run.into_iter().next().unwrap();
            return self.render_tool(index, call, open, cx);
        }
        let theme = cx.theme().clone();
        let summary =
            crate::thread::summarize(&run.iter().map(|(_, call)| call).collect::<Vec<_>>());
        let mut card = v_flex()
            .border_1()
            .border_color(theme.border)
            .rounded(theme.radius * 1.5)
            .child(
                h_flex()
                    .id(ElementId::Name(format!("run-{start}").into()))
                    .gap_2()
                    .px_3()
                    .py_2()
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle(start, cx)))
                    .child(status_icon(summary.status, cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .child(summary.text),
                    )
                    .child(
                        Icon::new(if open {
                            IconName::ChevronUp
                        } else {
                            IconName::ChevronDown
                        })
                        .xsmall()
                        .text_color(theme.muted_foreground),
                    ),
            );
        if open {
            let mut steps = v_flex().gap_1p5().px_2().pb_2();
            for (index, call) in run {
                let unfolded = self.open.contains(&index);
                steps = steps.child(self.render_tool(index, call, unfolded, cx));
            }
            card = card.child(steps);
        }
        card.into_any_element()
    }

    fn render_tool(
        &self,
        index: usize,
        call: ToolCall,
        open: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let status = status_icon(call.status, cx);
        let icon = match call.kind.as_str() {
            "edit" | "delete" | "move" => IconName::FileText,
            "execute" => IconName::SquareTerminal,
            "search" => IconName::Search,
            "fetch" => IconName::Globe,
            "think" => IconName::Bot,
            _ => IconName::Settings2,
        };
        let title = if call.title.is_empty() {
            call.kind.clone()
        } else {
            call.title.clone()
        };
        let has_content = !call.content.is_empty() || !call.locations.is_empty();
        let mut card = v_flex()
            .border_1()
            .border_color(theme.border)
            .rounded(theme.radius * 1.5)
            .child(
                h_flex()
                    .id(ElementId::Name(format!("tool-{index}").into()))
                    .gap_2()
                    .px_3()
                    .py_2()
                    .when(has_content, |row| {
                        row.cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| this.toggle(index, cx)))
                    })
                    .child(Icon::new(icon).small().text_color(theme.muted_foreground))
                    .child(div().flex_1().min_w_0().text_sm().truncate().child(title))
                    .child(status),
            );
        if open {
            let mut content = v_flex().gap_2().px_3().pb_3();
            for path in &call.locations {
                content = content.child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .truncate()
                        .child(path.clone()),
                );
            }
            for item in call.content {
                content = content.child(match item {
                    ToolContent::Text(text) => div().text_sm().child(text).into_any_element(),
                    ToolContent::Diff { path, old, new } => {
                        diff_view(&path, old.as_deref(), &new, cx)
                    }
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

    /// The plan on the composer: one line with how far along it is, the
    /// whole list when unfolded.
    fn render_plan(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = cx.theme().clone();
        let plan = self.session.read(cx).thread.plan.clone();
        if plan.is_empty() {
            return None;
        }
        let done = plan
            .iter()
            .filter(|entry| entry.status == "completed")
            .count();
        let current = plan
            .iter()
            .find(|entry| entry.status == "in_progress")
            .or_else(|| plan.iter().find(|entry| entry.status != "completed"));
        let progress = done as f32 / plan.len() as f32;
        let line =
            h_flex()
                .id("plan")
                .gap_2()
                .px_3()
                .py_2()
                .cursor_pointer()
                .text_sm()
                .border_b_1()
                .border_color(theme.border)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.plan_open = !this.plan_open;
                    cx.notify();
                }))
                .child(Icon::new(IconName::CircleCheck).xsmall().text_color(
                    if done == plan.len() {
                        theme.success
                    } else {
                        theme.primary
                    },
                ))
                .child(
                    div()
                        .flex_none()
                        .font_semibold()
                        .child(format!("Plan {done}/{}", plan.len())),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(theme.muted_foreground)
                        .child(
                            current
                                .map(|entry| entry.content.clone())
                                .unwrap_or_default(),
                        ),
                )
                .child(
                    div()
                        .w_16()
                        .h_1()
                        .flex_none()
                        .rounded_full()
                        .bg(theme.muted)
                        .child(
                            div()
                                .h_full()
                                .rounded_full()
                                .bg(theme.primary)
                                .w(gpui_kit::relative(progress)),
                        ),
                )
                .child(
                    Icon::new(if self.plan_open {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronUp
                    })
                    .xsmall()
                    .text_color(theme.muted_foreground),
                );
        if !self.plan_open {
            return Some(line.into_any_element());
        }
        let mut column = v_flex()
            .gap_1()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(theme.border);
        for entry in &plan {
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
        Some(v_flex().child(line).child(column).into_any_element())
    }

    fn render_permissions(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = cx.theme().clone();
        let permissions = self.session.read(cx).thread.permissions.clone();
        if permissions.is_empty() {
            return None;
        }
        let mut column = v_flex().gap_2();
        for permission in permissions {
            let mut buttons = h_flex().gap_2().flex_wrap().justify_end();
            for option in permission.options {
                let request = permission.request_id.clone();
                let option_id = option.id.clone();
                let button = Button::new(ElementId::Name(
                    format!("perm-{}-{}", permission.request_id, option.id).into(),
                ))
                .label(option.name.clone())
                .on_click(cx.listener(move |this, _, _, cx| {
                    let (request, option_id) = (request.clone(), option_id.clone());
                    this.session.update(cx, |session, cx| {
                        session.answer_permission(request, option_id, cx)
                    });
                }));
                buttons = buttons.child(if option.kind.starts_with("allow") {
                    button.primary()
                } else {
                    button.outline()
                });
            }
            column = column.child(
                v_flex()
                    .gap_3()
                    .p_3()
                    .rounded(theme.radius * 2.)
                    .border_1()
                    .border_color(theme.warning)
                    .bg(theme.warning.opacity(0.08))
                    .child(
                        h_flex()
                            .gap_2()
                            .font_semibold()
                            .child(
                                Icon::new(IconName::TriangleAlert)
                                    .small()
                                    .text_color(theme.warning),
                            )
                            .child(div().flex_1().min_w_0().child(permission.title.clone())),
                    )
                    .child(buttons),
            );
        }
        Some(column.into_any_element())
    }

    /// Signing the agent in: its ways, then whatever the one picked needs —
    /// a key, a page to open, what the sign-in on the server says and the
    /// code it asks for.
    fn render_sign_in(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = cx.theme().clone();
        let session = self.session.read(cx);
        let sign_in = &session.sign_in;
        if !sign_in.shown(&session.phase) {
            return None;
        }
        let mut card = v_flex()
            .gap_3()
            .p_3()
            .rounded(theme.radius * 2.)
            .border_1()
            .border_color(theme.primary)
            .bg(theme.primary.opacity(0.06))
            .child(
                h_flex()
                    .gap_2()
                    .font_semibold()
                    .child(Icon::new(IconName::User).small().text_color(theme.primary))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(format!("Sign in to {}", session.agent_name)),
                    ),
            );
        if session.has_queued() {
            card = card.child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("Your message goes once it is signed in."),
            );
        }
        if let Some(error) = &sign_in.error {
            card = card.child(
                div()
                    .text_sm()
                    .text_color(theme.danger)
                    .child(error.clone()),
            );
        }
        let cancel = Button::new("sign-in-cancel")
            .small()
            .outline()
            .label("Cancel")
            .on_click(cx.listener(|this, _, _, cx| {
                this.session
                    .update(cx, |session, cx| session.cancel_sign_in(cx))
            }));
        let input_row = |state: &Entity<InputState>, masked: bool, cx: &mut Context<Self>| {
            let mut input = Input::new(state);
            if masked {
                input = input.mask_toggle();
            }
            h_flex()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(gpui_android_glue::keyboard::on_tap(input, state, cx)),
                )
                .child(
                    Button::new("sign-in-submit")
                        .small()
                        .primary()
                        .label("Send")
                        .on_click(
                            cx.listener(|this, _, window, cx| this.submit_sign_in(window, cx)),
                        ),
                )
        };

        let Some(method) = sign_in.method.clone() else {
            if sign_in.methods.is_empty() {
                return Some(
                    card.child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child("The agent offers no way to sign in from here. Sign it in on the server, then retry."),
                    )
                    .into_any_element(),
                );
            }
            let mut buttons = h_flex().gap_2().flex_wrap();
            for (at, method) in sign_in.methods.iter().enumerate() {
                let chosen = method.clone();
                let mut button = Button::new(ElementId::Name(format!("sign-in-{at}").into()))
                    .small()
                    .label(method.name.clone())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let chosen = chosen.clone();
                        this.session
                            .update(cx, |session, cx| session.sign_in_with(chosen, cx));
                    }));
                button = if at == 0 {
                    button.primary()
                } else {
                    button.outline()
                };
                if !method.description.is_empty() {
                    button = button.tooltip(method.description.clone());
                }
                buttons = buttons.child(button);
            }
            return Some(card.child(buttons).into_any_element());
        };

        let busy = sign_in.busy;
        let page = sign_in.page.clone();
        let terminal = sign_in.terminal.clone();
        let waiting = |text: String| {
            h_flex()
                .gap_2()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(Spinner::new().small())
                .child(text)
        };
        card = match method.kind {
            MethodKind::ApiKey => card
                .child(input_row(&self.key_input, true, cx))
                .when(busy, |card| card.child(waiting("Checking the key…".into()))),
            MethodKind::Agent => match page {
                Some(page) => card
                    .child(div().text_sm().child(page.message.clone()))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .truncate()
                            .child(page.url.clone()),
                    )
                    .child(
                        h_flex().gap_2().child(
                            Button::new("sign-in-open")
                                .small()
                                .primary()
                                .icon(IconName::ExternalLink)
                                .label("Open the page")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    if let Some(url) = this
                                        .session
                                        .update(cx, |session, _| session.open_sign_in_page())
                                    {
                                        super::open_url(&url, cx);
                                    }
                                })),
                        ),
                    ),
                None => card.child(waiting(format!("Waiting for {}…", method.name))),
            },
            MethodKind::Terminal => match terminal {
                None => card.child(waiting("Starting the sign-in on the server…".into())),
                Some(login) => {
                    let url = crate::login::first_url(&login.output).map(str::to_owned);
                    card.child(
                        div()
                            .id("sign-in-output")
                            .max_h(rems(10.))
                            .overflow_y_scroll()
                            .lock_scroll_axis()
                            .p_2()
                            .rounded(theme.radius)
                            .bg(theme.muted)
                            .font_family(theme.mono_font_family.clone())
                            .text_xs()
                            .child(login.output.trim().to_owned()),
                    )
                    .when_some(url, |card, url| {
                        card.child(
                            h_flex().child(
                                Button::new("sign-in-open")
                                    .small()
                                    .primary()
                                    .icon(IconName::ExternalLink)
                                    .label("Open the link")
                                    .on_click(move |_, _, cx| super::open_url(&url, cx)),
                            ),
                        )
                    })
                    .child(input_row(&self.code_input, false, cx))
                }
            },
        };
        Some(
            card.child(h_flex().justify_end().child(cancel))
                .into_any_element(),
        )
    }

    fn render_status(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = cx.theme().clone();
        let (text, error, can_retry): (SharedString, bool, bool) =
            match &self.session.read(cx).phase {
                Phase::Ready => return None,
                Phase::Connecting => ("Connecting to the agent…".into(), false, false),
                Phase::Reconnecting(reason) => {
                    (format!("{reason}; connecting again…").into(), false, true)
                }
                // The sign-in card says it.
                Phase::SignIn => return None,
                Phase::Closed(reason) => (reason.clone().into(), true, true),
            };
        Some(
            h_flex()
                .gap_2()
                .px_4()
                .py_2()
                .text_sm()
                .bg(if error {
                    theme.danger.opacity(0.1)
                } else {
                    theme.muted
                })
                .child(if error {
                    Icon::new(IconName::CircleAlert)
                        .small()
                        .text_color(theme.danger)
                        .into_any_element()
                } else {
                    Spinner::new().small().into_any_element()
                })
                .child(div().flex_1().min_w_0().child(text))
                .when(can_retry, |row| {
                    row.child(
                        Button::new("retry")
                            .xsmall()
                            .label("Retry")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.session.update(cx, |session, cx| session.retry(cx));
                            })),
                    )
                })
                .into_any_element(),
        )
    }

    /// The agent's mode, picked from a menu on the composer.
    fn render_mode(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let thread = &self.session.read(cx).thread;
        if thread.modes.is_empty() {
            return None;
        }
        let current = thread.current_mode.clone();
        let name = thread
            .modes
            .iter()
            .find(|mode| Some(&mode.id) == current.as_ref())
            .map(|mode| mode.name.clone())
            .unwrap_or_else(|| "Mode".into());
        let modes = thread.modes.clone();
        let session = self.session.clone();
        Some(
            Button::new("mode")
                .ghost()
                .xsmall()
                .icon(IconName::Bot)
                .label(name)
                .dropdown_caret(true)
                .dropdown_menu(move |mut menu, _, _| {
                    for mode in &modes {
                        let id = mode.id.clone();
                        let session = session.clone();
                        menu = menu.item(
                            PopupMenuItem::new(mode.name.clone())
                                .checked(current.as_ref() == Some(&mode.id))
                                .on_click(move |_, _, cx| {
                                    session
                                        .update(cx, |session, cx| session.set_mode(id.clone(), cx))
                                }),
                        );
                    }
                    menu
                })
                .into_any_element(),
        )
    }

    fn render_config_options(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let options = self.session.read(cx).thread.config_options.clone();
        let option = options.into_iter().find(|option| {
            matches!(
                option.category.as_str(),
                "model" | "model_config" | "thought_level"
            ) && !option.values.is_empty()
        })?;
        let current = option
            .values
            .iter()
            .find(|(id, _)| id == &option.current)
            .map(|(_, name)| name.clone())
            .unwrap_or_else(|| option.current.clone());
        let id = option.id.clone();
        let session = self.session.clone();
        Some(
            Button::new(ElementId::Name(format!("config-{}", option.id).into()))
                .ghost()
                .xsmall()
                .icon(IconName::Settings2)
                .label(current)
                .dropdown_caret(true)
                .dropdown_menu(move |mut menu, _, _| {
                    for (value, name) in &option.values {
                        let value = value.clone();
                        let session = session.clone();
                        let id = id.clone();
                        menu = menu.item(PopupMenuItem::new(name.clone()).on_click(
                            move |_, _, cx| {
                                session.update(cx, |session, cx| {
                                    session.set_config_option(id.clone(), value.clone(), cx)
                                })
                            },
                        ));
                    }
                    menu
                })
                .into_any_element(),
        )
    }

    fn render_commands(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
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
        let mut column = v_flex()
            .px_3()
            .py_2()
            .gap_0p5()
            .border_b_1()
            .border_color(theme.border);
        for command in commands {
            let value = format!("/{} ", command.name);
            let composer = self.composer.clone();
            column = column.child(
                h_flex()
                    .id(ElementId::Name(format!("command-{}", command.name).into()))
                    .cursor_pointer()
                    .on_click(cx.listener(move |_, _, window, cx| {
                        composer.update(cx, |input, cx| input.set_value(value.clone(), window, cx));
                        composer.read(cx).focus_handle(cx).focus(window, cx);
                    }))
                    .gap_2()
                    .text_sm()
                    .child(
                        div()
                            .font_family(theme.mono_font_family.clone())
                            .child(format!("/{}", command.name)),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_color(theme.muted_foreground)
                            .child(command.description),
                    ),
            );
        }
        Some(column.into_any_element())
    }
}

/// The new text as added lines and the old as removed: enough to see what
/// an edit did, without a diff algorithm of its own yet.
fn diff_view(path: &str, old: Option<&str>, new: &str, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let mut lines = v_flex()
        .font_family(theme.mono_font_family.clone())
        .text_xs();
    if let Some(old) = old {
        for line in old.lines().take(200) {
            lines = lines.child(
                div()
                    .bg(theme.danger.opacity(0.12))
                    .child(format!("- {line}")),
            );
        }
    }
    for line in new.lines().take(400) {
        lines = lines.child(
            div()
                .bg(theme.success.opacity(0.12))
                .child(format!("+ {line}")),
        );
    }
    v_flex()
        .gap_1()
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .truncate()
                .child(path.to_owned()),
        )
        .child(lines.p_2().rounded(theme.radius).bg(theme.muted))
        .into_any_element()
}

fn status_icon(status: ToolStatus, cx: &App) -> AnyElement {
    let theme = cx.theme();
    match status {
        ToolStatus::Pending => Icon::new(IconName::Ellipsis)
            .small()
            .text_color(theme.muted_foreground)
            .into_any_element(),
        ToolStatus::InProgress => Spinner::new().small().into_any_element(),
        ToolStatus::Completed => Icon::new(IconName::CircleCheck)
            .small()
            .text_color(theme.success)
            .into_any_element(),
        ToolStatus::Failed => Icon::new(IconName::CircleX)
            .small()
            .text_color(theme.danger)
            .into_any_element(),
    }
}

impl Render for ChatView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let phone = super::is_phone(window);
        let this = cx.entity().downgrade();
        let rows = list(self.list.clone(), move |index, _, cx| {
            this.update(cx, |view, cx| view.render_entry(index, cx))
                .unwrap_or_else(|_| div().into_any_element())
        })
        .size_full()
        .py_3();

        let (busy, ready, empty, agent, project) = {
            let session = self.session.read(cx);
            (
                session.busy,
                session.is_ready(),
                session.thread.entries.is_empty(),
                session.agent_name.clone(),
                session.project_name.clone(),
            )
        };
        let action = if busy {
            Button::new("stop")
                .small()
                .icon(IconName::Pause)
                .tooltip("Stop the agent's turn")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.session.update(cx, |session, cx| session.cancel(cx))
                }))
        } else {
            Button::new("send")
                .primary()
                .small()
                .icon(IconName::ArrowUp)
                .tooltip("Send (Enter)")
                .disabled(!ready)
                .on_click(cx.listener(|this, _, window, cx| this.send(window, cx)))
        };
        // The shell lifts the page onto the keyboard; this is the bar's room.
        let bottom = super::insets().1;

        let composer = v_flex()
            .w_full()
            .rounded(px(18.))
            .border_1()
            .border_color(theme.border)
            .bg(theme.background)
            .overflow_hidden()
            .children(self.render_plan(cx))
            .children(self.render_commands(cx))
            .child(
                div()
                    .px_1()
                    .pt_1()
                    .child(gpui_android_glue::keyboard::on_tap(
                        Textarea::new(&self.composer).appearance(false),
                        &self.composer,
                        cx,
                    )),
            )
            .child(
                h_flex()
                    .gap_1()
                    .px_2()
                    .pb_2()
                    .children(self.render_mode(cx))
                    .children(self.render_config_options(cx))
                    .when(!phone, |bar| {
                        bar.child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child("/ for commands · Shift+Enter for a new line"),
                        )
                    })
                    .child(div().flex_1())
                    .child(action),
            );

        v_flex()
            .size_full()
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
                                .gap_3()
                                .px_6()
                                .text_center()
                                .child(
                                    div()
                                        .size(gpui_kit::rems(3.5))
                                        .rounded_full()
                                        .bg(theme.muted)
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .child(
                                            Icon::new(IconName::Bot)
                                                .large()
                                                .text_color(theme.muted_foreground),
                                        ),
                                )
                                .child(div().font_semibold().child(format!("{agent} in {project}")))
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(theme.muted_foreground)
                                        .child("Ask it to do something in this project."),
                                ),
                        )
                    })
                    .child(div().size_full().child(rows).vertical_scrollbar(&self.list)),
            )
            .child(
                h_flex().w_full().justify_center().child(
                    v_flex()
                        .w_full()
                        .max_w(rems(READING_WIDTH))
                        .gap_2()
                        .px_3()
                        .pt_1()
                        .pb(px(10.) + bottom)
                        .children(self.render_sign_in(cx))
                        .children(self.render_permissions(cx))
                        .child(composer),
                ),
            )
    }
}
