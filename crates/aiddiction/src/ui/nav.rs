//! Getting between sections: the rail down a desktop window's side, the tabs
//! along a phone's bottom, the button that starts a conversation, and the
//! list of the settings' pages.

use gpui_kit::base::InteractiveElementExt as _;
use gpui_kit::component::badge::Badge;
use gpui_kit::component::{ActiveTheme, Icon, IconName, h_flex, v_flex};
use gpui_kit::component::{Sizable as _, StyledExt as _};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, Context, ElementId, Hsla, SharedString, div, px};

use super::shell::{Page, Section, Shell};
use crate::inbox::Group;
use crate::server::Status;

impl Shell {
    /// How many conversations wait on the person: the inbox's badge.
    fn waiting(&self, cx: &Context<Self>) -> usize {
        self.listed(cx).iter().filter(|listed| listed.group == Group::NeedsYou).count()
    }

    pub(super) fn render_rail(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let section = self.page.section();
        let (letter, status) = match &self.server {
            Some(server) => {
                let server = server.read(cx);
                let letter = server.entry.name.chars().next().unwrap_or('?').to_uppercase().collect::<String>();
                let status = match server.status {
                    Status::Connecting => theme.warning,
                    Status::Online => theme.success,
                    Status::Offline(_) => theme.danger,
                };
                (letter, status)
            }
            None => ("+".to_owned(), theme.muted_foreground),
        };
        let server_name: SharedString =
            self.server.as_ref().map(|server| server.read(cx).entry.name.clone()).unwrap_or_else(|| "No server".into()).into();
        let server_button = div()
            .id("rail-server")
            .relative()
            .size_10()
            .rounded(theme.radius * 1.5)
            .bg(theme.primary)
            .text_color(theme.primary_foreground)
            .font_semibold()
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .child(letter)
            .child(
                div()
                    .absolute()
                    .right(px(-3.))
                    .bottom(px(-3.))
                    .size(px(12.))
                    .rounded_full()
                    .border_2()
                    .border_color(theme.sidebar)
                    .bg(status),
            )
            .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(server_name.clone()).build(window, cx))
            .on_click(cx.listener(|this, _, _, cx| this.go_root(Page::Servers, cx)));

