//! A project's page: where it came from, the agents running in it, and its
//! conversations. New conversations and the review start here too.

use aiddiction_api::{InstanceState, ProjectId, ProjectState, Task, TaskStatus};
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::{ActiveTheme, IconName, WindowExt, h_flex, v_flex};
use gpui_kit::component::{Sizable as _, StyledExt as _};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, AppContext, Context, ElementId, Entity, SharedString, Window, div};

use super::forms::{ProjectForm, TaskForm, open_form};
use super::shell::{Page, Shell};
use crate::server::ServerConnection;

impl Shell {
    pub(super) fn render_project(&mut self, id: ProjectId, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let Some(server) = self.server_of(id, cx) else {
            return div().p_4().text_color(theme.muted_foreground).child("This project is gone.").into_any_element();
        };
        let (project, instances, tasks) = {
            let server = server.read(cx);
            let Some(project) = server.project(id).cloned() else {
                return div().p_4().text_color(theme.muted_foreground).child("This project is gone.").into_any_element();
            };
            (project, server.instances.get(&id).cloned().unwrap_or_default(), server.tasks.get(&id).cloned().unwrap_or_default())
        };
        let ready = project.state == ProjectState::Ready;

        let delete = Button::new("delete-project")
            .ghost()
            .icon(IconName::Delete)
            .tooltip("Delete project")
            .on_click(cx.listener(move |this, _, window, cx| this.delete_project(id, window, cx)));
        let actions = h_flex().gap_2().flex_wrap().when(ready, |buttons| {
            buttons
                .child(
                    Button::new("review")
                        .outline()
                        .icon(IconName::FileText)
                        .label("Review changes")
                        .on_click(cx.listener(move |this, _, window, cx| this.open_review(id, window, cx))),
                )
                .child(
                    Button::new("project-new-chat")
                        .primary()
                        .icon(IconName::Plus)
                        .label("New conversation")
                        .on_click(cx.listener(|this, _, window, cx| this.new_conversation(window, cx))),
                )
        });
        let title = h_flex()
            .flex_1()
            .min_w_0()
            .gap_4()
            .child(super::inbox::avatar(&project, true, cx))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().text_xl().font_semibold().truncate().child(project.name.clone()))
                    .child(div().text_sm().text_color(theme.muted_foreground).truncate().child(super::inbox::origin(&project))),
            );
        // A phone has the buttons under the title; a desktop, beside it.
        let header = if super::is_phone(window) {
            v_flex().gap_4().child(title.child(delete)).child(actions).into_any_element()
        } else {
            h_flex().gap_4().child(title).child(actions.child(delete)).into_any_element()
        };

        let mut page = v_flex().gap_8().w_full().max_w(gpui_kit::rems(56.)).child(header);

        match project.state {
            ProjectState::Cloning => {
                page = page.child(
                    h_flex()
                        .gap_2()
                        .p_3()
                        .rounded(theme.radius * 1.5)
                        .bg(theme.muted)
                        .child(Spinner::new().small())
                        .child(div().text_sm().truncate().child(project.status_message.clone().unwrap_or_else(|| "Cloning…".into()))),
                );
            }
            ProjectState::Failed => {
                page = page.child(
                    div()
                        .p_3()
                        .rounded(theme.radius * 1.5)
                        .bg(theme.danger.opacity(0.12))
                        .text_sm()
                        .text_color(theme.danger)
                        .child(project.status_message.clone().unwrap_or_else(|| "The project could not be made.".into())),
                );
            }
            ProjectState::Ready => {
                if !instances.is_empty() {
                    let mut chips = h_flex().gap_2().flex_wrap();
                    let mut failures = v_flex().gap_2();
                    for instance in &instances {
                        let alive = matches!(instance.state, InstanceState::Starting | InstanceState::Ready);
                        let state: SharedString = match instance.state {
                            InstanceState::Starting => "starting".into(),
                            InstanceState::Ready => match instance.sessions.len() {
                                1 => "1 conversation".into(),
                                n => format!("{n} conversations").into(),
                            },
                            InstanceState::Exited => "stopped".into(),
                            InstanceState::Failed => "failed".into(),
                        };
                        let stopped = instance.clone();
                        let stop_server = server.clone();
                        chips = chips.child(
                            h_flex()
                                .id(ElementId::Name(format!("instance-{}", instance.id).into()))
                                .gap_2()
                                .pl_3()
                                .pr_1()
                                .py_1()
                                .rounded(theme.radius * 1.5)
                                .border_1()
                                .border_color(theme.border)
                                .child(super::inbox::dot(super::inbox::instance_color(instance.state, cx)))
                                .child(div().text_sm().font_semibold().child(instance.agent_name.clone()))
                                .child(div().text_xs().text_color(theme.muted_foreground).child(state))
                                .child(
                                    Button::new(ElementId::Name(format!("stop-{}", instance.id).into()))
                                        .ghost()
                                        .xsmall()
                                        .icon(if alive { IconName::Pause } else { IconName::Close })
                                        .tooltip(if alive { "Stop the agent" } else { "Dismiss" })
                                        .on_click(move |_, _, cx| stop_server.update(cx, |server, cx| server.stop_instance(&stopped, cx))),
                                ),
                        );
                        if !alive && !instance.stderr_tail.is_empty() {
                            let tail = instance.stderr_tail.iter().rev().take(6).rev().cloned().collect::<Vec<_>>().join("\n");
                            failures = failures.child(
                                v_flex()
                                    .gap_1()
                                    .child(div().text_xs().text_color(theme.muted_foreground).child(format!(
                                        "{}: {}",
                                        instance.agent_name,
                                        instance.message.clone().unwrap_or_else(|| "stopped".into())
                                    )))
                                    .child(
                                        div()
                                            .p_2()
                                            .rounded(theme.radius)
                                            .bg(theme.muted)
                                            .font_family(theme.mono_font_family.clone())
                                            .text_xs()
                                            .child(tail),
                                    ),
                            );
                        }
                    }
                    page = page.child(v_flex().gap_3().child(section("Agents", cx)).child(chips).child(failures));
                }

                let listed: Vec<_> = self.listed(cx).into_iter().filter(|item| item.project.id == id).collect();
                page = page.child(v_flex().gap_2().child(section("Tasks", cx)).child(self.render_board(&server, id, &tasks, cx)));
                let mut conversations = v_flex().gap_0p5().child(div().mb_2().child(section("Conversations", cx)));
                if listed.is_empty() {
                    conversations = conversations.child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child("No conversations here yet. Start one: the agent works in this project's folder."),
                    );
                }
                for item in &listed {
                    conversations = conversations.child(self.session_row(item, false, cx));
                }
                page = page.child(conversations);
            }
        }

        div()
            .id("project")
            .size_full()
            .overflow_y_scroll()
            .child(h_flex().w_full().justify_center().px_6().py_6().child(page))
            .into_any_element()
    }

    /// The project's cards in three columns. Each column adds a card of its
    /// own; a card opens for editing, moves left or right, or goes away.
    fn render_board(&mut self, server: &Entity<ServerConnection>, project: ProjectId, tasks: &[Task], cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme().clone();
        let mut board = h_flex().id("board").gap_3().w_full().items_start().overflow_x_scrollbar();
        for (index, (status, label)) in COLUMNS.into_iter().enumerate() {
            let header = h_flex()
                .justify_between()
                .child(div().font_semibold().child(label))
                .child(
                    Button::new(ElementId::Name(format!("new-task-{index}").into()))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Plus)
                        .tooltip("New task")
                        .on_click(cx.listener(move |this, _, window, cx| this.edit_task(project, None, status, window, cx))),
                );
            let mut column =
                v_flex().gap_2().p_3().min_w(gpui_kit::rems(12.)).flex_1().rounded(theme.radius).bg(theme.muted).child(header);
            for task in tasks.iter().filter(|task| task.status == status) {
                let move_to = |step: isize| COLUMNS.get(index.wrapping_add_signed(step)).map(|(status, _)| *status);
                let mut buttons = h_flex().gap_0p5().justify_end();
                for (step, icon, tip) in [(-1, IconName::ChevronLeft, "Move left"), (1, IconName::ChevronRight, "Move right")] {
                    if let Some(to) = move_to(step) {
                        let (task, server) = (task.clone(), server.clone());
                        buttons = buttons.child(
                            Button::new(ElementId::Name(format!("task-{}-{step}", task.id).into()))
                                .ghost()
                                .xsmall()
                                .icon(icon)
                                .tooltip(tip)
                                .on_click(move |_, _, cx| {
                                    cx.stop_propagation();
                                    server.update(cx, |server, cx| server.move_task(&task, to, cx));
                                }),
                        );
                    }
                }
                let (deleted, server) = (task.id, server.clone());
                buttons = buttons.child(
                    Button::new(ElementId::Name(format!("task-{}-delete", task.id).into()))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Delete)
                        .tooltip("Delete task")
                        .on_click(move |_, _, cx| {
                            cx.stop_propagation();
                            server.update(cx, |server, cx| server.delete_task(project, deleted, cx));
                        }),
                );
                let edited = task.clone();
                column = column.child(
                    v_flex()
                        .id(ElementId::Name(format!("task-{}", task.id).into()))
                        .gap_1()
                        .p_3()
                        .rounded(theme.radius)
                        .bg(theme.background)
                        .cursor_pointer()
                        .hover(|style| style.bg(theme.accent))
                        .child(div().font_semibold().child(task.title.clone()))
                        .when(!task.markdown.is_empty(), |card| {
                            card.child(div().text_sm().text_color(theme.muted_foreground).line_clamp(4).child(task.markdown.clone()))
                        })
                        .child(buttons)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.edit_task(project, Some(edited.clone()), edited.status, window, cx)
                        })),
                );
            }
            board = board.child(column);
        }
        board
    }

    /// The dialog for a new card in `status`'s column, or for `task`.
    fn edit_task(&mut self, project: ProjectId, task: Option<Task>, status: TaskStatus, window: &mut Window, cx: &mut Context<Self>) {
        let Some(server) = self.server_of(project, cx) else { return };
        let form = cx.new(|cx| TaskForm::new(task.as_ref(), status, window, cx));
        let first = form.read(cx).first(cx);
        let (title, commit) = if task.is_some() { ("Edit task", "Save") } else { ("New task", "Create") };
        open_form(title, commit, form, Some(first), move |form, _, cx| {
            let input = match form.input(cx) {
                Ok(input) => input,
                Err(error) => {
                    form.error = Some(error.into());
                    cx.notify();
                    return false;
                }
            };
            let id = form.id;
            server.update(cx, |server, cx| {
                server.save_task(project, id, input, cx, |result, cx| {
                    if let Err(error) = result {
                        super::toast_error(format!("Could not save the task: {error}"), cx);
                    }
                })
            });
            true
        }, window, cx);
    }

    pub(super) fn new_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(server) = self.server.clone() else { return };
        let client = server.read(cx).client().cloned();
        let form = cx.new(|cx| ProjectForm::new(client, window, cx));
        let first = form.read(cx).first(cx);
        let shell = cx.entity().downgrade();
        open_form("New project", "Create", form, Some(first), move |form, _, cx| {
            let input = match form.project(cx) {
                Ok(input) => input,
                Err(error) => {
                    form.error = Some(error.into());
                    cx.notify();
                    return false;
                }
            };
            let shell = shell.clone();
            server.update(cx, |server, cx| {
                server.create_project(input, cx, move |result, cx| match result {
                    Ok(project) => {
                        let _ = shell.update(cx, |shell, cx| {
                            shell.go_root(Page::Projects, cx);
                            shell.go(Page::Project(project.id), cx);
                        });
                    }
                    Err(error) => super::toast_error(format!("Could not create the project: {error}"), cx),
                })
            });
            true
        }, window, cx);
    }

    fn delete_project(&mut self, id: ProjectId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(server) = self.server_of(id, cx) else { return };
        let shell = cx.entity().downgrade();
        let (name, adopted) = server
            .read(cx)
            .project(id)
            .map(|project| (project.name.clone(), project.path.is_some()))
            .unwrap_or_default();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let server = server.clone();
            let shell = shell.clone();
            alert
                .title(format!("Delete “{name}”?"))
                .description(if adopted {
                    "Its agents are stopped. The folder stays on the server: it was there before the project."
                } else {
                    "Its agents are stopped and its folder on the server is deleted."
                })
                .confirm()
                .ok_text("Delete")
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, _, cx| {
                    server.update(cx, |server, cx| server.delete_project(id, true, cx));
                    let _ = shell.update(cx, |shell, cx| shell.go_root(Page::Projects, cx));
                    true
                })
        });
    }
}

const COLUMNS: [(TaskStatus, &str); 3] = [(TaskStatus::Todo, "Todo"), (TaskStatus::InProgress, "In progress"), (TaskStatus::Done, "Done")];

pub fn section(title: &'static str, cx: &Context<Shell>) -> impl IntoElement {
    div().text_xs().font_semibold().text_color(cx.theme().muted_foreground).child(title)
}
