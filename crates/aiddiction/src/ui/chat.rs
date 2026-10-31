//! A conversation with an agent: the thread, the agent's questions, and the
//! composer with the plan and the agent's mode on it. Tool calls in a row are
//! one line that unfolds, so a long turn reads as what was said, not as a
//! wall of steps. An agent that is not signed in gets a card for signing it
//! in where its questions go.

use std::collections::HashSet;

use aiddiction_api::ProjectId;
use gpui_adaptive::prim::{column, spinner};
use gpui_adaptive::{
    ActiveScale, Ask, Banner, Busy, Code, Command, Composer, ComposerEvent, ComposerState, Empty, Feed, FieldEvent, Fold,
    IconName, Message, Note, Purpose, Suggestion, TextField, TextFieldState, Tone,
};
use gpui_kit::Focusable as _;
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, AppContext, Context, ElementId, Entity, EventEmitter, ListState, Render, SharedString, Subscription,
    Window, div, px, rems,
};

use crate::login::MethodKind;
use crate::session::{AgentSession, Phase};
use crate::thread::{Entry, ToolCall, ToolContent, ToolStatus};

use super::work::{Diff, Plan, Step, StepStatus};

/// Asks the shell for the review of this conversation's project.
pub struct OpenReview;

impl EventEmitter<OpenReview> for ChatView {}

pub struct ChatView {
    pub session: Entity<AgentSession>,
    pub project_id: ProjectId,
    composer: Entity<ComposerState>,
    /// The API key a sign-in asks for.
    key_input: Entity<TextFieldState>,
    /// The code a sign-in page gave.
    code_input: Entity<TextFieldState>,
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
        let composer = cx.new(|cx| ComposerState::new("Ask the agent…", window, cx));
        let key_input = cx.new(|cx| {
            let mut field = TextFieldState::new(Purpose::Password, window, cx);
            field.set_placeholder("API key", window, cx);
            field
        });
        let code_input = cx.new(|cx| {
            let mut field = TextFieldState::new(Purpose::Plain, window, cx);
            field.set_placeholder("The code the page gives", window, cx);
            field
        });
        let list = Feed::state();

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
            cx.subscribe_in(&composer, window, |this, _, event: &ComposerEvent, window, cx| match event {
                ComposerEvent::Send => this.send(window, cx),
                // The commands matching what is typed stand over the field.
                ComposerEvent::Change => cx.notify(),
            }),
            cx.subscribe_in(&key_input, window, |this, _, event: &FieldEvent, window, cx| {
                if *event == FieldEvent::Submit {
                    this.submit_sign_in(window, cx);
                }
            }),
            cx.subscribe_in(&code_input, window, |this, _, event: &FieldEvent, window, cx| {
                if *event == FieldEvent::Submit {
                    this.submit_sign_in(window, cx);
                }
            }),
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
        let text = self.composer.read(cx).text(cx);
        let (ready, busy) = {
            let session = self.session.read(cx);
            (session.is_ready(), session.busy)
        };
        if text.trim().is_empty() || !ready || busy {
            return;
        }
        self.session
            .update(cx, |session, cx| session.prompt(text, cx));
        self.composer.update(cx, |composer, cx| composer.clear(window, cx));
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
        let text = input.read(cx).value(cx).to_string();
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
        let entry = self.session.read(cx).thread.entries.get(index).cloned();
        let Some(entry) = entry else {
            return div().into_any_element();
        };
        let open = self.open.contains(&index);
        let id = |kind: &str| SharedString::from(format!("{kind}-{index}"));
        match entry {
            Entry::User { text } => Message::mine(id("message"), text).into_any_element(),
            Entry::Assistant { text, thought } => {
                let thought = (!thought.is_empty()).then(|| {
                    let this = cx.entity().downgrade();
                    Fold::new(id("thought"), "Thinking").open(open).child(thought).on_toggle(move |_, cx| {
                        this.update(cx, |this, cx| this.toggle(index, cx)).ok();
                    })
                });
                Message::theirs(id("message"), text)
                    .before(thought)
                    .on_link(|url, _, cx| super::open_url(url, cx))
                    .into_any_element()
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
            Entry::Notice { text, error } => {
                let notice = Message::aside(id("notice"), text);
                if error { notice.tone(Tone::Danger) } else { notice }.into_any_element()
            }
        }
    }

