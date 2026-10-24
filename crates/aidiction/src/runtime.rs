//! Bridge between GPUI's main thread and Tokio, after matterfast's.
//!
//! Entities and windows are not `Send`; network futures run on Tokio. The
//! future runs on a Tokio worker, its result comes back over a channel, and
//! the callback gets a fresh `&mut App` on the main thread. Nothing on a
//! Tokio task ever touches application state.

use std::cell::RefCell;
use std::future::Future;
use std::sync::OnceLock;
use std::time::Duration;

use gpui_kit::{App, AsyncApp};
use tokio::runtime::Runtime;

static RUNTIME: OnceLock<Runtime> = OnceLock::new();

thread_local! {
    static APP: RefCell<Option<AsyncApp>> = const { RefCell::new(None) };
}

/// Makes the application reachable from [`spawn`] and friends. Call once.
pub fn install(cx: &mut App) {
    APP.with_borrow_mut(|app| *app = Some(cx.to_async()));
}

fn app() -> AsyncApp {
    APP.with_borrow(|app| app.clone()).expect("runtime::install runs before anything is spawned")
}

pub fn runtime() -> &'static Runtime {
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("aidiction-io")
            .build()
            .expect("failed to start the Tokio runtime")
    })
}

/// Runs `future` on Tokio and `on_done` with its result on the main thread.
pub fn spawn<T, F, C>(future: F, on_done: C)
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
    C: FnOnce(T, &mut App) + 'static,
{
    let (tx, rx) = async_channel::bounded(1);
    runtime().spawn(async move {
        let _ = tx.send(future.await).await;
    });
    let app = app();
    app.foreground_executor()
        .spawn({
            let app = app.clone();
            async move {
                if let Ok(value) = rx.recv().await {
                    app.update(|cx| on_done(value, cx));
                }
            }
        })
        .detach();
}

/// Drains a Tokio channel on the main thread. `on_item` answers whether to
/// keep going; `on_end` runs once the channel closes (not after a `false`).
pub fn receive<T: Send + 'static>(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<T>,
    mut on_item: impl FnMut(T, &mut App) -> bool + 'static,
    on_end: impl FnOnce(&mut App) + 'static,
) {
    let (tx, items) = async_channel::unbounded();
    runtime().spawn(async move {
        while let Some(item) = rx.recv().await {
            if tx.send(item).await.is_err() {
                break;
            }
        }
    });
    let app = app();
    app.foreground_executor()
        .spawn({
            let app = app.clone();
            async move {
                while let Ok(item) = items.recv().await {
                    if !app.update(|cx| on_item(item, cx)) {
                        return;
                    }
                }
                app.update(on_end);
            }
        })
        .detach();
}

/// Calls `f` on the main thread once `delay` has passed.
pub fn after(delay: Duration, f: impl FnOnce(&mut App) + 'static) {
    let app = app();
    let timer = app.background_executor().timer(delay);
    app.foreground_executor()
        .spawn({
            let app = app.clone();
            async move {
                timer.await;
                app.update(f);
            }
        })
        .detach();
}
