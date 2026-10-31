//! The one view the window holds: the servers it is connected to, which page
//! is up, and the conversations that are open.
//!
//! Every server in the settings is connected at once, so the inbox has every
//! conversation of all of them; the one "in use" is only whose projects and
//! agents the other sections list. A project's page, its review and its
//! conversations belong to the project's own server, whichever that is.
//!
//! Three sections — the inbox (every conversation on every server), the
//! projects, the settings — and the pages under them, which are
//! [`gpui_adaptive::Route`]s in a [`NavigatorState`]. The navigator puts a
//! page beside the list it was opened from when there is room and over it
//! when there is not; [`Destinations`] is the way between sections. No page
//! here knows which of the two it gets.

use std::collections::HashMap;

use aiddiction_api::{Instance, Project, ProjectId};
use gpui_adaptive::{
    AppFrame, Avatar, Command, Destination, Destinations, Empty, Navigator, NavigatorEvent, NavigatorState, Presence,
    Route, Screen, Signal, Tone,
};
use gpui_adaptive::IconName;
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, App, Context, Entity, EntityId, Render, Rems, SharedString, Subscription, Window, div, rems};

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
    /// What stands beside a section's list when nothing in it is open and
    /// there is room: "Pick a conversation".
    Pick(Section),
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
            Page::Pick(section) => section,
        }
    }
}

impl Route for Page {
    /// A conversation is opened from the inbox, a project from the projects,
    /// a settings page from their list: that list stands beside it.
    fn companion(&self) -> Option<Self> {
        match self {
            Page::Chat(_) => Some(Page::Inbox),
            Page::Project(_) => Some(Page::Projects),
            Page::Appearance | Page::Agents | Page::Servers => Some(Page::Settings),
            _ => None,
        }
    }

    fn wide_detail(&self) -> Option<Self> {
        match self {
            Page::Inbox => Some(Page::Pick(Section::Inbox)),
            Page::Projects => Some(Page::Pick(Section::Projects)),
            // With room, the settings open on how the window looks.
            Page::Settings => Some(Page::Appearance),
            _ => None,
        }
    }

    fn column_width(&self) -> Rems {
        rems(21.)
    }

    /// The task board and the diff go sideways.
    fn scrolls_sideways(&self) -> bool {
        matches!(self, Page::Project(_) | Page::Review(_))
    }
}

