//! Generated HTML is shown as its source text: Linux has no sandboxed web view.

use gpui_kit::div;
use gpui_kit::{Context, Window, prelude::*};

pub(super) struct Html {
    body: String,
    interactive: bool,
}
impl Html {
    pub(super) fn new(body: String) -> Self {
        Self {
            body,
            interactive: true,
        }
    }
    pub(super) fn set_interactive(&mut self, interactive: bool) {
        if self.interactive != interactive {
            self.interactive = interactive;
        }
    }
    pub(super) fn hide(&self) {}
}
impl gpui_kit::Render for Html {
    fn render(&mut self, _: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().p_4().child(self.body.clone()).into_any_element()
    }
}
