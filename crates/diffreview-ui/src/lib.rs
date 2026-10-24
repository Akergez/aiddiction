//! The diff review view for gpui-kit applications, after revdiff: the changed
//! files as a tree on one side, syntax-highlighted diffs the diff of one on the other, and annotations on its
//! lines, hunks and on the whole file, written out in revdiff's format.
//!
//! The view does not know where the repository is. The application hands it
//! a [`ReviewBackend`] — git on this machine, or a server's REST API — and
//! decides what [`ReviewEvent::Submit`] means: print and quit for a
//! standalone tool, send to an agent for Aiddiction.
//!
//! Call [`init`] once, after `gpui_kit::init`, for the key bindings.

mod editor;
mod highlight;
mod local;
mod rows;
mod view;

use diffreview::{DiffRequest, Refs};
use gpui_kit::{App, KeyBinding, SharedString};

pub use local::LocalBackend;
pub use view::ReviewView;

gpui_kit::actions!(
    diffreview,
    [
        CursorDown,
        CursorUp,
        PageDown,
        PageUp,
        NextFile,
        PreviousFile,
        NextHunk,
        PreviousHunk,
        Annotate,
        AnnotateFile,
        DeleteAnnotation,
        ToggleReviewed,
        Reload,
        Submit
    ]
);

/// The key context the view's bindings live in.
pub const CONTEXT: &str = "DiffReview";

/// revdiff's keys, where they mean the same thing.
pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("j", CursorDown, Some(CONTEXT)),
        KeyBinding::new("down", CursorDown, Some(CONTEXT)),
        KeyBinding::new("k", CursorUp, Some(CONTEXT)),
        KeyBinding::new("up", CursorUp, Some(CONTEXT)),
        KeyBinding::new("pagedown", PageDown, Some(CONTEXT)),
        KeyBinding::new("ctrl-d", PageDown, Some(CONTEXT)),
        KeyBinding::new("pageup", PageUp, Some(CONTEXT)),
        KeyBinding::new("ctrl-u", PageUp, Some(CONTEXT)),
        KeyBinding::new("n", NextFile, Some(CONTEXT)),
        KeyBinding::new("p", PreviousFile, Some(CONTEXT)),
        KeyBinding::new("]", NextHunk, Some(CONTEXT)),
        KeyBinding::new("[", PreviousHunk, Some(CONTEXT)),
        KeyBinding::new("a", Annotate, Some(CONTEXT)),
        KeyBinding::new("enter", Annotate, Some(CONTEXT)),
        KeyBinding::new("shift-a", AnnotateFile, Some(CONTEXT)),
        KeyBinding::new("d", DeleteAnnotation, Some(CONTEXT)),
        KeyBinding::new("space", ToggleReviewed, Some(CONTEXT)),
        KeyBinding::new("shift-r", Reload, Some(CONTEXT)),
        KeyBinding::new("ctrl-enter", Submit, Some(CONTEXT)),
    ]);
}

/// Answers with a result, on the main thread, whenever it has one.
pub type Done<T> = Box<dyn FnOnce(Result<T, String>, &mut App)>;

/// Where the diffs come from. Each call answers through `done`, on the main
/// thread; how it gets there (a background thread, a Tokio task, a request
/// to a server) is the application's business.
pub trait ReviewBackend: 'static {
    /// The unified diff `request` asks for.
    fn diff(&self, request: DiffRequest, cx: &mut App, done: Done<String>);
    /// Branches and recent commits, for the source picker.
    fn refs(&self, cx: &mut App, done: Done<Refs>);
}

/// What the view asks of the application that holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewEvent {
    /// The review is done: the annotations, in revdiff's format (empty when
    /// there are none).
    Submit(String),
    /// Leave without the annotations.
    Discard,
}

/// What the application calls its two ways out.
#[derive(Debug, Clone)]
pub struct ReviewOptions {
    pub submit_label: SharedString,
    /// `None`: no discard button (the application has its own way back).
    pub discard_label: Option<SharedString>,
}

impl Default for ReviewOptions {
    fn default() -> Self {
        Self { submit_label: "Done".into(), discard_label: None }
    }
}
