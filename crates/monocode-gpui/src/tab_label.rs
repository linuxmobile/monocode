//! TabLabel.tsx: leave short labels clear and fade the last 40px only on overflow.
use gpui::{ContentMask, SharedString, TextAlign, TextRun, canvas, prelude::*, px};

pub(crate) fn tab_label(text: impl Into<SharedString>) -> impl IntoElement {
    let text = text.into();
    canvas(
        move |bounds, window, _| {
            let style = window.text_style();
            let font_size = style.font_size.to_pixels(window.rem_size());
            let run = TextRun {
                len: text.len(),
                font: style.font(),
                color: style.color,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let line = window.text_system().shape_line(
                text.clone(),
                font_size,
                std::slice::from_ref(&run),
                None,
            );
            let overflow = line.width() > bounds.size.width && bounds.size.width > px(0.);

            let line = if overflow {
                // Other backends use the same measured ramp at glyph centres.
                let runs = text
                    .char_indices()
                    .map(|(ix, ch)| {
                        let mut run = run.clone();
                        let x = (line.x_for_index(ix) + line.x_for_index(ix + ch.len_utf8())) / 2.;
                        run.len = ch.len_utf8();
                        run.color.a *= ((bounds.size.width - x) / px(40.)).clamp(0., 1.);
                        run
                    })
                    .collect::<Vec<_>>();
                window
                    .text_system()
                    .shape_line(text.clone(), font_size, &runs, None)
            } else {
                line
            };
            (line, overflow)
        },
        |bounds, (line, overflow), window, cx| {
            window.with_content_mask(Some(ContentMask { bounds }), |window| {
                let _ = overflow;
                // Keep both marker quads on separate layers from the glyphs:
                // GPUI otherwise batches a layer's quads before its text.
                window.paint_layer(bounds, |window| {
                    let _ = line.paint(
                        bounds.origin,
                        bounds.size.height,
                        TextAlign::Left,
                        None,
                        window,
                        cx,
                    );
                });
            });
        },
    )
    .size_full()
}
