//! A project's board: its tasks in columns by their state, made of the
//! design system's primitives. The columns share the width (none narrower
//! than 12 rem, past that the board scrolls sideways); for a finger a column
//! is a phone's width less a peek of the next. A card's commands are its
//! [`ItemActions`]: on hover and a right click, or behind a long press.

use std::rc::Rc;

use gpui_adaptive::prim::{Surface, column, row, scroll_x, surface};
use gpui_adaptive::{ActiveColors, ActiveScale, Command, Env, Handler, ItemActions, Text};
use gpui_kit::prelude::*;
use gpui_kit::{App, ElementId, RenderOnce, SharedString, Window, rems};

#[derive(IntoElement)]
pub struct Board {
    columns: Vec<BoardColumn>,
}

impl Board {
    pub fn new() -> Self {
        Board { columns: Vec::new() }
    }

    pub fn column(mut self, column: BoardColumn) -> Self {
        self.columns.push(column);
        self
    }
}

/// A column: its title, the command that adds a card to it, its cards.
pub struct BoardColumn {
    title: SharedString,
    adds: Command,
    cards: Vec<Card>,
}

impl BoardColumn {
    pub fn new(title: impl Into<SharedString>, adds: Command) -> Self {
        BoardColumn { title: title.into(), adds, cards: Vec::new() }
    }

    pub fn card(mut self, card: Card) -> Self {
        self.cards.push(card);
        self
    }
}

/// A task's card: its title and four lines of its text.
#[derive(IntoElement)]
pub struct Card {
    id: ElementId,
    title: SharedString,
    body: SharedString,
    on_activate: Handler,
    actions: ItemActions,
}

impl Card {
    pub fn new(
        id: impl Into<ElementId>,
        title: impl Into<SharedString>,
        body: impl Into<SharedString>,
        on_activate: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        Card { id: id.into(), title: title.into(), body: body.into(), on_activate: Rc::new(on_activate), actions: ItemActions::new() }
    }

    pub fn actions(mut self, actions: ItemActions) -> Self {
        self.actions = actions;
        self
    }
}

impl RenderOnce for Card {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let env = Env::of(window, cx);
        let (colors, scale) = (cx.colors(), cx.scale());
        let (hover, pressed) = (colors.hover, colors.pressed);
        let on_activate = self.on_activate.clone();
        let card = surface(Surface::Card, cx)
            .id(self.id)
            .gap(scale.space_xs)
            .p(scale.space_m)
            .cursor_pointer()
            .when(!env.is_touch(), |card| card.hover(move |style| style.bg(hover)))
            .active(move |style| style.bg(pressed))
            .child(Text::title(self.title))
            .when(!self.body.is_empty(), |card| card.child(Text::small(self.body).muted().clamp(4)))
            .on_click(move |_, window, cx| on_activate(window, cx));
        self.actions.attach(card, env, cx)
    }
}

impl RenderOnce for Board {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let env = Env::of(window, cx);
        let scale = cx.scale();
        let columns = self.columns.into_iter().map(|column_of| {
            let head = row()
                .justify_between()
                .min_h(scale.target)
                .child(Text::title(column_of.title))
                .child(column_of.adds.icon_button(env));
            surface(Surface::Panel, cx)
                .gap(scale.space_s)
                .p(scale.space_m)
                .when(env.is_touch(), |panel| panel.flex_none().w(rems(17.)))
                .when(!env.is_touch(), |panel| panel.flex_1().min_w(rems(12.)))
                .child(head)
                .children(column_of.cards)
        });
        let board = row().items_start().gap(scale.space_m).w_full().children(columns);
        column().w_full().child(scroll_x("board", cx, board))
    }
}
