//! The inbox — every conversation on the server, grouped by what it needs
//! (`crate::inbox` decides) — and the list of projects. Their rows, the
//! projects' avatars and the status tags are shared with the project page
//! and the bar.

use aiddiction_api::{InstanceState, PermissionAnswer, Project, ProjectState};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme, Icon, IconName, h_flex, v_flex};
use gpui_kit::component::{Sizable as _, StyledExt as _};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, App, Context, Div, ElementId, Hsla, SharedString, Stateful, div, hsla, px};

use super::shell::{Page, Shell};
use crate::inbox::{self, Group, Listed};
use crate::settings::Settings;

impl Shell {
    /// Every conversation on the server, in the inbox's order.
    pub(crate) fn listed(&self, cx: &App) -> Vec<Listed> {
        let Some(server) = &self.server else { return Vec::new() };
        let server = server.read(cx);
        let settings = Settings::get(cx);
        let showing = self.showing_session(cx);
        inbox::list(&server.projects, &server.instances, |session| settings.seen(&server.entry.id, session), showing.as_deref())
    }

    /// The inbox: a desktop's column, with its heading, or a phone's page,
    /// whose bar is the heading.
    pub(super) fn render_inbox(&mut self, heading: bool, cx: &mut Context<Self>) -> AnyElement {
        let listed = self.listed(cx);
        let mut column = v_flex().id("inbox").size_full().overflow_y_scroll().p_2().pb_24();
        column = column.when(heading, |column| column.child(column_heading(
            "Inbox",
            self.server.is_some().then(|| {
                Button::new("inbox-new")
                    .ghost()
                    .small()
                    .icon(IconName::Plus)
                    .tooltip("New conversation")
                    .on_click(cx.listener(|this, _, window, cx| this.new_conversation(window, cx)))
                    .into_any_element()
            }),
            cx,
        )));
        if listed.is_empty() {
            return column
                .child(empty(
                    IconName::Inbox,
                    if self.server.is_some() { "No conversations yet" } else { "No server yet" },
                    if self.server.is_some() {
                        "Start one with an agent in any of your projects: they all come here."
                    } else {
                        "Add the server your projects live on."
                    },
                    cx,
                ))
                .into_any_element();
        }
        let mut group = None;
        for item in listed {
            if group != Some(item.group) {
                group = Some(item.group);
                column = column.child(group_heading(item.group, cx));
            }
            column = column.child(self.session_row(&item, true, cx));
        }
        column.into_any_element()
    }

