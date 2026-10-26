//! The one view the window holds: the servers it is connected to, which page
//! is up, and the conversations that are open.
//!
//! Every server in the settings is connected at once, so the inbox has every
//! conversation of all of them; the one "in use" is only whose projects and
//! agents the other sections list. A project's page, its review and its
//! conversations belong to the project's own server, whichever that is.
//!
//! Three sections — the inbox (every conversation on every server), the
//! projects, the settings. On a desktop a rail picks the section, a column
//! lists what is in it and the page is beside them; on a phone one page
//! shows at a time, the sections are tabs along the bottom and back goes
//! where the person came from.

use std::collections::HashMap;

use aiddiction_api::{Instance, Project, ProjectId};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme, IconName, h_flex, v_flex};
use gpui_kit::component::{Sizable as _, StyledExt as _};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, Context, Entity, EntityId, Render, SharedString, Subscription, Window, div, px};

use super::chat::ChatView;
use crate::server::ServerConnection;
use crate::session::AgentSession;
use crate::settings::Settings;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    /// Every conversation on every server: the home page.
    Inbox,
    Projects,
    Project(ProjectId),
    /// The diff review of a project's working tree.
    Review(ProjectId),
    Chat(EntityId),
    /// The settings' own page: their list on a phone, how the window looks
    /// on a desktop (where the list is the column beside it).
    Settings,
    Appearance,
    Agents,
    Servers,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Inbox,
    Projects,
    Settings,
}

impl Page {
    pub fn section(self) -> Section {
        match self {
            Page::Inbox | Page::Chat(_) => Section::Inbox,
            Page::Projects | Page::Project(_) | Page::Review(_) => Section::Projects,
            Page::Settings | Page::Appearance | Page::Agents | Page::Servers => Section::Settings,
        }
    }

    /// A section's first page: a tab on a phone, with nothing to go back to.
    pub fn is_root(self) -> bool {
        matches!(self, Page::Inbox | Page::Projects | Page::Settings)
    }
}

impl Section {
    pub fn root(self) -> Page {
        match self {
            Section::Inbox => Page::Inbox,
            Section::Projects => Page::Projects,
            Section::Settings => Page::Settings,
        }
    }
}

pub struct Shell {
    /// A connection to each server in the settings, in their order.
    pub(crate) servers: Vec<Entity<ServerConnection>>,
    /// The one in use: whose projects and agents are listed.
    pub(crate) server: Option<Entity<ServerConnection>>,
    pub(crate) page: Page,
    /// The pages before this one, for back.
    history: Vec<Page>,
    pub(crate) chats: Vec<Entity<ChatView>>,
    /// Made when the page is first opened.
    settings: Option<Entity<super::settings_page::SettingsPage>>,
    /// Each project's review, with its event subscription.
    pub(crate) reviews: HashMap<ProjectId, (Entity<diffreview_ui::ReviewView>, Subscription)>,
    /// The conversation a project's review was opened from: "Send to agent"
    /// goes there.
    pub(crate) review_from: HashMap<ProjectId, EntityId>,
    _server_subscriptions: Vec<Subscription>,
    _appearance: Vec<Subscription>,
    /// The system's back, while there is somewhere to go back to; see
    /// `render`.
    back: Option<gpui_android_glue::back::Guard>,
}

