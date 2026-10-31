//! The list of the settings' pages, and how many conversations wait on the
//! person: the inbox's count in the rail and the bar.

use gpui_adaptive::{Collection, ListRow, Screen};
use gpui_adaptive::{Icon, IconName};
use gpui_kit::{App, Context, SharedString};

use super::shell::{Page, Shell};
use crate::inbox::Group;

impl Shell {
    /// How many conversations wait on the person: the inbox's count.
    pub(super) fn waiting(&self, cx: &App) -> usize {
        self.listed(cx).iter().filter(|listed| listed.group == Group::NeedsYou).count()
    }

    /// The settings' pages.
    pub(super) fn render_settings_menu(&self, cx: &mut Context<Self>) -> Screen {
        let entries = [
            (Page::Appearance, "appearance", IconName::Palette, "Appearance", "Mode, colour and theme"),
            (Page::Agents, "agents", IconName::Bot, "Agents", "What the server can run"),
            (Page::Servers, "servers", IconName::HardDrive, "Servers", "Where the projects live"),
        ];
        let page = self.page(cx);
        let rows = entries.map(|(target, key, icon, title, subtitle)| {
            // Beside the list with nothing else open, the settings show how
            // the window looks.
            let selected = page == target || (target == Page::Appearance && page == Page::Settings);
            let shell = cx.entity().downgrade();
            ListRow::new(SharedString::from(format!("settings-{key}")), title)
                .leading(Icon::new(icon))
                .subtitle(subtitle)
                .selected(selected)
                .on_activate(move |_, cx| {
                    shell.update(cx, |shell, cx| shell.go(target, cx)).ok();
                })
        });
        Screen::new("settings-menu", "Settings").root(true).content(Collection::new("settings-list").children(rows))
    }
}
