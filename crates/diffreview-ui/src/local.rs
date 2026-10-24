//! A [`ReviewBackend`] over git on this machine: the standalone tool's.

use std::path::PathBuf;

use diffreview::git::LocalRepo;
use diffreview::{DiffRequest, Refs};
use gpui_kit::{App, AppContext as _};

use crate::{Done, ReviewBackend};

/// The repository the directory is in. git runs on GPUI's background
/// executor; the answer comes back on the main thread.
pub struct LocalBackend {
    dir: PathBuf,
}

impl LocalBackend {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }
}

impl ReviewBackend for LocalBackend {
    fn diff(&self, request: DiffRequest, cx: &mut App, done: Done<String>) {
        let dir = self.dir.clone();
        let task = cx.background_spawn(async move { LocalRepo::open(&dir)?.diff(&request) });
        cx.spawn(async move |cx| {
            let result = task.await;
            cx.update(|cx| done(result, cx));
        })
        .detach();
    }

    fn refs(&self, cx: &mut App, done: Done<Refs>) {
        let dir = self.dir.clone();
        let task = cx.background_spawn(async move { LocalRepo::open(&dir)?.refs() });
        cx.spawn(async move |cx| {
            let result = task.await;
            cx.update(|cx| done(result, cx));
        })
        .detach();
    }
}