impl Shell {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Only now is there a window to ask what the desktop looks like.
        crate::appearance::apply_mode(Some(window), cx);
        let appearance = vec![
            window.observe_window_appearance(|window, cx| crate::appearance::apply_mode(Some(window), cx)),
            // Android does not announce a new wallpaper colour; it is asked
            // again whenever the window comes back to the front.
            cx.observe_window_activation(window, |_, window, cx| {
                if window.is_window_active() {
                    gpui_adaptive_colors::refresh(cx);
                    // Nor is a change between light and dark always told.
                    #[cfg(target_os = "android")]
                    if crate::appearance::behind_the_system(cx) {
                        crate::appearance::apply_mode(None, cx);
                    }
                }
            }),
        ];
        let page = if Settings::get(cx).servers.is_empty() { Page::Servers } else { Page::Inbox };
        let mut this = Self {
            servers: Vec::new(),
            server: None,
            page,
            history: Vec::new(),
            chats: Vec::new(),
            settings: None,
            reviews: HashMap::new(),
            review_from: HashMap::new(),
            _server_subscriptions: Vec::new(),
            _appearance: appearance,
            back: None,
        };
        this.sync_servers(cx);
        this
    }

    /// Connects to the servers the settings list now: a known one keeps its
    /// connection, an edited one gets a new one, and a removed one's open
    /// conversations and reviews close with it.
    pub(super) fn sync_servers(&mut self, cx: &mut Context<Self>) {
        let settings = Settings::get(cx).clone();
        let mut old = std::mem::take(&mut self.servers);
        for entry in &settings.servers {
            let kept = old.iter().position(|server| server.read(cx).entry == *entry).map(|index| old.remove(index));
            self.servers.push(kept.unwrap_or_else(|| {
                let entry = entry.clone();
                cx.new(|cx| ServerConnection::new(entry, cx))
            }));
        }
        let gone: Vec<ProjectId> = old.iter().flat_map(|server| server.read(cx).projects.iter().map(|project| project.id)).collect();
        if !gone.is_empty() {
            self.chats.retain(|chat| !gone.contains(&chat.read(cx).project_id));
            self.reviews.retain(|project, _| !gone.contains(project));
            self.review_from.retain(|project, _| !gone.contains(project));
            let open = |page: &Page, shell: &Self| match *page {
                Page::Project(id) | Page::Review(id) => !gone.contains(&id),
                Page::Chat(id) => shell.chat(id).is_some(),
                _ => true,
            };
            let history = std::mem::take(&mut self.history);
            self.history = history.into_iter().filter(|page| open(page, self)).collect();
            if !open(&self.page, self) {
                self.page = self.page.section().root();
            }
        }
        self._server_subscriptions = self.servers.iter().map(|server| cx.observe(server, |_, _, cx| cx.notify())).collect();
        self.server = settings
            .active
            .as_deref()
            .and_then(|active| self.connection(active, cx))
            .or_else(|| self.servers.first().cloned());
        cx.notify();
    }

    /// The connection to the server with this id in the settings.
    pub(crate) fn connection(&self, id: &str, cx: &gpui_kit::App) -> Option<Entity<ServerConnection>> {
        self.servers.iter().find(|server| server.read(cx).entry.id == id).cloned()
    }

    /// The server a project is on; project ids are unique across servers.
    pub(crate) fn server_of(&self, project: ProjectId, cx: &gpui_kit::App) -> Option<Entity<ServerConnection>> {
        self.servers.iter().find(|server| server.read(cx).project(project).is_some()).cloned()
    }

    /// Makes the server with this id the one in use, and remembers it.
    pub(super) fn use_server(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(server) = self.connection(id, cx) else { return };
        if Settings::get(cx).active.as_deref() != Some(id) {
            Settings::update(cx, |settings| settings.active = Some(id.to_owned()));
        }
        self.server = Some(server);
        cx.notify();
    }

    /// Goes to `page`; back returns here.
    pub(super) fn go(&mut self, page: Page, cx: &mut Context<Self>) {
        if page != self.page {
            self.history.push(self.page);
        }
        self.show(page, cx);
    }

    /// Goes to a section's page from its tab: there is nothing to go back to.
    pub(super) fn go_root(&mut self, page: Page, cx: &mut Context<Self>) {
        self.history.clear();
        self.show(page, cx);
    }

    fn show(&mut self, page: Page, cx: &mut Context<Self>) {
        self.mark_seen(cx);
        // The projects listed beside a project's page are its server's.
        if let Page::Project(id) | Page::Review(id) = page
            && let Some(server) = self.server_of(id, cx)
        {
            let entry = server.read(cx).entry.id.clone();
            self.use_server(&entry, cx);
            if let Page::Project(id) = page {
                server.update(cx, |server, cx| server.refresh_instances(id, cx));
            }
        }
        self.page = page;
        self.mark_seen(cx);
        cx.notify();
    }

    pub(super) fn back(&mut self, cx: &mut Context<Self>) {
        // Pages that are gone meanwhile (a closed chat) are passed over.
        while let Some(page) = self.history.pop() {
            if !matches!(page, Page::Chat(id) if self.chat(id).is_none()) {
                self.show(page, cx);
                return;
            }
        }
        self.show(self.page.section().root(), cx);
    }

    /// Remembers how far the conversation on screen has been read.
    fn mark_seen(&self, cx: &mut Context<Self>) {
        let Page::Chat(id) = self.page else { return };
        let Some(chat) = self.chat(id) else { return };
        let Some(server) = self.server_of(chat.read(cx).project_id, cx) else { return };
        let session = chat.read(cx).session.read(cx);
        let Some(session_id) = session.session_id.clone() else { return };
        let seq = session.last_seq();
        let server = server.read(cx).entry.id.clone();
        if Settings::get(cx).seen(&server, &session_id) < seq {
            Settings::update(cx, |settings| {
                settings.mark_seen(&server, &session_id, seq);
            });
        }
    }

    pub(crate) fn chat(&self, id: EntityId) -> Option<&Entity<ChatView>> {
        self.chats.iter().find(|chat| chat.entity_id() == id)
    }

    /// The session of the conversation on screen, if one is.
    pub(super) fn showing_session(&self, cx: &gpui_kit::App) -> Option<String> {
        let Page::Chat(id) = self.page else { return None };
        self.chat(id)?.read(cx).session.read(cx).session_id.clone()
    }

    /// The project the page on screen is about, for a new conversation.
    pub(super) fn current_project(&self, cx: &gpui_kit::App) -> Option<ProjectId> {
        match self.page {
            Page::Project(id) | Page::Review(id) => Some(id),
            Page::Chat(id) => self.chat(id).map(|chat| chat.read(cx).project_id),
            _ => None,
        }
    }

    /// Shows the conversation with `instance`: the one already open, or a new
    /// view of it. `session` is `None` for a new conversation, which is then
    /// sent `prompt` once it is ready.
    pub(super) fn open_chat(
        &mut self,
        project: &Project,
        instance: &Instance,
        session: Option<String>,
        prompt: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(session) = &session {
            let open = self.chats.iter().find(|chat| {
                let session_of = chat.read(cx).session.read(cx);
                session_of.instance_id == instance.id && session_of.session_id.as_ref() == Some(session)
            });
            if let Some(chat) = open.cloned() {
                self.go(Page::Chat(chat.entity_id()), cx);
                chat.read(cx).composer_focus(cx).focus(window, cx);
                return;
            }
        }
        let Some(client) = self.server_of(project.id, cx).and_then(|server| server.read(cx).client().cloned()) else {
            return;
        };
        let session = cx.new(|cx| {
            let mut session =
                AgentSession::new(client, instance.id, instance.agent_name.clone(), project.name.clone(), session, cx);
            session.queue_prompt(prompt);
            session
        });
        let chat = cx.new(|cx| ChatView::new(session, project.id, window, cx));
        let project_id = project.id;
        let review = cx.subscribe_in(&chat, window, move |this, chat, _: &super::chat::OpenReview, window, cx| {
            this.review_from.insert(project_id, chat.entity_id());
            this.open_review(project_id, window, cx);
        });
        // The subscription lives as long as the chat does.
        chat.update(cx, |chat, _| chat.keep(review));
        chat.read(cx).composer_focus(cx).focus(window, cx);
        self.chats.push(chat.clone());
        self.go(Page::Chat(chat.entity_id()), cx);
    }

    /// Starts `agent` in the project (or finds it running) and opens a new
    /// conversation with it, sent `prompt` first if there is one.
    pub(super) fn start_chat(
        &mut self,
        project: ProjectId,
        agent: String,
        prompt: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(server) = self.server_of(project, cx) else { return };
        Settings::update(cx, |settings| settings.last_agent = Some(agent.clone()));
        let this = cx.entity().downgrade();
        let window_handle = window.window_handle();
        server.update(cx, |server, cx| {
            server.start_instance(project, agent, cx, move |result, cx| match result {
                Ok(instance) => {
                    let _ = window_handle.update(cx, |_, window, cx| {
                        let _ = this.update(cx, |shell, cx| {
                            let project = shell.server_of(project, cx).and_then(|server| server.read(cx).project(project).cloned());
                            if let Some(project) = project {
                                shell.open_chat(&project, &instance, None, prompt, window, cx);
                            }
                        });
                    });
                }
                Err(error) => super::toast_error(format!("Could not start the agent: {error}"), cx),
            });
        });
    }

    pub(super) fn close_chat(&mut self, id: EntityId, cx: &mut Context<Self>) {
        self.mark_seen(cx);
        self.chats.retain(|chat| chat.entity_id() != id);
        if self.page == Page::Chat(id) {
            self.back(cx);
        }
        cx.notify();
    }

    fn render_page(&mut self, phone: bool, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        match self.page {
            Page::Inbox if phone => self.render_inbox(false, cx),
            Page::Projects if phone => self.render_projects(false, cx),
            Page::Settings if phone => self.render_settings_menu(false, cx),
            Page::Inbox => self.render_placeholder(IconName::Inbox, "Pick a conversation, or start a new one.", cx),
            Page::Projects => self.render_placeholder(IconName::Folder, "Pick a project, or make a new one.", cx),
            Page::Settings | Page::Appearance => self
                .settings
                .get_or_insert_with(|| cx.new(|cx| super::settings_page::SettingsPage::new(window, cx)))
                .clone()
                .into_any_element(),
            Page::Agents => self.render_agents(window, cx),
            Page::Servers => self.render_servers(window, cx),
            Page::Project(id) => self.render_project(id, window, cx),
            Page::Review(id) => self.render_review(id),
            Page::Chat(id) => match self.chat(id) {
                Some(chat) => chat.clone().into_any_element(),
                None => self.render_placeholder(IconName::Inbox, "This conversation is closed.", cx),
            },
        }
    }

    fn render_placeholder(&self, icon: IconName, text: &'static str, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let text = if self.servers.is_empty() { "Add a server to begin." } else { text };
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_3()
            .text_color(theme.muted_foreground)
            .child(
                div()
                    .size_16()
                    .rounded_full()
                    .bg(theme.muted)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(gpui_kit::component::Icon::new(icon).large()),
            )
            .child(div().text_sm().child(text))
            .when(!self.servers.is_empty() && self.page.section() == Section::Inbox, |column| {
                column.child(
                    Button::new("placeholder-new")
                        .primary()
                        .icon(IconName::Plus)
                        .label("New conversation")
                        .on_click(cx.listener(|this, _, window, cx| this.new_conversation(window, cx))),
                )
            })
            .into_any_element()
    }

    /// What the bar across the top says about the page. A desktop's
    /// section pages have their heading in the column: the bar names the
    /// server instead, or every server for the inbox, which is theirs.
    fn title(&self, phone: bool, cx: &Context<Self>) -> (SharedString, Option<SharedString>) {
        if !phone && self.page == Page::Inbox && self.servers.len() > 1 {
            let names = self.servers.iter().map(|server| server.read(cx).entry.name.clone()).collect::<Vec<_>>();
            return ("All servers".into(), Some(names.join(" · ").into()));
        }
        if !phone && self.page.is_root() {
            return match &self.server {
                Some(server) => {
                    let server = server.read(cx);
                    let status = match &server.status {
                        crate::server::Status::Connecting => "connecting…".to_owned(),
                        crate::server::Status::Online => server.entry.url.clone(),
                        crate::server::Status::Offline(error) => error.clone(),
                    };
                    (server.entry.name.clone().into(), Some(status.into()))
                }
                None => ("Aiddiction".into(), None),
            };
        }
        let project_name = |id: ProjectId| {
            self.server_of(id, cx).and_then(|server| server.read(cx).project(id).map(|project| project.name.clone())).unwrap_or_default()
        };
        match self.page {
            Page::Inbox => ("Inbox".into(), None),
            Page::Projects => ("Projects".into(), None),
            Page::Settings => ("Settings".into(), None),
            Page::Appearance => ("Appearance".into(), None),
            Page::Agents => ("Agents".into(), None),
            Page::Servers => ("Servers".into(), None),
            Page::Project(id) => (project_name(id).into(), None),
            Page::Review(id) => ("Review".into(), Some(project_name(id).into())),
            Page::Chat(id) => match self.chat(id) {
                Some(chat) => {
                    let session = chat.read(cx).session.read(cx);
                    let title = self.session_title(session, cx);
                    (title, Some(format!("{} · {}", session.project_name, session.agent_name).into()))
                }
                None => (SharedString::default(), None),
            },
        }
    }

    /// A conversation's title: the server's (from its first prompt, or the
    /// agent's), else the agent's name.
    fn session_title(&self, session: &AgentSession, cx: &Context<Self>) -> SharedString {
        let known = session.session_id.as_deref().and_then(|id| {
            self.servers.iter().find_map(|server| {
                server
                    .read(cx)
                    .instances
                    .values()
                    .flatten()
                    .flat_map(|instance| &instance.sessions)
                    .find(|summary| summary.session_id == id && !summary.title.is_empty())
                    .map(|summary| summary.title.clone())
            })
        });
        known.unwrap_or_else(|| "New conversation".into()).into()
    }

    /// The page's own buttons at the end of the bar.
    fn bar_actions(&self, phone: bool, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme().clone();
        match self.page {
            Page::Chat(id) => {
                let Some(chat) = self.chat(id).cloned() else { return Vec::new() };
                let session = chat.read(cx).session.read(cx);
                let mut actions = Vec::new();
                if phone {
                    // No room: the question's card and the stop button say as much.
                } else if !session.thread.permissions.is_empty() {
                    actions.push(super::inbox::status_tag("Needs you", theme.warning, None, cx));
                } else if session.busy {
                    actions.push(super::inbox::status_tag("Working", theme.primary, Some(Spinner::new().xsmall().into_any_element()), cx));
                }
                let review = Button::new("bar-review").ghost().small().icon(IconName::FileText).tooltip("Review the changes and annotate them for the agent");
                let review_chat = chat.clone();
                actions.push(
                    (if phone { review } else { review.label("Review") })
                        .on_click(move |_, _, cx| review_chat.update(cx, |_, cx| cx.emit(super::chat::OpenReview)))
                        .into_any_element(),
                );
                actions.push(
                    Button::new("bar-close")
                        .ghost()
                        .small()
                        .icon(IconName::Close)
                        .tooltip("Close the conversation (it goes on in the inbox)")
                        .on_click(cx.listener(move |this, _, _, cx| this.close_chat(id, cx)))
                        .into_any_element(),
                );
                actions
            }
            _ => Vec::new(),
        }
    }
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let phone = super::is_phone(window);
        let (title, subtitle) = self.title(phone, cx);
        let can_go_back = phone && !self.page.is_root();
        // The system's back does what the button does, while there is one;
        // without it back leaves the application.
        if can_go_back != self.back.is_some() {
            self.back = can_go_back.then(|| {
                let shell = cx.weak_entity();
                gpui_android_glue::back::push(move |_, cx| shell.update(cx, |shell, cx| shell.back(cx)).is_ok())
            });
        }
        let theme = cx.theme().clone();

        // A phone has no room for the subtitle beside the title: it goes under it.
        let title_text = div()
            .min_w_0()
            .truncate()
            .font_semibold()
            .text_size(if !phone { px(13.) } else if self.page.is_root() { px(22.) } else { px(14.) })
            .line_height(gpui_kit::relative(1.2))
            .child(title);
        // Two lines fit a phone's bar, and a narrow desktop window's too.
        let subtitle = subtitle.map(|subtitle| {
            div()
                .min_w_0()
                .truncate()
                .text_size(px(11.5))
                .line_height(gpui_kit::relative(1.2))
                .text_color(theme.muted_foreground)
                .child(subtitle)
        });
        let heading = if phone {
            v_flex().flex_1().min_w_0().child(title_text).children(subtitle).into_any_element()
        } else {
            h_flex()
                .flex_1()
                .min_w_0()
                .gap_2()
                .items_baseline()
                .child(title_text.flex_none().max_w(gpui_kit::relative(0.7)))
                .children(subtitle)
                .into_any_element()
        };
        let bar = h_flex()
            .w_full()
            .gap_1()
            .pr_2()
            .when(phone, |bar| bar.min_h_12())
            .when(can_go_back, |bar| {
                bar.child(
                    Button::new("back").ghost().icon(IconName::ArrowLeft).on_click(cx.listener(|this, _, _, cx| this.back(cx))),
                )
            })
            .when(!can_go_back, |bar| bar.pl_3())
            .child(heading)
            .children(self.bar_actions(phone, cx));

        let page = self.render_page(phone, window, cx);
        let body = if phone {
            let tabs = self.page.is_root().then(|| self.render_tabs(cx));
            let fab = (self.page == Page::Inbox && !self.servers.is_empty()).then(|| self.render_fab(cx));
            v_flex()
                .flex_1()
                .min_h_0()
                .w_full()
                .child(div().relative().flex_1().min_h_0().w_full().child(page).children(fab))
                .children(tabs)
                .into_any_element()
        } else {
            // The review wants every column it can get for the diff.
            let column = match self.page {
                Page::Review(_) => None,
                _ => Some(match self.page.section() {
                    Section::Inbox => self.render_inbox(true, cx),
                    Section::Projects => self.render_projects(true, cx),
                    Section::Settings => self.render_settings_menu(true, cx),
                }),
            };
            h_flex()
                .flex_1()
                .min_h_0()
                .w_full()
                .items_start()
                .child(self.render_rail(cx))
                .children(column.map(|column| {
                    div()
                        .w(gpui_kit::rems(21.))
                        .flex_none()
                        .h_full()
                        .border_r_1()
                        .border_color(theme.sidebar_border)
                        .bg(theme.sidebar)
                        .child(column)
                }))
                .child(div().flex_1().min_w_0().h_full().child(page))
                .into_any_element()
        };

        v_flex()
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(super::title_frame(bar, cx))
            .child(body)
    }
}