        let waiting = self.waiting(cx);
        v_flex()
            .w(gpui_kit::rems(3.75))
            .flex_none()
            .h_full()
            .items_center()
            .gap_1()
            .py_2p5()
            .bg(theme.sidebar)
            .border_r_1()
            .border_color(theme.sidebar_border)
            .child(server_button)
            .child(div().w_6().h(px(1.)).my_1p5().bg(theme.sidebar_border))
            .child(rail_button("rail-inbox", IconName::Inbox, "Inbox", section == Section::Inbox, waiting, cx)
                .on_click(cx.listener(|this, _, _, cx| this.go_root(Page::Inbox, cx))))
            .child(rail_button("rail-projects", IconName::Folder, "Projects", section == Section::Projects, 0, cx)
                .on_click(cx.listener(|this, _, _, cx| this.go_root(Page::Projects, cx))))
            .child(div().flex_1())
            .child(rail_button("rail-settings", IconName::Settings, "Settings", section == Section::Settings, 0, cx)
                .on_click(cx.listener(|this, _, _, cx| this.go_root(Page::Settings, cx))))
            .into_any_element()
    }

    pub(super) fn render_tabs(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let section = self.page.section();
        let waiting = self.waiting(cx);
        let bottom = super::insets().1;
        h_flex()
            .w_full()
            .flex_none()
            .pt_1p5()
            .pb(px(6.) + bottom)
            .bg(theme.sidebar)
            .border_t_1()
            .border_color(theme.sidebar_border)
            .child(tab("tab-inbox", IconName::Inbox, "Inbox", section == Section::Inbox, waiting, cx)
                .on_click(cx.listener(|this, _, _, cx| this.go_root(Page::Inbox, cx))))
            .child(tab("tab-projects", IconName::Folder, "Projects", section == Section::Projects, 0, cx)
                .on_click(cx.listener(|this, _, _, cx| this.go_root(Page::Projects, cx))))
            .child(tab("tab-settings", IconName::Settings, "Settings", section == Section::Settings, 0, cx)
                .on_click(cx.listener(|this, _, _, cx| this.go_root(Page::Settings, cx))))
            .into_any_element()
    }

    /// The phone's button for a new conversation, over the inbox.
    pub(super) fn render_fab(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        div()
            .id("fab")
            .absolute()
            .right_4()
            .bottom_4()
            .size(gpui_kit::rems(3.5))
            .rounded(px(18.))
            .bg(theme.primary)
            .text_color(theme.primary_foreground)
            .shadow_lg()
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .child(Icon::new(IconName::Plus).large())
            .on_click(cx.listener(|this, _, window, cx| this.new_conversation(window, cx)))
            .into_any_element()
    }

    /// The settings' pages: the column on a desktop, the page on a phone.
    pub(super) fn render_settings_menu(&self, heading: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let entries = [
            (Page::Appearance, IconName::Palette, "Appearance", "Mode, colour and theme"),
            (Page::Agents, IconName::Bot, "Agents", "What the server can run"),
            (Page::Servers, IconName::HardDrive, "Servers", "Where the projects live"),
        ];
        let mut column = v_flex().id("settings-menu").size_full().overflow_y_scroll().lock_scroll_axis().p_2().gap_0p5();
        if heading {
            column = column.child(super::inbox::column_heading("Settings", None, cx));
        }
        for (page, icon, title, subtitle) in entries {
            // The desktop's settings page is the appearance.
            let selected = self.page == page || (page == Page::Appearance && self.page == Page::Settings);
            column = column.child(
                super::inbox::row_frame(ElementId::Name(format!("settings-{title}").into()), selected, cx)
                    .child(
                        div()
                            .size_9()
                            .flex_none()
                            .rounded(theme.radius * 1.5)
                            .bg(theme.muted)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(Icon::new(icon).small().text_color(theme.muted_foreground)),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_sm().font_medium().child(title))
                            .child(div().text_xs().text_color(theme.muted_foreground).truncate().child(subtitle)),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if this.page.is_root() { this.go(page, cx) } else { this.history_replace(page, cx) }
                    })),
            );
        }
        column.into_any_element()
    }

    /// Goes to a page beside this one in the same list: back still returns
    /// to the list's page, not through every one picked.
    pub(super) fn history_replace(&mut self, page: Page, cx: &mut Context<Self>) {
        self.go_root(self.page.section().root(), cx);
        self.go(page, cx);
    }
}

fn badge(count: usize, color: Hsla, icon: Icon) -> AnyElement {
    if count == 0 {
        icon.into_any_element()
    } else {
        Badge::new().count(count).color(color).child(icon).into_any_element()
    }
}

fn rail_button(
    id: &'static str,
    icon: IconName,
    label: &'static str,
    selected: bool,
    count: usize,
    cx: &Context<Shell>,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    let theme = cx.theme();
    let hover = theme.sidebar_accent.opacity(0.6);
    div()
        .id(id)
        .size_10()
        .rounded(theme.radius * 1.5)
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .text_color(if selected { theme.sidebar_accent_foreground } else { theme.muted_foreground })
        .when(selected, |button| button.bg(theme.sidebar_accent))
        .hover(move |style| style.bg(hover))
        .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(label).build(window, cx))
        .child(badge(count, theme.warning, Icon::new(icon)))
}

fn tab(
    id: &'static str,
    icon: IconName,
    label: &'static str,
    selected: bool,
    count: usize,
    cx: &Context<Shell>,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    let theme = cx.theme();
    v_flex()
        .id(id)
        .flex_1()
        .items_center()
        .gap_1()
        .cursor_pointer()
        .child(
            div()
                .w(gpui_kit::rems(3.5))
                .h_8()
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .when(selected, |pill| pill.bg(theme.sidebar_accent).text_color(theme.sidebar_accent_foreground))
                .when(!selected, |pill| pill.text_color(theme.muted_foreground))
                .child(badge(count, theme.warning, Icon::new(icon))),
        )
        .child(
            div()
                .text_xs()
                .when(selected, |label| label.font_semibold())
                .text_color(if selected { theme.foreground } else { theme.muted_foreground })
                .child(label),
        )
}