pub struct Shell {
    /// A connection to each server in the settings, in their order.
    pub(crate) servers: Vec<Entity<ServerConnection>>,
    /// The one in use: whose projects and agents are listed.
    pub(crate) server: Option<Entity<ServerConnection>>,
    /// The pages: the one up now is its top.
    pub(crate) navigator: Entity<NavigatorState<Page>>,
    /// The page up when the navigator last changed: read when it leaves.
    shown: Page,
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
    _navigator: Subscription,
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
        let first = if Settings::get(cx).servers.is_empty() { Page::Servers } else { Page::Inbox };
        let navigator = cx.new(|_| NavigatorState::new(first));
        let navigated = cx.subscribe(&navigator, |this, _, _: &NavigatorEvent, cx| this.navigated(cx));
        let mut this = Self {
            servers: Vec::new(),
            server: None,
            navigator,
            shown: first,
            chats: Vec::new(),
            settings: None,
            reviews: HashMap::new(),
            review_from: HashMap::new(),
            _server_subscriptions: Vec::new(),
            _appearance: appearance,
            _navigator: navigated,
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
            let chats: Vec<EntityId> = self.chats.iter().map(|chat| chat.entity_id()).collect();
            self.navigator.update(cx, |navigator, cx| {
                navigator.retain(
                    |page| match *page {
                        Page::Project(id) | Page::Review(id) => !gone.contains(&id),
                        Page::Chat(id) => chats.contains(&id),
                        _ => true,
                    },
                    cx,
                )
            });
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

    /// The page up now.
    pub(crate) fn page(&self, cx: &App) -> Page {
        *self.navigator.read(cx).top()
    }

    /// Opens `page` over this one; back returns here.
    pub(super) fn go(&mut self, page: Page, cx: &mut Context<Self>) {
        self.navigator.update(cx, |navigator, cx| navigator.push(page, cx));
    }

    /// Goes to a section's page from its tab: there is nothing to go back to.
    pub(super) fn go_root(&mut self, page: Page, cx: &mut Context<Self>) {
        self.navigator.update(cx, |navigator, cx| navigator.replace_root(page, cx));
    }

    /// What a page needs as it comes up and as it goes.
    fn navigated(&mut self, cx: &mut Context<Self>) {
        let left = self.shown;
        let page = self.page(cx);
        self.shown = page;
        self.mark_seen(left, cx);
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
        self.mark_seen(page, cx);
        cx.notify();
    }

    /// Remembers how far the conversation `page` shows has been read.
    fn mark_seen(&self, page: Page, cx: &mut Context<Self>) {
        let Page::Chat(id) = page else { return };
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
        let Page::Chat(id) = self.page(cx) else { return None };
        self.chat(id)?.read(cx).session.read(cx).session_id.clone()
    }

    /// The project the page on screen is about, for a new conversation.
    pub(super) fn current_project(&self, cx: &gpui_kit::App) -> Option<ProjectId> {
        match self.page(cx) {
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
        self.mark_seen(Page::Chat(id), cx);
        self.navigator.update(cx, |navigator, cx| navigator.retain(|page| *page != Page::Chat(id), cx));
        self.chats.retain(|chat| chat.entity_id() != id);
        cx.notify();
    }

    /// Draws one page. The navigator calls it for the page up, the list
    /// beside it, and the page a back gesture shows: it changes nothing.
    fn render_route(&mut self, route: Page, page: gpui_adaptive::Page, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let screen = match route {
            Page::Inbox => self.render_inbox(cx),
            Page::Projects => self.render_projects(cx),
            Page::Settings => self.render_settings_menu(cx),
            Page::Pick(section) => self.render_pick(section, cx),
            Page::Appearance => {
                let settings = self
                    .settings
                    .get_or_insert_with(|| cx.new(|cx| super::settings_page::SettingsPage::new(window, cx)))
                    .clone();
                Screen::new("appearance", "Appearance").content(settings)
            }
            Page::Agents => self.render_agents(cx),
            Page::Servers => self.render_servers(cx),
            Page::Project(id) => self.render_project(id, window, cx),
            Page::Review(id) => self.render_review(id, cx),
            Page::Chat(id) => self.render_chat(id, cx),
        };
        screen.page(&page).in_title_bar(true).into_any_element()
    }

    /// What stands beside a section's list with nothing open. Its bar, the
    /// window's title, names the server; for the inbox, which is every
    /// server's, all of them.
    fn render_pick(&self, section: Section, cx: &mut Context<Self>) -> Screen {
        let (title, subtitle) = if section == Section::Inbox && self.servers.len() > 1 {
            let names = self.servers.iter().map(|server| server.read(cx).entry.name.clone()).collect::<Vec<_>>();
            ("All servers".into(), Some(names.join(" · ")))
        } else {
            match &self.server {
                Some(server) => {
                    let server = server.read(cx);
                    let status = match &server.status {
                        crate::server::Status::Connecting => "connecting…".to_owned(),
                        crate::server::Status::Online => server.entry.url.clone(),
                        crate::server::Status::Offline(error) => error.clone(),
                    };
                    (SharedString::from(server.entry.name.clone()), Some(status))
                }
                None => ("Aiddiction".into(), None),
            }
        };
        let empty = if self.servers.is_empty() {
            Empty::new(IconName::HardDrive, "Add a server to begin.")
        } else if section == Section::Inbox {
            Empty::new(IconName::Inbox, "Pick a conversation, or start a new one.").action(self.new_conversation_command(cx))
        } else {
            Empty::new(IconName::Folder, "Pick a project, or make a new one.")
        };
        Screen::new(("pick", section as usize), title)
            .when_some(subtitle, |screen, subtitle| screen.subtitle(subtitle))
            .scrolls(false)
            .content(empty)
    }

    fn render_chat(&self, id: EntityId, cx: &mut Context<Self>) -> Screen {
        let Some(chat) = self.chat(id).cloned() else {
            return Screen::new("chat-closed", "").scrolls(false).content(Empty::new(IconName::Inbox, "This conversation is closed."));
        };
        let session = chat.read(cx).session.read(cx);
        let title = self.session_title(session, cx);
        let subtitle = format!("{} · {}", session.project_name, session.agent_name);
        let status = if !session.thread.permissions.is_empty() {
            Some(("Needs you".into(), Signal::Waiting(Tone::Warning)))
        } else if session.busy {
            Some(("Working".into(), Signal::Busy))
        } else {
            None
        };
        let review = chat.clone();
        let shell = cx.entity().downgrade();
        Screen::new("chat", title)
            .subtitle(subtitle)
            .status(status)
            .scrolls(false)
            .command(
                Command::new("bar-review", "Review")
                    .icon(IconName::FileText)
                    .on_trigger(move |_, cx| review.update(cx, |_, cx| cx.emit(super::chat::OpenReview))),
            )
            .command(Command::new("bar-close", "Close").icon(IconName::Close).on_trigger(move |_, cx| {
                shell.update(cx, |shell, cx| shell.close_chat(id, cx)).ok();
            }))
            .content(chat)
    }

    /// A conversation's title: the server's (from its first prompt, or the
    /// agent's), else the agent's name.
    fn session_title(&self, session: &AgentSession, cx: &App) -> SharedString {
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

    /// Starts a conversation: the inbox's primary command.
    pub(super) fn new_conversation_command(&self, cx: &mut Context<Self>) -> Command {
        let shell = cx.entity().downgrade();
        Command::new("new-conversation", "New conversation")
            .icon(IconName::Plus)
            .primary()
            .disabled(self.servers.is_empty())
            .on_trigger(move |window, cx| {
                shell.update(cx, |shell, cx| shell.new_conversation(window, cx)).ok();
            })
    }

    /// The server in use, heading the rail: its letter and whether it
    /// answers. Picking it opens the servers.
    fn render_server_switch(&self, cx: &mut Context<Self>) -> AnyElement {
        let (letter, presence, name) = match &self.server {
            Some(server) => {
                let server = server.read(cx);
                let letter = server.entry.name.chars().next().unwrap_or('?').to_uppercase().collect::<String>();
                let presence = match server.status {
                    crate::server::Status::Connecting => Presence::Away,
                    crate::server::Status::Online => Presence::Online,
                    crate::server::Status::Offline(_) => Presence::Busy,
                };
                (letter, presence, SharedString::from(server.entry.name.clone()))
            }
            None => ("+".to_owned(), Presence::Offline, "No server".into()),
        };
        div()
            .id("rail-server")
            .cursor_pointer()
            .child(Avatar::new("server-avatar", letter).square().presence(presence))
            .tooltip(gpui_adaptive::prim::tooltip(name))
            .on_click(cx.listener(|this, _, _, cx| this.go_root(Page::Servers, cx)))
            .into_any_element()
    }
}

impl Render for Shell {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (section, at_root) = {
            let navigator = self.navigator.read(cx);
            (navigator.top().section(), navigator.is_at_root())
        };
        let selected = match section {
            Section::Inbox => "inbox",
            Section::Projects => "projects",
            Section::Settings => "settings",
        };
        let waiting = self.waiting(cx);
        let shell = cx.entity();
        let pages = Navigator::new("pages", &self.navigator, move |route, page, window, cx| {
            shell.update(cx, |shell, cx| shell.render_route(*route, page, window, cx))
        });
        let shell = cx.entity().downgrade();
        AppFrame::new("Aiddiction").title_bar(false).edge_to_edge(true).child(
            Destinations::new("sections", selected)
                .prefer_rail(true)
                .header(self.render_server_switch(cx))
                .destination(Destination::new("inbox", "Inbox", IconName::Inbox).count(waiting, true))
                .destination(Destination::new("projects", "Projects", IconName::Folder))
                .destination(Destination::new("settings", "Settings", IconName::Settings).pinned())
                .on_select(move |key, _, cx| {
                    let root = match key.as_ref() {
                        "projects" => Page::Projects,
                        "settings" => Page::Settings,
                        _ => Page::Inbox,
                    };
                    shell.update(cx, |shell, cx| shell.go_root(root, cx)).ok();
                })
                .bar_visible(at_root)
                .content(pages),
        )
    }
}
