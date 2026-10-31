//! A scripted pair of hands, after money-manager-ng's: a Wayland session
//! gives a test no other way to press a button and see what follows.
//!
//! `AIDDICTION_SCRIPT="wait:1500;click:120,80;type:hello;key:enter;shot:chat;quit"`
//! plays those steps into the window once it opens. Steps:
//!
//! - `wait:<ms>`, `click:<x>,<y>`, `key:<keystroke>`, `type:<text>`
//! - `swipe:<x>,<y>,<dx>,<dy>` — a finger from (x, y) moved by (dx, dy) and
//!   lifted, as a phone's touches: gpui makes it a pan of whatever is under it
//! - `hold:<x>,<y>,<dx>,<dy>` — the same finger, left down where it got to;
//!   `lift` lifts it there
//! - `shot:<name>` — runs `$AIDDICTION_SHOT_CMD <name>`
//! - `expect:<what>=<value>` — ends the process with status 1 unless it
//!   holds: `page=inbox|projects|project|chat|review|settings|appearance|agents|servers`,
//!   `projects=<n>`, `chats=<n>`, `sessions=<n>` (in the inbox), `waiting=<n>`
//!   (conversations that need the person),
//!   `agents=<n>`, `tasks=<n>` (cards on every board), `annotations=<n>` (of the review on screen),
//!   `entries=<n>` (of the conversation on screen), `busy=yes|no`,
//!   `permissions=<n>`, `sign-in=none|choose|terminal|key|agent` (the card
//!   for signing the agent in, and the way being tried), `dialog=open|closed`,
//!   `theme=light|dark`
//! - `until:<what>=<value>` — waits (up to 15 s) until it holds, else fails
//! - `quit`

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use gpui_kit::component::WindowExt as _;
use gpui_kit::{
    AnyWindowHandle, App, Entity, Keystroke, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    PlatformInput, TouchEvent, TouchId, TouchPhase, Window, point, px,
};

use crate::ui::shell::{Page, Shell};

#[derive(Debug, Clone, PartialEq)]
enum Step {
    Wait(u64),
    Click(f32, f32),
    Swipe(f32, f32, f32, f32),
    Hold(f32, f32, f32, f32),
    Lift,
    Key(String),
    Type(String),
    Shot(String),
    Expect(String, String),
    Until(String, String, Option<Instant>),
    Quit,
}

const UNTIL_AT_MOST: Duration = Duration::from_secs(15);

fn parse(script: &str) -> Vec<Step> {
    script
        .split(';')
        .filter_map(|step| {
            let step = step.trim();
            let (verb, rest) = step.split_once(':').unwrap_or((step, ""));
            let pair = || rest.split_once('=').map(|(what, value)| (what.trim().to_owned(), value.trim().to_owned()));
            Some(match verb {
                "wait" => Step::Wait(rest.parse().ok()?),
                "click" => {
                    let (x, y) = rest.split_once(',')?;
                    Step::Click(x.trim().parse().ok()?, y.trim().parse().ok()?)
                }
                "swipe" | "hold" => {
                    let numbers: Vec<f32> = rest.split(',').map(|number| number.trim().parse().ok()).collect::<Option<_>>()?;
                    let [x, y, dx, dy] = numbers[..] else { return None };
                    if verb == "swipe" { Step::Swipe(x, y, dx, dy) } else { Step::Hold(x, y, dx, dy) }
                }
                "lift" => Step::Lift,
                "key" => Step::Key(rest.to_owned()),
                "type" => Step::Type(rest.to_owned()),
                "shot" => Step::Shot(rest.to_owned()),
                "expect" => {
                    let (what, value) = pair()?;
                    Step::Expect(what, value)
                }
                "until" => {
                    let (what, value) = pair()?;
                    Step::Until(what, value, None)
                }
                "quit" => Step::Quit,
                _ => {
                    tracing::warn!(step, "unknown script step");
                    return None;
                }
            })
        })
        .collect()
}

