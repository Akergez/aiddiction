//! How an agent's work reads in a conversation: a [`Step`] (it ran a
//! command, edited a file) that unfolds into what came of it, a run of steps
//! summed up in one line, the [`Plan`] on the composer with how far along it
//! is, and the [`Diff`] of an edit. Made of the design system's primitives,
//! so they look like the rest in either skin.

use std::rc::Rc;

use gpui_adaptive::prim::{Surface, column, pressable, row, spinner, surface};
use gpui_adaptive::{ActiveColors, ActiveScale, Handler, Icon, IconName, Meter, Note, Sizable as _, Text};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, App, ElementId, RenderOnce, SharedString, Window, div};

/// Where a step is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepStatus {
    Pending,
    Running,
    Done,
    Failed,
}

impl StepStatus {
    fn mark(self, cx: &App) -> AnyElement {
        let colors = cx.colors();
        match self {
            StepStatus::Pending => Icon::new(IconName::Ellipsis).small().text_color(colors.muted).into_any_element(),
            StepStatus::Running => spinner(),
            StepStatus::Done => Icon::new(IconName::CircleCheck).small().text_color(colors.success).into_any_element(),
            StepStatus::Failed => Icon::new(IconName::CircleX).small().text_color(colors.danger).into_any_element(),
        }
    }
}

/// A step of work, or a run of them summed up. Controlled: the owner keeps
/// whether it is open.
#[derive(IntoElement)]
pub struct Step {
    id: ElementId,
    title: SharedString,
    icon: Option<IconName>,
    status: StepStatus,
    open: bool,
    on_toggle: Option<Handler>,
    children: Vec<AnyElement>,
}

impl Step {
    pub fn new(id: impl Into<ElementId>, title: impl Into<SharedString>, status: StepStatus) -> Self {
        Step { id: id.into(), title: title.into(), icon: None, status, open: false, on_toggle: None, children: Vec::new() }
    }

    /// What kind of step it is. With an icon the state goes at the end;
    /// without one (a run of steps) it leads, and a chevron says the line
    /// unfolds.
    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn open(mut self, open: bool) -> Self {
        self.open = open;
        self
    }

    pub fn on_toggle(mut self, on_toggle: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_toggle = Some(Rc::new(on_toggle));
        self
    }

    pub fn child(mut self, child: impl IntoElement) -> Self {
        self.children.push(child.into_any_element());
        self
    }
}

impl RenderOnce for Step {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let (colors, scale) = (cx.colors(), cx.scale());
        let mark = self.status.mark(cx);
        let title = Text::small(self.title).truncate().grow();
        let head = match &self.on_toggle {
            Some(on_toggle) => {
                let on_toggle = on_toggle.clone();
                pressable(self.id, cx).on_click(move |_, window, cx| on_toggle(window, cx))
            }
            None => row().id(self.id),
        }
        .gap(scale.space_s)
        .px(scale.space_m)
        .py(scale.space_s)
        .min_h(scale.target)
        .rounded(scale.radius_container);
        let head = match self.icon {
            Some(icon) => head.child(Icon::new(icon).small().text_color(colors.muted)).child(title).child(mark),
            None => head.child(mark).child(title).child(
                Icon::new(if self.open { IconName::ChevronUp } else { IconName::ChevronDown }).xsmall().text_color(colors.muted),
            ),
        };
        surface(Surface::Card, cx).child(head).when(self.open && !self.children.is_empty(), |card| {
            card.child(column().gap(scale.space_s).px(scale.space_m).pb(scale.space_m).children(self.children))
        })
    }
}

/// What the agent plans to do, folded into one line with how far along it
/// is.
#[derive(IntoElement)]
pub struct Plan {
    items: Vec<(SharedString, StepStatus)>,
    open: bool,
    on_toggle: Handler,
}

impl Plan {
    pub fn new(open: bool, on_toggle: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        Plan { items: Vec::new(), open, on_toggle: Rc::new(on_toggle) }
    }

    pub fn item(mut self, text: impl Into<SharedString>, status: StepStatus) -> Self {
        self.items.push((text.into(), status));
        self
    }
}

impl RenderOnce for Plan {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let (colors, scale) = (cx.colors(), cx.scale());
        let total = self.items.len();
        let done = self.items.iter().filter(|(_, status)| *status == StepStatus::Done).count();
        let current = self
            .items
            .iter()
            .find(|(_, status)| *status == StepStatus::Running)
            .or_else(|| self.items.iter().find(|(_, status)| *status != StepStatus::Done))
            .map(|(text, _)| text.clone())
            .unwrap_or_default();
        let on_toggle = self.on_toggle.clone();
        let line = pressable("plan", cx)
            .gap(scale.space_s)
            .px(scale.space_m)
            .py(scale.space_s)
            .min_h(scale.target)
            .on_click(move |_, window, cx| on_toggle(window, cx))
            .child(Icon::new(IconName::CircleCheck).xsmall().text_color(if done == total { colors.success } else { colors.accent }))
            .child(Text::small(format!("Plan {done}/{total}")).strong())
            .child(Text::small(current).muted().truncate().grow())
            .child(Meter::new(if total == 0 { 0. } else { done as f32 / total as f32 }))
            .child(Icon::new(if self.open { IconName::ChevronDown } else { IconName::ChevronUp }).xsmall().text_color(colors.muted));
        if !self.open {
            return line.into_any_element();
        }
        let items = self.items.into_iter().map(|(text, status)| {
            let (icon, color) = match status {
                StepStatus::Done => (IconName::CircleCheck, colors.success),
                StepStatus::Running => (IconName::LoaderCircle, colors.accent),
                StepStatus::Failed => (IconName::CircleX, colors.danger),
                StepStatus::Pending => (IconName::Dash, colors.muted),
            };
            row().gap(scale.space_s).child(Icon::new(icon).xsmall().text_color(color)).child(Text::small(text).truncate())
        });
        column()
            .child(line)
            .child(
                column()
                    .gap(scale.space_xs)
                    .px(scale.space_m)
                    .py(scale.space_s)
                    .border_t_1()
                    .border_color(colors.border)
                    .children(items),
            )
            .into_any_element()
    }
}

/// The most lines a side of a diff shows.
const OLD_LINES: usize = 200;
const NEW_LINES: usize = 400;

/// What an edit did to a file: the old text as removed lines and the new as
/// added. Not a line-by-line comparison: enough to see what changed.
#[derive(IntoElement)]
pub struct Diff {
    path: SharedString,
    old: Option<String>,
    new: String,
}

impl Diff {
    pub fn new(path: impl Into<SharedString>, old: Option<String>, new: String) -> Self {
        Diff { path: path.into(), old, new }
    }
}

impl RenderOnce for Diff {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let (colors, scale) = (cx.colors(), cx.scale());
        let line = |sign: char, text: &str, bg| div().bg(bg).child(Text::mono(format!("{sign} {text}")));
        let removed = colors.danger.opacity(0.12);
        let added = colors.success.opacity(0.12);
        let old: Vec<_> = self.old.iter().flat_map(|old| old.lines().take(OLD_LINES)).map(|text| line('-', text, removed)).collect();
        let new: Vec<_> = self.new.lines().take(NEW_LINES).map(|text| line('+', text, added)).collect();
        column()
            .w_full()
            .gap(scale.space_xs)
            .child(Note::new(self.path).truncate())
            .child(surface(Surface::Well, cx).p(scale.space_s).children(old).children(new))
    }
}
