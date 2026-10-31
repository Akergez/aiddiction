//! The settings page: how the window looks. Everything takes effect as it is
//! chosen. The theme lists and the registry browser are the engine's own
//! (`gpui-zed-themes`); this page only lays them out.

use gpui_adaptive::{Choice, ChoiceEvent, ChoiceOption, ChoiceState, Segments, Setting, Stack, Swatches};
use gpui_adaptive_colors::Color;
use gpui_kit::component::ThemeMode;
use gpui_kit::prelude::*;
use gpui_kit::{App, AppContext, Context, Entity, Render, Subscription, Window, div, rems};
use gpui_zed_themes::{Browser, BrowserEvent};

use crate::appearance::{self, Mode, SEEDS};

pub struct SettingsPage {
    light: Entity<ChoiceState>,
    dark: Entity<ChoiceState>,
    browser: Entity<Browser>,
    browsing: bool,
    _subscriptions: Vec<Subscription>,
}

/// The themes installed for `mode`, as options.
fn theme_options(mode: ThemeMode, cx: &App) -> Vec<ChoiceOption> {
    appearance::theme_names(mode, cx).into_iter().map(|name| ChoiceOption::new(name.clone(), name)).collect()
}

impl SettingsPage {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut subscriptions = Vec::new();
        let mut picker = |mode: ThemeMode, cx: &mut Context<Self>| {
            let (options, worn) = (theme_options(mode, cx), appearance::worn(mode, cx));
            let state = cx.new(|cx| ChoiceState::searchable(options, Some(worn), window, cx));
            subscriptions.push(cx.subscribe(&state, move |_, _, event: &ChoiceEvent, cx| {
                let ChoiceEvent::Change(name) = event;
                appearance::choose_theme(mode, name, cx);
            }));
            state
        };
        let light = picker(ThemeMode::Light, cx);
        let dark = picker(ThemeMode::Dark, cx);
        let browser = cx.new(|cx| Browser::new(appearance::registry(), appearance::store(), window, cx));
        subscriptions.push(cx.subscribe_in(&browser, window, |this, _, _: &BrowserEvent, window, cx| {
            appearance::reload_themes(cx);
            this.relist(window, cx);
        }));
        Self { light, dark, browser, browsing: false, _subscriptions: subscriptions }
    }

    /// Makes the two lists say what is installed now.
    fn relist(&self, window: &mut Window, cx: &mut Context<Self>) {
        for (state, mode) in [(&self.light, ThemeMode::Light), (&self.dark, ThemeMode::Dark)] {
            let (options, worn) = (theme_options(mode, cx), appearance::worn(mode, cx));
            state.update(cx, |state, cx| state.set_options(options, Some(worn), window, cx));
        }
        cx.notify();
    }

    fn browse(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.browsing = !self.browsing;
        if self.browsing {
            self.browser.update(cx, |browser, cx| {
                browser.load(cx);
                browser.focus(window, cx);
            });
        }
        cx.notify();
    }
}

/// The colours a scheme can be made from, the one in use picked.
fn colors(current: Option<Color>) -> Swatches {
    let picked = SEEDS.into_iter().find(|(stored, _)| current.is_some() && Color::parse_hex(stored) == current).map(|(stored, _)| stored);
    SEEDS
        .into_iter()
        .filter_map(|(stored, name)| Some((stored, name, Color::parse_hex(stored)?)))
        .fold(Swatches::new("colors", picked).unset("System"), |swatches, (stored, name, color)| {
            swatches.swatch(stored, name, gpui_adaptive_colors::hsla(color))
        })
        .on_pick(|key, _, cx| appearance::set_color(key.and_then(|key| Color::parse_hex(key)), cx))
}

impl Render for SettingsPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mode = appearance::mode(cx);
        let modes = Mode::ALL
            .iter()
            .fold(Segments::new("mode", mode.label()), |segments, choice| segments.segment(choice.label(), choice.label()))
            .on_change(|key, window, cx| {
                if let Some(choice) = Mode::ALL.iter().find(|choice| choice.label() == key.as_ref()) {
                    appearance::set_mode(*choice, window, cx);
                }
            });
        let theme = |state: &Entity<ChoiceState>, id: &'static str| div().w(rems(14.)).child(Choice::new(state).id(id));
        let browse = gpui_adaptive::Command::new("browse-themes", if self.browsing { "Done" } else { "Browse…" }).on_trigger({
            let page = cx.entity().downgrade();
            move |window, cx| {
                page.update(cx, |page, cx| page.browse(window, cx)).ok();
            }
        });
        let env = gpui_adaptive::Env::of(window, cx);
        let more = Setting::new("More themes").detail("From the Zed editor's registry").control(browse.button(env));
        Stack::new()
            .child(Setting::new("Mode").detail("System follows the desktop as it changes").control(modes))
            .child(
                Setting::new("Color")
                    .detail("The scheme is made from it; System follows the desktop's accent")
                    .below(colors(appearance::color(cx))),
            )
            .child(Setting::new("Light theme").detail("Worn in light, instead of the scheme").control(theme(&self.light, "light-theme")))
            .child(Setting::new("Dark theme").detail("Worn in dark, instead of the scheme").control(theme(&self.dark, "dark-theme")))
            .child(if self.browsing { more.below(self.browser.clone()) } else { more })
    }
}
