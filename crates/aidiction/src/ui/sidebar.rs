//! The sidebar: the server in use, its projects, and the open conversations.
//! On a phone it is the home page.

use aidiction_api::ProjectState;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme, Icon, IconName, h_flex, v_flex};
use gpui_kit::component::{Sizable as _, StyledExt as _};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, Context, ElementId, Hsla, SharedString, Window, div};

use super::shell::{Page, Shell};
use crate::server::Status;
use crate::session::Phase;

impl Shell {
    pub(super) fn render_sidebar(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let mut column = v_flex().id("sidebar").size_full().overflow_y_scroll().p_2().gap_1();

        // The server in use; a click goes to the list of servers.
        let (server_name, status, status_color) = match &self.server {
            Some(server) => {
                let server = server.read(cx);
                let (status, color) = match &server.status {
                    Status::Connecting => ("Connecting…".to_owned(), theme.warning),
                    Status::Online => (server.entry.url.clone(), theme.success),
                    Status::Offline(error) => (error.clone(), theme.danger),
                };
                (server.entry.name.clone(), status, color)
            }
            None => ("No server".to_owned(), "Add one to begin".to_owned(), theme.muted_foreground),
        };
        column = column.child(
            row("server", self.page == Page::Servers, cx)
                .child(dot(status_color))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(div().text_sm().font_semibold().truncate().child(server_name))
                        .child(div().text_xs().text_color(theme.muted_foreground).truncate().child(status)),
                )
                .child(Icon::new(IconName::ChevronsUpDown).small().text_color(theme.muted_foreground))
                .on_click(cx.listener(|this, _, _, cx| this.go(Page::Servers, cx))),
        );

        let Some(server) = self.server.clone() else {
            return v_flex().size_full().child(column.flex_1()).child(self.settings_row(cx)).into_any_element();
        };

        column = column.child(
            h_flex()
                .mt_3()
                .px_2()
                .child(heading("Projects", cx))
                .child(
                    Button::new("new-project")
                        .ghost()
                        .xsmall()
                        .icon(IconName::Plus)
                        .tooltip("New project")
                        .on_click(cx.listener(|this, _, window, cx| this.new_project(window, cx))),
                ),
        );
        let projects = server.read(cx).projects.clone();
        if projects.is_empty() {
            column = column.child(div().px_2().py_1().text_sm().text_color(theme.muted_foreground).child("No projects yet"));
        }
        for project in projects {
            let id = project.id;
            let selected = self.page == Page::Project(id);
            let mut item = row(ElementId::Name(format!("project-{id}").into()), selected, cx).child(
                match project.state {
                    ProjectState::Cloning => Spinner::new().small().into_any_element(),
                    ProjectState::Failed => Icon::new(IconName::TriangleAlert).small().text_color(theme.danger).into_any_element(),
                    ProjectState::Ready => Icon::new(if project.git_url.is_some() { IconName::Github } else { IconName::Folder })
                        .small()
                        .text_color(theme.muted_foreground)
                        .into_any_element(),
                },
            );
            item = item.child(div().flex_1().min_w_0().text_sm().truncate().child(project.name.clone()));
            let running = server.read(cx).instances.get(&id).map(|instances| instances.len()).unwrap_or(0);
            if running > 0 {
                item = item.child(Icon::new(IconName::Bot).xsmall().text_color(theme.success));
            }
            column = column.child(item.on_click(cx.listener(move |this, _, _, cx| this.go(Page::Project(id), cx))));
        }

        if !self.chats.is_empty() {
            column = column.child(div().mt_3().px_2().child(heading("Conversations", cx)));
            for chat in self.chats.clone() {
                let id = chat.entity_id();
                let session = chat.read(cx).session.read(cx);
                let label: SharedString = format!("{} · {}", session.agent_name, session.project_name).into();
                let needs_you = !session.thread.permissions.is_empty();
                let busy = session.busy;
                let broken = matches!(session.phase, Phase::Closed(_) | Phase::Reconnecting(_));
                let mut item = row(ElementId::Name(format!("chat-{id}").into()), self.page == Page::Chat(id), cx)
                    .child(Icon::new(IconName::Bot).small().text_color(theme.muted_foreground))
                    .child(div().flex_1().min_w_0().text_sm().truncate().child(label));
                if needs_you {
                    item = item.child(dot(theme.warning));
                } else if busy {
                    item = item.child(Spinner::new().xsmall());
                } else if broken {
                    item = item.child(dot(theme.danger));
                }
                column = column.child(item.on_click(cx.listener(move |this, _, _, cx| this.go(Page::Chat(id), cx))));
            }
        }
        v_flex().size_full().child(column.flex_1().min_h_0()).child(self.settings_row(cx)).into_any_element()
    }

    fn settings_row(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        div().p_2().border_t_1().border_color(theme.sidebar_border).child(
            row("settings", self.page == Page::Settings, cx)
                .child(Icon::new(IconName::Settings).small().text_color(theme.muted_foreground))
                .child(div().text_sm().child("Settings"))
                .on_click(cx.listener(|this, _, _, cx| this.go(Page::Settings, cx))),
        )
    }
}

/// A clickable row of the sidebar.
fn row(id: impl Into<ElementId>, selected: bool, cx: &Context<Shell>) -> gpui_kit::Stateful<gpui_kit::Div> {
    let theme = cx.theme();
    let hover = theme.sidebar_accent;
    h_flex()
        .id(id)
        .gap_2()
        .px_2()
        .py_1p5()
        .rounded(theme.radius)
        .cursor_pointer()
        .when(selected, |row| row.bg(theme.sidebar_accent).text_color(theme.sidebar_accent_foreground))
        .hover(move |style| style.bg(hover))
}

fn heading(text: &'static str, cx: &Context<Shell>) -> impl IntoElement {
    div().flex_1().text_xs().font_semibold().text_color(cx.theme().muted_foreground).child(text)
}

fn dot(color: Hsla) -> impl IntoElement {
    div().size_2().flex_none().rounded_full().bg(color)
}
