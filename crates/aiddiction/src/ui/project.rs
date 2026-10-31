//! A project's page: where it came from, the agents running in it, and its
//! conversations. New conversations and the review start here too.

use aiddiction_api::{InstanceState, ProjectId, ProjectState, Task, TaskStatus};
use gpui_adaptive::prim::column;
use gpui_adaptive::{
    ActiveScale, Banner, Busy, Code, Collection, Command, Confirm, Empty, Env, Icon, IconName, ItemActions, ListRow, Note, Screen,
    SectionHeader, Signal, Stack, Tone,
};
use gpui_kit::prelude::*;
use gpui_kit::{AppContext, Context, Entity, SharedString, Window};

use super::board::{Board, BoardColumn, Card};
use super::forms::{ProjectForm, TaskForm, open_form};
use super::shell::{Page, Shell};
use crate::server::ServerConnection;

impl Shell {
    pub(super) fn render_project(&mut self, id: ProjectId, window: &mut Window, cx: &mut Context<Self>) -> Screen {
        let env = Env::of(window, cx);
        let gone = || Screen::new("project-gone", "").scrolls(false).content(Empty::new(IconName::Folder, "This project is gone."));
        let Some(server) = self.server_of(id, cx) else { return gone() };
        let (project, instances, tasks) = {
            let server = server.read(cx);
            let Some(project) = server.project(id).cloned() else { return gone() };
            (project, server.instances.get(&id).cloned().unwrap_or_default(), server.tasks.get(&id).cloned().unwrap_or_default())
        };
        let ready = project.state == ProjectState::Ready;

        let shell = cx.entity().downgrade();
        let screen = Screen::new("project", project.name.clone())
            .subtitle(super::inbox::origin(&project))
            .when(ready, |screen| {
                let (review, start) = (shell.clone(), shell.clone());
                screen
                    .command(Command::new("review", "Review changes").icon(IconName::FileText).on_trigger(move |window, cx| {
                        review.update(cx, |shell, cx| shell.open_review(id, window, cx)).ok();
                    }))
                    .command(
                        Command::new("project-new-chat", "New conversation").icon(IconName::Plus).primary().on_trigger(
                            move |window, cx| {
                                start.update(cx, |shell, cx| shell.new_conversation(window, cx)).ok();
                            },
                        ),
                    )
            })
            .command(Command::new("delete-project", "Delete project").icon(IconName::Delete).destructive().on_trigger(
                move |window, cx| {
                    shell.update(cx, |shell, cx| shell.delete_project(id, window, cx)).ok();
                },
            ));

        let mut page = Stack::new();

        match project.state {
            ProjectState::Cloning => {
                page = page.child(Busy::new("cloning").label(project.status_message.clone().unwrap_or_else(|| "Cloning…".into())));
            }
            ProjectState::Failed => {
                return screen.banner(Some(
                    Banner::new("failed", project.status_message.clone().unwrap_or_else(|| "The project could not be made.".into()))
                        .tone(Tone::Danger)
                        .icon(IconName::CircleX),
                ));
            }
            ProjectState::Ready => {
                if !instances.is_empty() {
                    let mut failures = Vec::new();
                    let rows: Vec<ListRow> = instances
                        .iter()
                        .map(|instance| {
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
                            let signal = match instance.state {
                                InstanceState::Starting => Some(Signal::Busy),
                                InstanceState::Failed => Some(Signal::Waiting(Tone::Danger)),
                                InstanceState::Ready | InstanceState::Exited => None,
                            };
                            let (stopped, stop_server) = (instance.clone(), server.clone());
                            let stop = Command::new(SharedString::from(format!("stop-{}", instance.id)), if alive { "Stop the agent" } else { "Dismiss" })
                                .icon(if alive { IconName::Pause } else { IconName::Close })
                                .on_trigger(move |_, cx| stop_server.update(cx, |server, cx| server.stop_instance(&stopped, cx)));
                            ListRow::new(SharedString::from(format!("instance-{}", instance.id)), instance.agent_name.clone())
                                .leading(Icon::new(IconName::Bot))
                                .subtitle(state)
                                .signal(signal)
                                .muted(!alive)
                                .trailing(stop.icon_button(env))
                        })
                        .collect();
                    for instance in instances.iter().filter(|instance| {
                        !matches!(instance.state, InstanceState::Starting | InstanceState::Ready) && !instance.stderr_tail.is_empty()
                    }) {
                        let tail = instance.stderr_tail.iter().rev().take(6).rev().cloned().collect::<Vec<_>>().join("\n");
                        let message = instance.message.clone().unwrap_or_else(|| "stopped".into());
                        failures.push(
                            column()
                                .gap(cx.scale().space_xs)
                                .child(Note::new(format!("{}: {message}", instance.agent_name)))
                                .child(Code::new(SharedString::from(format!("stderr-{}", instance.id)), tail)),
                        );
                    }
                    page = page.child(
                        column()
                            .gap(cx.scale().space_s)
                            .child(Collection::new("instances").section(SectionHeader::new("Agents"), rows))
                            .children(failures),
                    );
                }

                let listed: Vec<_> = self.listed(cx).into_iter().filter(|item| item.project.id == id).collect();
                page = page.child(
                    column().gap(cx.scale().space_s).child(SectionHeader::new("Tasks")).child(self.render_board(&server, id, &tasks, cx)),
                );
                let conversations = Collection::new("project-conversations");
                let conversations = if listed.is_empty() {
                    conversations
                        .child(SectionHeader::new("Conversations"))
                        .child(Note::new("No conversations here yet. Start one: the agent works in this project's folder."))
                } else {
                    conversations.section(SectionHeader::new("Conversations"), listed.iter().map(|item| self.session_row(item, false, cx)))
                };
                page = page.child(conversations);
            }
        }

        screen.content(page)
    }

