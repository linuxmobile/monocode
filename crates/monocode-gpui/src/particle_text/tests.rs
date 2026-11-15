use super::*;
use gpui::{Context, Render, TestAppContext, size};

#[test]
fn particles_wait_for_the_edge_then_drift_shrink_and_expire() {
    assert_eq!(edge_at(0., 200.), -18.);
    assert_eq!(edge_at(0.55, 200.), 100.);
    assert_eq!(edge_at(1.1, 200.), 218.);
    assert_eq!(edge_at(3., 200.), 218.);
    assert!((edge_at(0.275, 200.) - (-3.25)).abs() < 0.001);
    let mut particle = Particle {
        x: 40.,
        y: 9.,
        alpha: 0.5,
        vx: 12.,
        vy: -8.,
        life: 0.85,
        born: None,
    };
    assert_eq!(particle.frame(0.3, 39.), (true, None));
    assert!(particle.born.is_none());
    let (alive, first) = particle.frame(0.4, 40.);
    assert!(alive);
    let (bounds, alpha) = first.unwrap();
    assert_eq!(bounds.origin, point(px(40.), px(9.)));
    assert_eq!(bounds.size, size(px(1.8), px(1.8)));
    assert!((alpha - 0.6).abs() < 0.0001);
    let (_, middle) = particle.frame(0.825, 200.);
    let (bounds, alpha) = middle.unwrap();
    assert_eq!(bounds.origin, point(px(50.5), px(2.)));
    assert!((f32::from(bounds.size.width) - 1.35).abs() < 0.001);
    assert!((alpha - 0.6 * 0.5_f32.powf(1.2)).abs() < 0.001);
    assert_eq!(particle.frame(1.26, 200.), (false, None));
}

#[gpui::test]
fn interruption_uses_the_last_target_and_completion_discards_all_effects(cx: &mut TestAppContext) {
    use gpui::canvas;
    use std::{cell::RefCell, rc::Rc, time::Duration};
    struct Harness(Rc<RefCell<Option<()>>>);
    impl Render for Harness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let checked = self.0.clone();
            canvas(
                move |bounds, window, cx| {
                    let style = LabelStyle {
                        width: bounds.size.width,
                        font: window.text_style().font(),
                        font_size: px(14.),
                        color: gpui::white(),
                    };
                    let now = Instant::now();
                    let mut state = State {
                        shown: "New session".into(),
                        line: fit_text("New session", &style, window),
                        style: style.clone(),
                        transition: None,
                    };
                    state.prepare("Temporary prompt", style.clone(), bounds, now, window, cx);
                    let frame = state.prepare(
                        "Generated title",
                        style.clone(),
                        bounds,
                        now + Duration::from_millis(300),
                        window,
                        cx,
                    );
                    assert_eq!(frame.previous.unwrap().0.text.as_ref(), "Temporary prompt");
                    let start = state.transition.as_ref().unwrap().start;
                    // The retained ghost keeps its original truncation during resize.
                    let narrow = LabelStyle {
                        width: px(40.),
                        ..style.clone()
                    };
                    let frame = state.prepare(
                        "Generated title",
                        narrow,
                        bounds,
                        start + Duration::from_millis(550),
                        window,
                        cx,
                    );
                    assert!(frame.line.text.ends_with('…'));
                    assert_eq!(frame.previous.unwrap().0.text.as_ref(), "Temporary prompt");
                    assert_eq!(state.transition.as_ref().unwrap().start, start);
                    let frame = state.prepare(
                        "Generated title",
                        style.clone(),
                        bounds,
                        start + Duration::from_secs(3),
                        window,
                        cx,
                    );
                    assert!(frame.previous.is_none());
                    assert!(frame.particles.is_empty());
                    assert!(state.transition.is_none());
                    let frame = state.prepare(
                        "Generated title",
                        style,
                        bounds,
                        start + Duration::from_secs(4),
                        window,
                        cx,
                    );
                    assert!(frame.previous.is_none());
                    *checked.borrow_mut() = Some(());
                },
                |_, _, _, _| {},
            )
            .w(px(220.))
            .h(px(20.))
        }
    }
    let checked = Rc::new(RefCell::new(None));
    cx.update(|cx| {
        cx.open_window(Default::default(), |_, cx| {
            cx.new(|_| Harness(checked.clone()))
        })
        .unwrap()
    });
    cx.refresh().unwrap();
    assert!(checked.borrow().is_some());
}
