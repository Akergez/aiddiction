use gpui_adaptive::prim::{pressable, row, scroll_x, surface, testable};
use gpui_adaptive::{ActiveScale, Command, Env, Surface, Text, TextField, TextFieldState};
use gpui_kit::component::button::ButtonVariants as _;
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, App, Entity, RenderOnce, SharedString, Window, div};

use super::work::Plan;

pub struct Completion {
    command: Command,
    detail: SharedString,
}

impl Completion {
    pub fn new(command: Command, detail: impl Into<SharedString>) -> Self {
        Self {
            command,
            detail: detail.into(),
        }
    }
}

#[derive(IntoElement)]
pub struct PromptComposer {
    state: Entity<TextFieldState>,
    send: Command,
    tools: Vec<Command>,
    plan: Option<Plan>,
    completions: Vec<Completion>,
}

impl PromptComposer {
    pub fn new(state: &Entity<TextFieldState>, send: Command) -> Self {
        Self {
            state: state.clone(),
            send,
            tools: Vec::new(),
            plan: None,
            completions: Vec::new(),
        }
    }

    pub fn tool(mut self, tool: Option<Command>) -> Self {
        self.tools.extend(tool);
        self
    }

    pub fn plan(mut self, plan: Option<Plan>) -> Self {
        self.plan = plan;
        self
    }

    pub fn completions(mut self, completions: Vec<Completion>) -> Self {
        self.completions = completions;
        self
    }

    fn completion_band(completions: Vec<Completion>, cx: &App) -> Option<AnyElement> {
        if completions.is_empty() {
            return None;
        }
        let scale = cx.scale();
        let rows = completions.into_iter().map(|completion| {
            let command = completion.command;
            let run = command.clone();
            let row = pressable(command.id().clone(), cx)
                .w_full()
                .gap(scale.space_s)
                .px(scale.space_m)
                .min_h(scale.row)
                .rounded(scale.radius_control)
                .child(Text::mono(command.label().clone()))
                .child(Text::small(completion.detail).muted().truncate().grow())
                .on_click(move |_, window, cx| run.run(window, cx));
            testable(row)
        });
        Some(
            surface(Surface::Card, cx)
                .p(scale.space_xs)
                .child(gpui_adaptive::prim::column().children(rows))
                .into_any_element(),
        )
    }
}

impl RenderOnce for PromptComposer {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let scale = cx.scale();
        let env = Env::of(window, cx);
        let state = self.state.clone();
        let above = self
            .plan
            .map(|plan| surface(Surface::Card, cx).overflow_hidden().child(plan).into_any_element());
        let tools = row()
            .flex_1()
            .min_w_0()
            .gap(scale.space_xs)
            .children(self.tools.iter().map(|tool| tool.icon_button(env)));
        let toolbar = row()
            .w_full()
            .gap(scale.space_s)
            .child(scroll_x(("composer-tools", self.state.entity_id()), cx, tools))
            .child(
                div()
                    .flex_none()
                    .child(self.send.icon_button(env).primary().w(scale.target).h(scale.target).rounded_full()),
            );
        gpui_adaptive::prim::column()
            .w_full()
            .gap(scale.space_xs)
            .children(above)
            .children(Self::completion_band(self.completions, cx))
            .child(TextField::new(&state))
            .child(toolbar)
    }
}