    /// One conversation: its project, its title, what it is doing, and the
    /// agent's question with its answers when it asks one.
    pub(super) fn session_row(&self, item: &Listed, show_project: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let selected = self.showing_session(cx).as_deref() == Some(item.session.session_id.as_str());
        let stripe = match item.group {
            Group::NeedsYou if item.session.permissions.is_empty() => Some(theme.danger),
            Group::NeedsYou => Some(theme.warning),
            Group::Working => Some(theme.primary),
            _ => None,
        };
        let indicator: Option<AnyElement> = match item.group {
            Group::NeedsYou => Some(dot(if item.session.permissions.is_empty() { theme.danger } else { theme.warning })),
            Group::Working => Some(Spinner::new().xsmall().into_any_element()),
            Group::Unread => Some(dot(theme.primary)),
            Group::Recent | Group::Stopped => None,
        };
        let meta: SharedString = if show_project {
            format!("{} · {}", item.project.name, item.instance.agent_name).into()
        } else {
            item.instance.agent_name.clone().into()
        };
        let time = inbox::ago(inbox::now_ms(), item.session.updated_at_ms);
        let unread = matches!(item.group, Group::Unread | Group::NeedsYou);

        let mut text = v_flex()
            .flex_1()
            .min_w_0()
            .gap_0p5()
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .when(unread, |title| title.font_semibold())
                            .when(!unread, |title| title.font_medium())
                            .child(item.title().to_owned()),
                    )
                    .child(div().flex_none().text_xs().text_color(theme.muted_foreground).child(time)),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(div().flex_1().min_w_0().truncate().text_xs().text_color(theme.muted_foreground).child(meta))
                    .children(indicator),
            );
        if let Some(permission) = item.session.permissions.first() {
            let mut buttons = h_flex().gap_1p5().flex_wrap();
            for option in permission.options.iter().take(3) {
                let server = self.server.clone();
                let answer = PermissionAnswer { request_id: permission.request_id.clone(), option_id: option.id.clone() };
                let instance = item.instance.id;
                // Big enough for a finger: this is how a phone answers.
                let button = Button::new(ElementId::Name(format!("answer-{}-{}", item.session.session_id, option.id).into()))
                    .small()
                    .label(option.name.clone())
                    .on_click(move |_, _, cx| {
                        // The row under the button opens the conversation;
                        // answering is all this click is for.
                        cx.stop_propagation();
                        if let Some(server) = &server {
                            server.update(cx, |server, cx| server.answer_permission(instance, answer.clone(), cx));
                        }
                    });
                buttons = buttons.child(if option.kind.starts_with("allow") { button.primary() } else { button.outline() });
            }
            text = text.child(
                v_flex()
                    .mt_1p5()
                    .gap_2()
                    .p_2()
                    .rounded(theme.radius * 1.5)
                    .bg(theme.warning.opacity(0.1))
                    .border_1()
                    .border_color(theme.warning.opacity(0.35))
                    .child(
                        h_flex()
                            .gap_1p5()
                            .text_xs()
                            .child(Icon::new(IconName::TriangleAlert).xsmall().text_color(theme.warning))
                            .child(div().flex_1().min_w_0().truncate().child(permission.title.clone())),
                    )
                    .child(buttons),
            );
        } else if let (Group::NeedsYou, Some(error)) = (item.group, &item.session.error) {
            text = text.child(
                h_flex()
                    .gap_1p5()
                    .text_xs()
                    .text_color(theme.danger)
                    .child(Icon::new(IconName::CircleX).xsmall())
                    .child(div().flex_1().min_w_0().truncate().child(error.clone())),
            );
        } else if !item.session.preview.is_empty() {
            text = text.child(div().truncate().text_xs().text_color(theme.muted_foreground).child(item.session.preview.clone()));
        } else if item.group == Group::Stopped {
            text = text.child(div().truncate().text_xs().text_color(theme.muted_foreground).child("The agent stopped"));
        }

        let project = item.project.clone();
        let instance = item.instance.clone();
        let session = item.session.session_id.clone();
        let stopped = item.group == Group::Stopped;
        let project_id = item.project.id;
        row_frame(ElementId::Name(format!("session-{}", item.session.session_id).into()), selected, cx)
            .relative()
            .items_start()
            .when(stopped, |row| row.opacity(0.6))
            .children(stripe.map(|color| div().absolute().left(px(2.)).top_3().bottom_3().w(px(3.)).rounded_full().bg(color)))
            .child(avatar(&item.project, false, cx))
            .child(text)
            .on_click(cx.listener(move |this, _, window, cx| {
                if stopped {
                    this.go(Page::Project(project_id), cx);
                } else {
                    this.open_chat(&project, &instance, Some(session.clone()), None, window, cx);
                }
            }))
            .into_any_element()
    }

    pub(super) fn render_projects(&mut self, heading: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let mut column = v_flex().id("projects").size_full().overflow_y_scroll().p_2();
        column = column.when(heading, |column| column.child(column_heading(
            "Projects",
            self.server.is_some().then(|| {
                Button::new("new-project")
                    .ghost()
                    .small()
                    .icon(IconName::Plus)
                    .tooltip("New project")
                    .on_click(cx.listener(|this, _, window, cx| this.new_project(window, cx)))
                    .into_any_element()
            }),
            cx,
        )));
        let Some(server) = self.server.clone() else {
            return column.child(empty(IconName::Folder, "No server yet", "Add the server your projects live on.", cx)).into_any_element();
        };
        let projects = server.read(cx).projects.clone();
        if projects.is_empty() {
            return column
                .child(empty(IconName::Folder, "No projects yet", "Make one: an empty folder, a git clone, or a folder already on the server.", cx))
                .into_any_element();
        }
        let listed = self.listed(cx);
        for project in projects {
            let id = project.id;
            let of_project = || listed.iter().filter(move |item| item.project.id == id);
            let waiting = of_project().filter(|item| item.group == Group::NeedsYou).count();
            let working = of_project().filter(|item| item.group == Group::Working).count();
            let mut counts = Vec::new();
            if waiting > 0 {
                counts.push(if waiting == 1 { "1 needs you".to_owned() } else { format!("{waiting} need you") });
            }
            if working > 0 {
                counts.push(format!("{working} working"));
            }
            let total = of_project().count();
            if counts.is_empty() && total > 0 {
                counts.push(format!("{total} conversation{}", if total == 1 { "" } else { "s" }));
            }
            let status: SharedString = match project.state {
                ProjectState::Cloning => project.status_message.clone().unwrap_or_else(|| "Cloning…".into()).into(),
                ProjectState::Failed => "Could not be made".into(),
                ProjectState::Ready => counts.join(" · ").into(),
            };
            let selected = matches!(self.page, Page::Project(open) | Page::Review(open) if open == id);
            column = column.child(
                row_frame(ElementId::Name(format!("project-{id}").into()), selected, cx)
                    .child(avatar(&project, false, cx))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_sm().font_medium().truncate().child(project.name.clone()))
                            .child(div().text_xs().text_color(theme.muted_foreground).truncate().child(origin(&project)))
                            .when(!status.is_empty(), |text| {
                                text.child(
                                    div()
                                        .text_xs()
                                        .truncate()
                                        .text_color(if waiting > 0 { theme.warning } else { theme.muted_foreground })
                                        .child(status),
                                )
                            }),
                    )
                    .when(project.state == ProjectState::Cloning, |row| row.child(Spinner::new().xsmall()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if this.page.is_root() { this.go(Page::Project(id), cx) } else { this.history_replace(Page::Project(id), cx) }
                    })),
            );
        }
        column.into_any_element()
    }
}