pub fn play(window: AnyWindowHandle, shell: Entity<Shell>, cx: &mut App) {
    let Ok(script) = std::env::var("AIDDICTION_SCRIPT") else { return };
    next(window, shell, parse(&script).into(), cx);
}

fn observe(what: &str, shell: &Entity<Shell>, window: &mut Window, cx: &mut App) -> Option<String> {
    let shell = shell.read(cx);
    let page = shell.page(cx);
    let chat = match page {
        Page::Chat(id) => shell.chat(id).map(|chat| chat.read(cx).session.read(cx)),
        _ => None,
    };
    Some(match what {
        "page" => match page {
            Page::Inbox => "inbox",
            Page::Projects => "projects",
            Page::Project(_) => "project",
            Page::Review(_) => "review",
            Page::Chat(_) => "chat",
            Page::Settings => "settings",
            Page::Appearance => "appearance",
            Page::Agents => "agents",
            Page::Servers => "servers",
            Page::Pick(_) => "pick",
        }
        .to_owned(),
        "sessions" => shell.listed(cx).len().to_string(),
        "waiting" => shell.listed(cx).iter().filter(|listed| listed.group == crate::inbox::Group::NeedsYou).count().to_string(),
        "projects" => shell.server.as_ref().map_or(0, |server| server.read(cx).projects.len()).to_string(),
        "chats" => shell.chats.len().to_string(),
        "annotations" => match page {
            Page::Review(project) => shell.reviews.get(&project).map_or(0, |(view, _)| view.read(cx).annotations().count()),
            _ => 0,
        }
        .to_string(),
        "agents" => shell.server.as_ref().map_or(0, |server| server.read(cx).agents.len()).to_string(),
        "tasks" => shell.server.as_ref().map_or(0, |server| server.read(cx).tasks.values().map(Vec::len).sum::<usize>()).to_string(),
        "entries" => chat?.thread.entries.len().to_string(),
        "busy" => if chat?.busy { "yes" } else { "no" }.to_owned(),
        "permissions" => chat?.thread.permissions.len().to_string(),
        "sign-in" => {
            let chat = chat?;
            match &chat.sign_in.method {
                _ if !chat.sign_in.shown(&chat.phase) => "none",
                None => "choose",
                Some(method) => match method.kind {
                    crate::login::MethodKind::Terminal => "terminal",
                    crate::login::MethodKind::ApiKey => "key",
                    crate::login::MethodKind::Agent => "agent",
                },
            }
            .to_owned()
        }
        // A form is a dialog on a pointer and a sheet on a finger.
        "dialog" => {
            if window.has_active_dialog(cx) || gpui_adaptive::Sheet::is_open(window, cx) { "open" } else { "closed" }.to_owned()
        }
        #[allow(clippy::disallowed_methods, reason = "the scenario asks which mode the theme is in, not for a colour")]
        "theme" => if gpui_kit::component::ActiveTheme::theme(cx).mode.is_dark() { "dark" } else { "light" }.to_owned(),
        _ => return None,
    })
}