    /// The project's cards in three columns. Each column adds a card of its
    /// own; a card opens for editing, and its commands move it to another
    /// column or delete it (on hover and a right click for a pointer, a long
    /// press for a finger).
    fn render_board(&mut self, server: &Entity<ServerConnection>, project: ProjectId, tasks: &[Task], cx: &mut Context<Self>) -> Board {
        let mut board = Board::new();
        for (index, (status, label)) in COLUMNS.into_iter().enumerate() {
            let shell = cx.entity().downgrade();
            let adds = Command::new(SharedString::from(format!("new-task-{index}")), "New task").icon(IconName::Plus).on_trigger(
                move |window, cx| {
                    shell.update(cx, |shell, cx| shell.edit_task(project, None, status, window, cx)).ok();
                },
            );
            let mut column = BoardColumn::new(label, adds);
            for task in tasks.iter().filter(|task| task.status == status) {
                let mut actions = ItemActions::new().title(task.title.clone());
                for (step, icon) in [(-1, IconName::ChevronLeft), (1, IconName::ChevronRight)] {
                    if let Some((to, name)) = COLUMNS.get(index.wrapping_add_signed(step)).copied() {
                        let (task, server) = (task.clone(), server.clone());
                        actions = actions.quick(
                            Command::new(SharedString::from(format!("task-{}-{step}", task.id)), format!("Move to {name}"))
                                .icon(icon)
                                .on_trigger(move |_, cx| server.update(cx, |server, cx| server.move_task(&task, to, cx))),
                        );
                    }
                }
                let (deleted, server) = (task.id, server.clone());
                actions = actions.quick(
                    Command::new(SharedString::from(format!("task-{}-delete", task.id)), "Delete task")
                        .icon(IconName::Delete)
                        .destructive()
                        .on_trigger(move |_, cx| server.update(cx, |server, cx| server.delete_task(project, deleted, cx))),
                );
                let (edited, shell) = (task.clone(), cx.entity().downgrade());
                let card = Card::new(SharedString::from(format!("task-{}", task.id)), task.title.clone(), task.markdown.clone(), move |window, cx| {
                    shell.update(cx, |shell, cx| shell.edit_task(project, Some(edited.clone()), edited.status, window, cx)).ok();
                });
                column = column.card(card.actions(actions));
            }
            board = board.column(column);
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
            let Some(input) = form.input(cx) else { return false };
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
            let Some(input) = form.project(cx) else { return false };
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
        Confirm::new("delete-project-confirm", format!("Delete “{name}”?"))
            .body(if adopted {
                "Its agents are stopped. The folder stays on the server: it was there before the project."
            } else {
                "Its agents are stopped and its folder on the server is deleted."
            })
            .confirm(Command::new("delete-project-yes", "Delete").destructive().on_trigger(move |_, cx| {
                server.update(cx, |server, cx| server.delete_project(id, true, cx));
                shell.update(cx, |shell, cx| shell.go_root(Page::Projects, cx)).ok();
            }))
            .open(window, cx);
    }
}

const COLUMNS: [(TaskStatus, &str); 3] = [(TaskStatus::Todo, "Todo"), (TaskStatus::InProgress, "In progress"), (TaskStatus::Done, "Done")];