    /// What pressing a step does: opens or closes it.
    fn toggler(&self, index: usize, cx: &mut Context<Self>) -> impl Fn(&mut Window, &mut App) + 'static {
        let this = cx.entity().downgrade();
        move |_, cx| {
            this.update(cx, |this, cx| this.toggle(index, cx)).ok();
        }
    }

    /// A run of tool calls: one line that sums them up and unfolds into
    /// them. A single call is drawn as itself.
    fn render_run(&self, start: usize, run: Vec<(usize, ToolCall)>, open: bool, cx: &mut Context<Self>) -> AnyElement {
        if run.len() == 1 {
            let (index, call) = run.into_iter().next().unwrap();
            return self.render_tool(index, call, open, cx).into_any_element();
        }
        let summary = crate::thread::summarize(&run.iter().map(|(_, call)| call).collect::<Vec<_>>());
        let mut step = Step::new(SharedString::from(format!("run-{start}")), summary.text, step_status(summary.status))
            .open(open)
            .on_toggle(self.toggler(start, cx));
        if open {
            for (index, call) in run {
                let unfolded = self.open.contains(&index);
                step = step.child(self.render_tool(index, call, unfolded, cx));
            }
        }
        step.into_any_element()
    }

    fn render_tool(&self, index: usize, call: ToolCall, open: bool, cx: &mut Context<Self>) -> Step {
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
        let mut step = Step::new(SharedString::from(format!("tool-{index}")), title, step_status(call.status)).icon(icon).open(open);
        if has_content {
            step = step.on_toggle(self.toggler(index, cx));
        }
        if !open {
            return step;
        }
        for path in &call.locations {
            step = step.child(Note::new(path.clone()).truncate());
        }
        for item in call.content {
            step = match item {
                ToolContent::Text(text) => step.child(gpui_adaptive::Text::small(text)),
                ToolContent::Diff { path, old, new } => step.child(Diff::new(path, old, new)),
                ToolContent::Terminal(id) => step.child(Note::new(format!("Terminal {id}"))),
            };
        }
        step
    }

    /// The plan on the composer: one line with how far along it is, the
    /// whole list when unfolded.
    fn render_plan(&self, cx: &mut Context<Self>) -> Option<Plan> {
        let plan = self.session.read(cx).thread.plan.clone();
        if plan.is_empty() {
            return None;
        }
        let this = cx.entity().downgrade();
        let shown = Plan::new(self.plan_open, move |_, cx| {
            this.update(cx, |this, cx| {
                this.plan_open = !this.plan_open;
                cx.notify();
            })
            .ok();
        });
        Some(plan.iter().fold(shown, |shown, entry| {
            let status = match entry.status.as_str() {
                "completed" => StepStatus::Done,
                "in_progress" => StepStatus::Running,
                _ => StepStatus::Pending,
            };
            shown.item(entry.content.clone(), status)
        }))
    }

    /// The agent's questions, each with its answers.
    fn render_permissions(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let permissions = self.session.read(cx).thread.permissions.clone();
        if permissions.is_empty() {
            return None;
        }
        let mut asks = Vec::new();
        for permission in permissions {
            let mut ask = Ask::new(SharedString::from(format!("perm-{}", permission.request_id)), permission.title.clone());
            for option in permission.options {
                let (request, option_id, session) = (permission.request_id.clone(), option.id.clone(), self.session.clone());
                let answer = Command::new(SharedString::from(format!("perm-{}-{}", permission.request_id, option.id)), option.name.clone())
                    .on_trigger(move |_, cx| {
                        let (request, option_id) = (request.clone(), option_id.clone());
                        session.update(cx, |session, cx| session.answer_permission(request, option_id, cx));
                    });
                ask = ask.answer(if option.kind.starts_with("allow") { answer.primary() } else { answer });
            }
            asks.push(ask);
        }
        Some(column().gap(cx.scale().space_s).children(asks).into_any_element())
    }

    /// Signing the agent in: its ways, then whatever the one picked needs —
    /// a key, a page to open, what the sign-in on the server says and the
    /// code it asks for.
    fn render_sign_in(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let space = cx.scale().space_s;
        let session = self.session.read(cx);
        let sign_in = &session.sign_in;
        if !sign_in.shown(&session.phase) {
            return None;
        }
        let mut ask = Ask::new("sign-in", format!("Sign in to {}", session.agent_name)).tone(Tone::Info);
        // What the card says under its question: what went wrong, else
        // that the message waits.
        if let Some(error) = &sign_in.error {
            ask = ask.detail(error.clone());
        } else if session.has_queued() {
            ask = ask.detail("Your message goes once it is signed in.");
        }
        let this = cx.entity().downgrade();
        let cancel = Command::new("sign-in-cancel", "Cancel").on_trigger({
            let this = this.clone();
            move |_, cx| {
                this.update(cx, |this, cx| this.session.update(cx, |session, cx| session.cancel_sign_in(cx))).ok();
            }
        });
        let send = Command::new("sign-in-submit", "Send").primary().on_trigger({
            let this = this.clone();
            move |window, cx| {
                this.update(cx, |this, cx| this.submit_sign_in(window, cx)).ok();
            }
        });

        let Some(method) = sign_in.method.clone() else {
            if sign_in.methods.is_empty() {
                return Some(
                    ask.detail("The agent offers no way to sign in from here. Sign it in on the server, then retry.")
                        .into_any_element(),
                );
            }
            for (at, method) in sign_in.methods.iter().enumerate() {
                let (chosen, session) = (method.clone(), self.session.clone());
                let way = Command::new(SharedString::from(format!("sign-in-{at}")), method.name.clone())
                    .on_trigger(move |_, cx| session.update(cx, |session, cx| session.sign_in_with(chosen.clone(), cx)));
                ask = ask.answer(if at == 0 { way.primary() } else { way });
            }
            return Some(ask.into_any_element());
        };

        let waiting = |text: String| Busy::new("sign-in-busy").label(text);
        let ask = match method.kind {
            MethodKind::ApiKey => ask
                .body(
                    column()
                        .gap(space)
                        .child(TextField::new(&self.key_input))
                        .when(sign_in.busy, |body| body.child(waiting("Checking the key…".into()))),
                )
                .answer(cancel)
                .answer(send),
            MethodKind::Agent => match sign_in.page.clone() {
                Some(page) => {
                    let session = self.session.clone();
                    ask.body(
                        column()
                            .gap(space)
                            .child(gpui_adaptive::Text::small(page.message.clone()))
                            .child(Note::new(page.url.clone()).truncate()),
                    )
                    .answer(cancel)
                    .answer(Command::new("sign-in-open", "Open the page").icon(IconName::ExternalLink).primary().on_trigger(
                        move |_, cx| {
                            if let Some(url) = session.update(cx, |session, _| session.open_sign_in_page()) {
                                super::open_url(&url, cx);
                            }
                        },
                    ))
                }
                None => ask.body(waiting(format!("Waiting for {}…", method.name))).answer(cancel),
            },
            MethodKind::Terminal => match sign_in.terminal.clone() {
                None => ask.body(waiting("Starting the sign-in on the server…".into())).answer(cancel),
                Some(login) => {
                    let url = crate::login::first_url(&login.output).map(str::to_owned);
                    let ask = ask.body(
                        column()
                            .gap(space)
                            .child(Code::new("sign-in-output", login.output.trim().to_owned()).max_height(rems(10.)))
                            .child(TextField::new(&self.code_input)),
                    );
                    let ask = ask.answer(cancel);
                    let ask = match url {
                        Some(url) => ask.answer(
                            Command::new("sign-in-open", "Open the link")
                                .icon(IconName::ExternalLink)
                                .on_trigger(move |_, cx| super::open_url(&url, cx)),
                        ),
                        None => ask,
                    };
                    ask.answer(send)
                }
            },
        };
        Some(ask.into_any_element())
    }

    /// The connection to the agent, while it is not simply there.
    fn render_status(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (text, error, can_retry): (SharedString, bool, bool) = match &self.session.read(cx).phase {
            Phase::Ready => return None,
            Phase::Connecting => ("Connecting to the agent…".into(), false, false),
            Phase::Reconnecting(reason) => (format!("{reason}; connecting again…").into(), false, true),
            // The sign-in card says it.
            Phase::SignIn => return None,
            Phase::Closed(reason) => (reason.clone().into(), true, true),
        };
        let session = self.session.clone();
        let banner = Banner::new("status", text)
            .tone(if error { Tone::Danger } else { Tone::Info })
            .when(error, |banner| banner.icon(IconName::CircleAlert))
            .when(!error, |banner| banner.extra(spinner()))
            .when(can_retry, |banner| {
                banner.action(
                    Command::new("retry", "Retry").on_trigger(move |_, cx| session.update(cx, |session, cx| session.retry(cx))),
                )
            });
        Some(banner.into_any_element())
    }

    /// The agent's mode, picked from a menu on the composer.
    fn mode_menu(&self, composer: Composer, cx: &App) -> Composer {
        let thread = &self.session.read(cx).thread;
        if thread.modes.is_empty() {
            return composer;
        }
        let current = thread.current_mode.clone();
        let name = thread
            .modes
            .iter()
            .find(|mode| Some(&mode.id) == current.as_ref())
            .map(|mode| mode.name.clone())
            .unwrap_or_else(|| "Mode".into());
        let options = thread
            .modes
            .iter()
            .map(|mode| {
                let (id, session) = (mode.id.clone(), self.session.clone());
                Command::new(SharedString::from(format!("mode-{}", mode.id)), mode.name.clone())
                    .on_trigger(move |_, cx| session.update(cx, |session, cx| session.set_mode(id.clone(), cx)))
            })
            .collect();
        let chosen = current.map(|id| ElementId::Name(format!("mode-{id}").into()));
        composer.menu(Command::new("mode", name).icon(IconName::Bot), options, chosen)
    }

    /// The model or how hard it thinks, picked from a menu on the composer.
    fn config_menu(&self, composer: Composer, cx: &App) -> Composer {
        let options = self.session.read(cx).thread.config_options.clone();
        let Some(option) = options.into_iter().find(|option| {
            matches!(option.category.as_str(), "model" | "model_config" | "thought_level") && !option.values.is_empty()
        }) else {
            return composer;
        };
        let current = option
            .values
            .iter()
            .find(|(id, _)| id == &option.current)
            .map(|(_, name)| name.clone())
            .unwrap_or_else(|| option.current.clone());
        let values = option
            .values
            .iter()
            .map(|(value, name)| {
                let (id, value, session) = (option.id.clone(), value.clone(), self.session.clone());
                Command::new(SharedString::from(format!("config-{}-{value}", option.id)), name.clone()).on_trigger(move |_, cx| {
                    session.update(cx, |session, cx| session.set_config_option(id.clone(), value.clone(), cx))
                })
            })
            .collect();
        let chosen = Some(ElementId::Name(format!("config-{}-{}", option.id, option.current).into()));
        composer.menu(
            Command::new(SharedString::from(format!("config-{}", option.id)), current).icon(IconName::Settings2),
            values,
            chosen,
        )
    }

    /// The agent's commands matching what is typed after a slash.
    fn suggestions(&self, cx: &mut Context<Self>) -> Vec<Suggestion> {
        let typed = self.composer.read(cx).text(cx);
        let Some(prefix) = typed.strip_prefix('/') else { return Vec::new() };
        if prefix.contains(char::is_whitespace) {
            return Vec::new();
        }
        self.session
            .read(cx)
            .thread
            .commands
            .iter()
            .filter(|command| command.name.starts_with(prefix))
            .take(8)
            .map(|command| {
                let (value, composer) = (format!("/{} ", command.name), self.composer.clone());
                Suggestion::new(SharedString::from(format!("command-{}", command.name)), format!("/{}", command.name), move |window, cx| {
                    composer.update(cx, |composer, cx| composer.set_text(value.clone(), window, cx));
                    composer.read(cx).focus_handle(cx).focus(window, cx);
                })
                .detail(command.description.clone())
                .code()
            })
            .collect()
    }
}