fn next(handle: AnyWindowHandle, shell: Entity<Shell>, mut steps: VecDeque<Step>, cx: &mut App) {
    let Some(step) = steps.pop_front() else { return };
    let mut pause = 120;
    tracing::info!(?step, "script");
    let _ = handle.update(cx, |_, window, cx| match &step {
        Step::Wait(ms) => pause = *ms,
        Step::Click(x, y) => {
            let position = point(px(*x), px(*y));
            window.dispatch_event(
                PlatformInput::MouseMove(MouseMoveEvent { position, pressed_button: None, modifiers: Modifiers::default() }),
                cx,
            );
            window.dispatch_event(
                PlatformInput::MouseDown(MouseDownEvent {
                    button: MouseButton::Left,
                    position,
                    modifiers: Modifiers::default(),
                    click_count: 1,
                    first_mouse: false,
                }),
                cx,
            );
            window.dispatch_event(
                PlatformInput::MouseUp(MouseUpEvent {
                    button: MouseButton::Left,
                    position,
                    modifiers: Modifiers::default(),
                    click_count: 1,
                }),
                cx,
            );
        }
        Step::Swipe(x, y, dx, dy) | Step::Hold(x, y, dx, dy) => {
            let (x, y, dx, dy) = (*x, *y, *dx, *dy);
            const MOVES: u16 = 10;
            let lifted = matches!(step, Step::Swipe(..));
            for step in 0..=MOVES + u16::from(lifted) {
                let (phase, done) = match step {
                    0 => (TouchPhase::Started, 0.),
                    step if step <= MOVES => (TouchPhase::Moved, f32::from(step) / f32::from(MOVES)),
                    _ => (TouchPhase::Ended, 1.),
                };
                let position = point(px(x + dx * done), px(y + dy * done));
                touch(phase, position, window, cx);
            }
        }
        Step::Lift => {
            if let Some(position) = FINGER.get() {
                touch(TouchPhase::Ended, position, window, cx);
            }
        }
        Step::Key(key) => match Keystroke::parse(key) {
            Ok(keystroke) => {
                window.dispatch_keystroke(keystroke, cx);
            }
            Err(error) => tracing::warn!(%error, key, "not a keystroke"),
        },
        Step::Type(text) => {
            for letter in text.chars() {
                let keystroke = Keystroke {
                    modifiers: Modifiers::default(),
                    key: letter.to_string(),
                    key_char: Some(letter.to_string()),
                };
                window.dispatch_keystroke(keystroke, cx);
            }
        }
        Step::Shot(name) => {
            if let Ok(command) = std::env::var("AIDDICTION_SHOT_CMD") {
                let _ = std::process::Command::new(command).arg(name).status();
            }
        }
        Step::Expect(what, value) => {
            let found = observe(what, &shell, window, cx);
            if found.as_deref() != Some(value) {
                eprintln!("script: expected {what}={value}, found {found:?}");
                std::process::exit(1);
            }
        }
        Step::Until(what, value, since) => {
            let since = since.unwrap_or_else(Instant::now);
            let found = observe(what, &shell, window, cx);
            if found.as_deref() != Some(value) {
                if since.elapsed() > UNTIL_AT_MOST {
                    eprintln!("script: waited for {what}={value}, found {found:?}");
                    std::process::exit(1);
                }
                steps.push_front(Step::Until(what.clone(), value.clone(), Some(since)));
                pause = 100;
            }
        }
        Step::Quit => cx.quit(),
    });
    cx.spawn(async move |cx| {
        cx.background_executor().timer(Duration::from_millis(pause)).await;
        cx.update(|cx| next(handle, shell, steps, cx));
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_script_is_steps_between_semicolons() {
        assert_eq!(
            parse("wait:200; click:10,20 ;swipe:1,2,-3,0;swipe:1,2;key:enter;type:a:b;until:page=chat;quit;bogus"),
            [
                Step::Wait(200),
                Step::Click(10.0, 20.0),
                Step::Swipe(1.0, 2.0, -3.0, 0.0),
                Step::Key("enter".into()),
                Step::Type("a:b".into()),
                Step::Until("page".into(), "chat".into(), None),
                Step::Quit,
            ]
        );
    }
}

thread_local! {
    /// Where the finger is while it is down.
    static FINGER: std::cell::Cell<Option<gpui_kit::Point<gpui_kit::Pixels>>> = const { std::cell::Cell::new(None) };
}

fn touch(phase: TouchPhase, position: gpui_kit::Point<gpui_kit::Pixels>, window: &mut Window, cx: &mut App) {
    FINGER.set((phase != TouchPhase::Ended).then_some(position));
    let touch = TouchEvent { id: TouchId(1), phase, position, predicted_position: None, force: None };
    window.dispatch_event(PlatformInput::Touch(touch), cx);
}
