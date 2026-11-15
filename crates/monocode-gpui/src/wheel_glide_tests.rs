//! GPUI Fast applies wheel notches and trackpad pixels immediately, with
//! no glide animation; both land at once.
use gpui_kit::{
    Context, IntoElement, ListAlignment, ListState, Render, ScrollDelta, ScrollEvent,
    TestAppContext, Window, div, list, point, prelude::*, px, size,
};

struct Rows(ListState);
impl Render for Rows {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        list(self.0.clone(), |_, _, _| {
            div().h(px(20.)).w_full().into_any()
        })
        .w_full()
        .h_full()
    }
}

#[gpui_kit::test]
fn wheel_notches_glide_and_trackpad_pixels_land_at_once(cx: &mut TestAppContext) {
    let cx = cx.add_empty_window();
    // Know the full scroll range before asserting an exact pixel distance.
    let state = ListState::new(50, ListAlignment::Top, px(10.)).measure_all();
    cx.draw(point(px(0.), px(0.)), size(px(100.), px(100.)), |_, cx| {
        cx.new(|_| Rows(state.clone())).into_any_element()
    });
    // Three notches at 20px each land three 20px rows down at once.
    cx.simulate_event(ScrollEvent {
        position: point(px(1.), px(1.)),
        delta: ScrollDelta::Lines(point(0., -3.)),
        ..Default::default()
    });
    assert_eq!(state.logical_scroll_top().item_ix, 3);
    assert_eq!(state.logical_scroll_top().offset_in_item, px(0.));
    // The harness paints once, so the next wheel event still measures from
    // the original snapshot: -40px lands two rows down from the top.
    cx.simulate_event(ScrollEvent {
        position: point(px(1.), px(1.)),
        delta: ScrollDelta::Pixels(point(px(0.), px(-40.))),
        ..Default::default()
    });
    assert_eq!(state.logical_scroll_top().item_ix, 2);
}
