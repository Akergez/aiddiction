//! The review page of a project, and what its "Send to agent" does: the
//! annotations go to the project's conversation as a prompt, and the review
//! starts over — revdiff's loop of annotate, fix, review again.

use std::rc::Rc;

use aiddiction_api::ProjectId;
use diffreview::DiffRequest;
use diffreview_ui::{ReviewEvent, ReviewOptions, ReviewView};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, AppContext, Context, Entity, Window, div};

use super::shell::{Page, Shell};
use crate::review::ServerBackend;

impl Shell {
    /// The project's review, made on first use and kept while the server
    /// is, so annotations survive going elsewhere and back.
    pub(super) fn open_review(&mut self, project: ProjectId, window: &mut Window, cx: &mut Context<Self>) {
        if !self.reviews.contains_key(&project) {
            let Some(client) = self.server.as_ref().and_then(|server| server.read(cx).client().cloned()) else { return };
            let backend = Rc::new(ServerBackend::new(client, project));
            let options = ReviewOptions { submit_label: "Send to agent".into(), discard_label: None };
            let view = cx.new(|cx| ReviewView::new(backend, DiffRequest::default(), options, window, cx));
            let subscription = cx.subscribe_in(&view, window, move |this, view, event: &ReviewEvent, window, cx| {
                if let ReviewEvent::Submit(output) = event {
                    this.send_review(project, view.clone(), output.clone(), window, cx);
                }
            });
            self.reviews.insert(project, (view, subscription));
        } else if let Some((view, _)) = self.reviews.get(&project) {
            // Back to a review: what is on disk may have changed meanwhile.
            view.update(cx, |view, cx| view.load(cx));
        }
        if let Some((view, _)) = self.reviews.get(&project) {
            view.read(cx).focus_handle().focus(window, cx);
        }
        self.go(Page::Review(project), cx);
    }

    pub(super) fn render_review(&self, project: ProjectId) -> AnyElement {
        match self.reviews.get(&project) {
            Some((view, _)) => view.clone().into_any_element(),
            None => div().into_any_element(),
        }
    }

    /// Hands the annotations to the conversation the review was opened from,
    /// else the project's conversation that was opened last and is ready for
    /// a prompt.
    fn send_review(&mut self, project: ProjectId, view: Entity<ReviewView>, output: String, window: &mut Window, cx: &mut Context<Self>) {
        if output.is_empty() {
            super::toast_error("Annotate a line first: there is nothing to send.", cx);
            return;
        }
        let ready = |chat: &&Entity<super::chat::ChatView>| {
            let chat = chat.read(cx);
            let session = chat.session.read(cx);
            chat.project_id == project && session.is_ready() && !session.busy
        };
        let from = self.review_from.get(&project).and_then(|id| self.chat(*id)).filter(ready);
        let chat = from.or_else(|| self.chats.iter().rev().find(ready));
        let Some(chat) = chat.cloned() else {
            super::toast_error(
                "Open a conversation with an agent in this project first (and let it finish its turn); the annotations stay here.",
                cx,
            );
            return;
        };
        let prompt = diffreview::prompt::for_agent(&output);
        chat.read(cx).session.clone().update(cx, |session, cx| session.prompt(prompt, cx));
        view.update(cx, |view, cx| view.set_annotations(diffreview::Store::default(), cx));
        self.go(Page::Chat(chat.entity_id()), cx);
        chat.read(cx).composer_focus(cx).focus(window, cx);
    }
}
