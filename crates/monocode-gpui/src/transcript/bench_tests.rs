//! Scaling checks for the streaming patch path, run with
//! `cargo test -p monocode-gpui --profile native-dev -- --ignored --nocapture
//! perf_`
use super::*;
use gpui_kit::{TestAppContext, WindowBounds, WindowOptions, point};

fn row(role: &str, text: &str, first: bool) -> Row {
    Row {
        role: role.into(),
        text: text.into(),
        first,
        sent_at: None,
    }
}

fn paragraph(n: usize) -> String {
    format!(
        "## Step {n}\n\nFrame keeps the transcript retained while only the live row grows. \
         Every settled row keeps its layout, its measured height and its painted glyphs.\n\n\
         - Measure row {n} once per content version.\n- Move rows by translating paint.\n\n\
         ```rust\nfn step_{n}(rows: &mut [Row]) -> usize {{ rows.len() }}\n```\n\n"
    )
}

fn chat(cx: &mut TestAppContext) -> gpui_kit::WindowHandle<TranscriptView> {
    cx.update(|cx| {
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(0.), px(0.)),
                    size(px(900.), px(600.)),
                ))),
                ..Default::default()
            },
            move |_, cx| cx.new(|_| TranscriptView::new()),
        )
        .unwrap()
    })
}

/// Per-patch cost of streaming into a transcript of `turns` messages.
fn stream_cost(cx: &mut TestAppContext, turns: usize) -> (f64, f64) {
    let window = chat(cx);
    let mut rows = Vec::new();
    for n in 0..turns {
        rows.push(row("user", &format!("Question {n}"), true));
        rows.push(row("assistant", &paragraph(n).repeat(3), true));
    }
    window
        .update(cx, |view, _, cx| view.patch(0, rows, cx))
        .unwrap();
    cx.run_until_parked();

    let at = turns * 2 - 1;
    let mut streamed = paragraph(turns).repeat(3);
    let reply = streamed.clone();
    let start = std::time::Instant::now();
    let rounds = 20;
    for round in 0..rounds {
        streamed.push_str(&reply[round % reply.len()..(round % reply.len() + 16).min(reply.len())]);
        let text = streamed.clone();
        window
            .update(cx, |view, _, cx| {
                view.patch(at, vec![row("assistant", &text, true)], cx)
            })
            .unwrap();
    }
    let per_patch = start.elapsed().as_secs_f64() * 1000. / rounds as f64;
    let memory = window
        .update(cx, |view, _, _| {
            view.source_bytes + view.rows.len() * std::mem::size_of::<UiRow>()
        })
        .unwrap();
    (per_patch, memory as f64 / 1024. / 1024.)
}

#[gpui_kit::test]
#[ignore]
fn perf_streaming_rebuild_scaling(cx: &mut TestAppContext) {
    for turns in [20, 80, 320] {
        let (per_patch, memory) = stream_cost(cx, turns);
        println!(
            "turns={turns} rows={} patch={per_patch:.3}ms retained={memory:.1}MiB",
            turns * 2
        );
    }
}

fn tool_row(n: usize) -> Row {
    let call = serde_json::json!({
        "id": format!("call-{n}"),
        "kind": "execute",
        "title": format!("cargo test --package monocode-core -- test_{n}"),
        "status": "completed",
        "input": "{\"command\":\"cargo test\"}",
        "output": "running 818 tests\n".repeat(40),
        "preview": {"title": format!("cargo test {n}")},
    });
    row("tool", &call.to_string(), true)
}

/// A streaming turn that carries a run of tool calls, the shape that walks
/// every tool row to find agent panels.
#[gpui_kit::test]
#[ignore]
fn perf_streaming_with_tool_rows(cx: &mut TestAppContext) {
    let window = chat(cx);
    let mut rows = Vec::new();
    for n in 0..300 {
        rows.push(row("user", &format!("Question {n}"), true));
        for step in 0..8 {
            rows.push(tool_row(n * 8 + step));
        }
        rows.push(row("assistant", &paragraph(n), true));
    }
    window
        .update(cx, |view, _, cx| view.patch(0, rows, cx))
        .unwrap();
    cx.run_until_parked();
    let at = window
        .update(cx, |view, _, _| view.source.len() - 1)
        .unwrap();
    let start = std::time::Instant::now();
    let rounds = 20;
    for round in 0..rounds {
        let text = paragraph(300).repeat(3) + &" more words".repeat(round);
        window
            .update(cx, |view, _, cx| {
                view.patch(at, vec![row("assistant", &text, true)], cx)
            })
            .unwrap();
    }
    println!(
        "tool-heavy patch={:.3}ms over {} rows",
        start.elapsed().as_secs_f64() * 1000. / rounds as f64,
        at
    );
}
