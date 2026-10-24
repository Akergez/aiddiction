//! The one view the window holds: which server is in use, which page is up,
//! and the conversations that are open.
//!
//! On a desktop the sidebar (server, projects, open chats) stays beside the
//! page; on a phone one page shows at a time and the title bar has a back
//! button.

use std::collections::HashMap;

use aiddiction_api::{Instance, Project, ProjectId};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{ActiveTheme, IconName, h_flex, v_flex};
use gpui_kit::component::{Sizable as _, StyledExt as _};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, Context, Entity, EntityId, Render, SharedString, Subscription, Window, div};

use super::chat::ChatView;
use crate::server::ServerConnection;
use crate::session::AgentSession;
use crate::settings::{ServerEntry, Settings};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Servers,
    Settings,
    /// The projects: the sidebar itself on a phone, a greeting beside it on
    /// a desktop.
    Home,
    Project(ProjectId),
    /// The diff review of a project's working tree.
    Review(ProjectId),
    Chat(EntityId),
}

pub struct Shell {
    pub(crate) server: Option<Entity<ServerConnection>>,
    pub(crate) page: Page,
    pub(crate) chats: Vec<Entity<ChatView>>,
    /// Made when the page is first opened.
    settings: Option<Entity<super::settings_page::SettingsPage>>,
    /// Each project's review, with its event subscription.
    pub(crate) reviews: HashMap<ProjectId, (Entity<diffreview_ui::ReviewView>, Subscription)>,
    _server_subscription: Option<Subscription>,
    _appearance: Vec<Subscription>,
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
                }
            }),
        ];
        let active = Settings::get(cx).active_server().cloned();
        let page = if active.is_some() { Page::Home } else { Page::Servers };
        let mut this = Self {
            server: None,
            page,
            chats: Vec::new(),
            settings: None,
            reviews: HashMap::new(),
            _server_subscription: None,
            _appearance: appearance,
        };
        this.use_server(active, cx);
        this
    }

    /// Makes `entry` the server in use; the open chats belonged to the old one.
    pub(super) fn use_server(&mut self, entry: Option<ServerEntry>, cx: &mut Context<Self>) {
        self.chats.clear();
        self.reviews.clear();
        self.server = entry.map(|entry| cx.new(|cx| ServerConnection::new(entry, cx)));
        self._server_subscription = self.server.as_ref().map(|server| cx.observe(server, |_, _, cx| cx.notify()));
        if matches!(self.page, Page::Project(_) | Page::Chat(_)) {
            self.page = Page::Home;
        }
        cx.notify();
    }

    pub(super) fn go(&mut self, page: Page, cx: &mut Context<Self>) {
        if let (Page::Project(id), Some(server)) = (page, &self.server) {
            server.update(cx, |server, cx| server.refresh_instances(id, cx));
        }
        self.page = page;
        cx.notify();
    }

    fn back(&mut self, cx: &mut Context<Self>) {
        let page = match self.page {
            Page::Review(project) => Page::Project(project),
            Page::Chat(id) => self
                .chats
                .iter()
                .find(|chat| chat.entity_id() == id)
                .map(|chat| Page::Project(chat.read(cx).project_id))
                .unwrap_or(Page::Home),
            _ => Page::Home,
        };
        self.go(page, cx);
    }

    pub(crate) fn chat(&self, id: EntityId) -> Option<&Entity<ChatView>> {
        self.chats.iter().find(|chat| chat.entity_id() == id)
    }

    /// Shows the conversation with `instance`: the one already open, or a new
    /// view of it. `session` is `None` for a new conversation.
    pub(super) fn open_chat(
        &mut self,
        project: &Project,
        instance: &Instance,
        session: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(session) = &session {
            let open = self.chats.iter().find(|chat| {
                let session_of = chat.read(cx).session.read(cx);
                session_of.instance_id == instance.id && session_of.session_id.as_ref() == Some(session)
            });
            if let Some(chat) = open.cloned() {
                self.page = Page::Chat(chat.entity_id());
                chat.read(cx).composer_focus(cx).focus(window, cx);
                cx.notify();
                return;
            }
        }
        let Some(client) = self.server.as_ref().and_then(|server| server.read(cx).client().cloned()) else {
            return;
        };
        let session = cx.new(|cx| {
            AgentSession::new(client, instance.id, instance.agent_name.clone(), project.name.clone(), session, cx)
        });
        let chat = cx.new(|cx| ChatView::new(session, project.id, window, cx));
        let closed = cx.subscribe_in(&chat, window, |this, chat, _: &super::chat::Close, _, cx| {
            this.close_chat(chat.entity_id(), cx);
        });
        let project_id = project.id;
        let review = cx.subscribe_in(&chat, window, move |this, _, _: &super::chat::OpenReview, window, cx| {
            this.open_review(project_id, window, cx);
        });
        // The subscriptions live as long as the chat does.
        chat.update(cx, |chat, _| {
            chat.keep(closed);
            chat.keep(review);
        });
        self.page = Page::Chat(chat.entity_id());
        chat.read(cx).composer_focus(cx).focus(window, cx);
        self.chats.push(chat);
        cx.notify();
    }

    /// Starts `agent` in the project (or finds it running) and opens a new
    /// conversation with it.
    pub(super) fn start_chat(&mut self, project: ProjectId, agent: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(server) = self.server.clone() else { return };
        let this = cx.entity().downgrade();
        let window_handle = window.window_handle();
        server.update(cx, |server, cx| {
            server.start_instance(project, agent, cx, move |result, cx| match result {
                Ok(instance) => {
                    let _ = window_handle.update(cx, |_, window, cx| {
                        let _ = this.update(cx, |shell, cx| {
                            let project = shell.server.as_ref().and_then(|server| server.read(cx).project(project).cloned());
                            if let Some(project) = project {
                                shell.open_chat(&project, &instance, None, window, cx);
                            }
                        });
                    });
                }
                Err(error) => super::toast_error(format!("Could not start the agent: {error}"), cx),
            });
        });
    }

    fn close_chat(&mut self, id: EntityId, cx: &mut Context<Self>) {
        let project = self.chat(id).map(|chat| chat.read(cx).project_id);
        self.chats.retain(|chat| chat.entity_id() != id);
        if self.page == Page::Chat(id) {
            self.go(project.map(Page::Project).unwrap_or(Page::Home), cx);
        }
        cx.notify();
    }

    fn title(&self, cx: &Context<Self>) -> SharedString {
        match self.page {
            Page::Servers => "Servers".into(),
            Page::Settings => "Settings".into(),
            Page::Home => self
                .server
                .as_ref()
                .map(|server| SharedString::from(server.read(cx).entry.name.clone()))
                .unwrap_or_else(|| "Aiddiction".into()),
            Page::Project(id) => self
                .server
                .as_ref()
                .and_then(|server| server.read(cx).project(id).map(|project| project.name.clone()))
                .unwrap_or_default()
                .into(),
            Page::Review(id) => self
                .server
                .as_ref()
                .and_then(|server| server.read(cx).project(id).map(|project| format!("Review · {}", project.name)))
                .unwrap_or_default()
                .into(),
            Page::Chat(id) => self
                .chat(id)
                .map(|chat| {
                    let session = chat.read(cx).session.read(cx);
                    SharedString::from(format!("{} · {}", session.agent_name, session.project_name))
                })
                .unwrap_or_default(),
        }
    }

    fn render_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        match self.page {
            Page::Servers => self.render_servers(window, cx),
            Page::Settings => self
                .settings
                .get_or_insert_with(|| cx.new(|cx| super::settings_page::SettingsPage::new(window, cx)))
                .clone()
                .into_any_element(),
            Page::Home if super::is_phone(window) => self.render_sidebar(window, cx),
            Page::Home => self.render_welcome(cx),
            Page::Project(id) => self.render_project(id, window, cx),
            Page::Review(id) => self.render_review(id),
            Page::Chat(id) => match self.chat(id) {
                Some(chat) => chat.clone().into_any_element(),
                None => self.render_welcome(cx),
            },
        }
    }

    fn render_welcome(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let text = if self.server.is_none() { "Add a server to begin." } else { "Pick a project, or make a new one." };
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_2()
            .text_color(theme.muted_foreground)
            .child(gpui_kit::component::Icon::new(IconName::Bot).large())
            .child(text)
            .into_any_element()
    }
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let phone = super::is_phone(window);
        let title = self.title(cx);
        let can_go_back = phone && self.page != Page::Home;
        let theme = cx.theme().clone();

        let bar = h_flex()
            .w_full()
            .gap_2()
            .pr_2()
            .when(can_go_back, |bar| {
                bar.child(
                    Button::new("back").ghost().small().icon(IconName::ArrowLeft).on_click(cx.listener(|this, _, _, cx| this.back(cx))),
                )
            })
            .when(!can_go_back, |bar| bar.pl_1())
            .child(div().flex_1().min_w_0().truncate().text_sm().font_semibold().child(title));

        let page = self.render_page(window, cx);
        let body = if phone {
            div().flex_1().min_h_0().w_full().child(page).into_any_element()
        } else {
            h_flex()
                .flex_1()
                .min_h_0()
                .w_full()
                .items_start()
                .child(
                    div()
                        .w_72()
                        .flex_none()
                        .h_full()
                        .border_r_1()
                        .border_color(theme.sidebar_border)
                        .bg(theme.sidebar)
                        .child(self.render_sidebar(window, cx)),
                )
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