/// Where a project came from, in a line.
pub fn origin(project: &Project) -> SharedString {
    match (&project.path, &project.git_url) {
        (Some(path), _) => path.clone().into(),
        (None, Some(url)) => url.trim_start_matches("https://").trim_end_matches(".git").to_owned().into(),
        (None, None) => "Empty folder".into(),
    }
}

/// A clickable row of a list column.
pub fn row_frame(id: impl Into<ElementId>, selected: bool, cx: &App) -> Stateful<Div> {
    let theme = cx.theme();
    let hover = theme.sidebar_accent.opacity(0.5);
    h_flex()
        .id(id)
        .gap_3()
        .px_3()
        .py_2p5()
        .rounded(theme.radius * 1.5)
        .cursor_pointer()
        .when(selected, |row| row.bg(theme.sidebar_accent).text_color(theme.sidebar_accent_foreground))
        .when(!selected, |row| row.hover(move |style| style.bg(hover)))
}

/// The title of a list column, with its button.
pub fn column_heading(title: &'static str, action: Option<AnyElement>, cx: &App) -> impl IntoElement {
    let _ = cx;
    h_flex()
        .h_10()
        .px_2()
        .mb_1()
        .child(div().flex_1().text_lg().font_semibold().child(title))
        .children(action)
}

fn group_heading(group: Group, cx: &App) -> impl IntoElement {
    div()
        .px_3()
        .pt_3()
        .pb_1()
        .text_xs()
        .font_semibold()
        .text_color(if group == Group::NeedsYou { cx.theme().warning } else { cx.theme().muted_foreground })
        .child(group.title())
}

fn empty(icon: IconName, title: &'static str, text: &'static str, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    v_flex()
        .items_center()
        .gap_2()
        .px_6()
        .py_10()
        .text_center()
        .child(
            div()
                .size_12()
                .rounded_full()
                .bg(theme.muted)
                .flex()
                .items_center()
                .justify_center()
                .child(Icon::new(icon).text_color(theme.muted_foreground)),
        )
        .child(div().text_sm().font_semibold().child(title))
        .child(div().text_xs().text_color(theme.muted_foreground).child(text))
}

/// A project's letter on its own colour.
pub fn avatar(project: &Project, large: bool, cx: &App) -> AnyElement {
    let dark = cx.theme().mode.is_dark();
    let hue = inbox::hue(project.id);
    let background = hsla(hue, 0.55, if dark { 0.26 } else { 0.88 }, 1.0);
    let foreground = hsla(hue, 0.6, if dark { 0.8 } else { 0.3 }, 1.0);
    let letter = project.name.chars().find(|c| c.is_alphanumeric()).unwrap_or('?').to_uppercase().collect::<String>();
    div()
        .flex_none()
        .when(large, |avatar| avatar.size_12().rounded(px(14.)).text_xl())
        .when(!large, |avatar| avatar.size_9().rounded(px(10.)).text_sm())
        .bg(background)
        .text_color(foreground)
        .font_bold()
        .flex()
        .items_center()
        .justify_center()
        .child(letter)
        .into_any_element()
}

pub fn dot(color: Hsla) -> AnyElement {
    div().size_2().flex_none().rounded_full().bg(color).into_any_element()
}

/// A short state on a tinted pill: "Working", "Needs you".
pub fn status_tag(label: &'static str, color: Hsla, icon: Option<AnyElement>, cx: &App) -> AnyElement {
    let _ = cx;
    h_flex()
        .flex_none()
        .gap_1p5()
        .h_6()
        .px_2p5()
        .rounded_full()
        .bg(color.opacity(0.14))
        .text_color(color)
        .text_xs()
        .font_semibold()
        .child(icon.unwrap_or_else(|| dot(color)))
        .child(label)
        .into_any_element()
}

/// The dot an agent's state is drawn as.
pub fn instance_color(state: InstanceState, cx: &App) -> Hsla {
    let theme = cx.theme();
    match state {
        InstanceState::Starting => theme.warning,
        InstanceState::Ready => theme.success,
        InstanceState::Exited => theme.muted_foreground,
        InstanceState::Failed => theme.danger,
    }
}