fn step_status(status: ToolStatus) -> StepStatus {
    match status {
        ToolStatus::Pending => StepStatus::Pending,
        ToolStatus::InProgress => StepStatus::Running,
        ToolStatus::Completed => StepStatus::Done,
        ToolStatus::Failed => StepStatus::Failed,
    }
}

impl Render for ChatView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
        let this = cx.entity().downgrade();
        let feed = Feed::new(&self.list, move |index, _, cx| {
            this.update(cx, |view, cx| view.render_entry(index, cx)).unwrap_or_else(|_| div().into_any_element())
        })
        .empty((empty && ready).then(|| {
            Empty::new(IconName::Bot, format!("{agent} in {project}")).body("Ask it to do something in this project.")
        }));

        let session = self.session.clone();
        let stop = busy.then(|| {
            Command::new("stop", "Stop the agent's turn").on_trigger(move |_, cx| session.update(cx, |session, cx| session.cancel(cx)))
        });
        let mut composer = Composer::new(&self.composer).stop(stop).can_send(ready).hint("/ for commands · Shift+Enter for a new line");
        if let Some(plan) = self.render_plan(cx) {
            composer = composer.above(plan);
        }
        let composer = composer.suggestions(self.suggestions(cx));
        let composer = self.config_menu(self.mode_menu(composer, cx), cx);

        let scale = cx.scale();
        column()
            .size_full()
            .children(self.render_status(cx))
            .child(feed)
            .child(
                gpui_adaptive::prim::row().w_full().justify_center().child(
                    column()
                        .w_full()
                        .max_w(rems(gpui_adaptive::feed::READING_WIDTH))
                        .gap(scale.space_s)
                        .px(scale.space_m)
                        .pt(scale.space_xs)
                        .pb(px(10.))
                        .children(self.render_sign_in(cx))
                        .children(self.render_permissions(cx))
                        .child(composer),
                ),
            )
    }
}
