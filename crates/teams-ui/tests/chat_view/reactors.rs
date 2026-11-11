use std::sync::Arc;

use gpui::{Bounds, Entity, Modifiers, Pixels, VisualTestContext, point, px};
use teams_ui::ViewTrace;
use teams_ui::chat_view::ChatView;
use teams_ui::chat_view::reactors::{DELAY, GRACE};

use super::common::{LAB_A, LAB_B};
use super::{chat_window, draw, varied_chat};

fn bounds(cx: &mut VisualTestContext, selector: &str) -> Option<Bounds<Pixels>> {
    cx.debug_bounds(Box::leak(selector.to_owned().into_boxed_str()))
}

fn shown(view: &Entity<ChatView>, cx: &mut VisualTestContext) -> Option<(String, String)> {
    cx.update(|_, cx| {
        view.read(cx)
            .reactors_shown()
            .map(|(message, key)| (message.to_owned(), key.to_owned()))
    })
}

fn point_at(cx: &mut VisualTestContext, at: gpui::Point<Pixels>) {
    cx.simulate_mouse_move(at, None, Modifiers::none());
    draw(cx);
}

fn wait(cx: &mut VisualTestContext, how_long: std::time::Duration) {
    cx.executor().advance_clock(how_long);
    draw(cx);
}

#[test]
fn a_pill_under_the_pointer_shows_who_reacted_under_it_in_the_order_they_did() {
    let rig = varied_chat();
    let (view, cx) = chat_window(&rig, Arc::new(ViewTrace::off()));
    let like = bounds(cx, "reaction-like").unwrap();
    point_at(cx, like.center());
    assert!(bounds(cx, "reactors-like").is_none(), "not before the pointer rests");
    wait(cx, DELAY);
    assert_eq!(shown(&view, cx), Some(("101".to_owned(), "like".to_owned())));
    let card = bounds(cx, "reactors-like").unwrap();
    assert!(
        card.top() >= like.bottom() && card.left() <= like.left() && card.right() >= like.right(),
        "the card under its pill: {card:?} under {like:?}"
    );
    let first = bounds(cx, &format!("reactor-{LAB_B}")).unwrap();
    let then = bounds(cx, &format!("reactor-{LAB_A}")).unwrap();
    assert!(
        first.top() < then.top(),
        "b reacted first, then the account itself: {first:?}, {then:?}"
    );
    assert!(card.contains(&first.center()) && card.contains(&then.center()));
}

#[test]
fn the_card_stays_while_the_pointer_is_on_it_and_goes_once_it_left() {
    let rig = varied_chat();
    let (view, cx) = chat_window(&rig, Arc::new(ViewTrace::off()));
    let like = bounds(cx, "reaction-like").unwrap();
    point_at(cx, like.center());
    wait(cx, DELAY);
    let card = bounds(cx, "reactors-like").unwrap();
    point_at(cx, point(card.center().x, card.top() + px(2.0)));
    wait(cx, GRACE * 2);
    assert!(bounds(cx, "reactors-like").is_some(), "kept while the pointer is on it");
    point_at(cx, point(px(790.0), px(5.0)));
    assert!(bounds(cx, "reactors-like").is_some(), "time to come back");
    wait(cx, GRACE);
    assert_eq!(shown(&view, cx), None);
    assert!(bounds(cx, "reactors-like").is_none());
}

#[test]
fn a_shown_card_follows_the_pointer_to_the_next_pill_at_once_and_a_passing_one_shows_none() {
    let rig = varied_chat();
    let (view, cx) = chat_window(&rig, Arc::new(ViewTrace::off()));
    let like = bounds(cx, "reaction-like").unwrap();
    let heart = bounds(cx, "reaction-heart").unwrap();
    point_at(cx, like.center());
    wait(cx, DELAY);
    point_at(cx, heart.center());
    assert_eq!(shown(&view, cx), Some(("101".to_owned(), "heart".to_owned())));
    assert!(bounds(cx, "reactors-heart").is_some() && bounds(cx, "reactors-like").is_none());
    assert!(bounds(cx, &format!("reactor-{LAB_B}")).is_some());
    assert!(
        bounds(cx, &format!("reactor-{LAB_A}")).is_none(),
        "only who reacted with it"
    );
    point_at(cx, point(px(790.0), px(5.0)));
    wait(cx, GRACE);
    point_at(cx, like.center());
    point_at(cx, point(px(790.0), px(5.0)));
    wait(cx, DELAY);
    assert_eq!(shown(&view, cx), None, "the pointer passed over it without resting");
}

#[test]
fn the_account_itself_is_asked_for_by_name_as_teams_lists_it() {
    let rig = varied_chat();
    rig.core.hold_people.lock().unwrap().insert(LAB_A.to_owned());
    let (_view, _cx) = chat_window(&rig, Arc::new(ViewTrace::off()));
    let held: Vec<String> = rig
        .core
        .held
        .lock()
        .unwrap()
        .iter()
        .map(|(_, person)| person.mri.clone())
        .collect();
    assert!(held.contains(&LAB_A.to_owned()), "{held:?}");
}

#[test]
fn shown_on_request_at_once_and_none_for_a_key_nobody_set() {
    let rig = varied_chat();
    let (view, cx) = chat_window(&rig, Arc::new(ViewTrace::off()));
    view.update(cx, |view, cx| view.show_reactors(Some(("101", "heart")), cx));
    draw(cx);
    assert!(bounds(cx, "reactors-heart").is_some());
    view.update(cx, |view, cx| view.show_reactors(Some(("101", "laugh")), cx));
    draw(cx);
    assert!(bounds(cx, "reactors-laugh").is_none(), "no one reacted with it");
    view.update(cx, |view, cx| view.show_reactors(None, cx));
    draw(cx);
    assert_eq!(shown(&view, cx), None);
}
