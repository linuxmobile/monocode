//! Opt-in timings for the app's own hot paths: with `MONOCODE_PERF_TRACE=1`
//! each second prints a line with the calls, total time, worst call and
//! average per named scope, e.g. `transcript.rebuild calls=25 total=81.2ms
//! max=5.0ms`.
//!
//! Scopes cost nothing when the variable is unset: the guard checks a cached
//! flag before it reads the clock, and nothing is recorded.
use gpui_kit::App;
use std::{
    cell::RefCell,
    collections::HashMap,
    sync::OnceLock,
    time::{Duration, Instant},
};

const PERIOD: Duration = Duration::from_secs(1);

#[derive(Default, Clone, Copy)]
struct Totals {
    calls: u32,
    total: Duration,
    max: Duration,
}

thread_local! {
    static TOTALS: RefCell<HashMap<&'static str, Totals>> = RefCell::new(HashMap::new());
}

fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("MONOCODE_PERF_TRACE").is_some())
}

/// Times the enclosing statement and records it under `name` when tracing.
pub(crate) struct Scope {
    name: &'static str,
    start: Option<Instant>,
}
impl Scope {
    pub(crate) fn new(name: &'static str) -> Self {
        Self {
            name,
            start: enabled().then(Instant::now),
        }
    }
}
impl Drop for Scope {
    fn drop(&mut self) {
        let Some(start) = self.start else { return };
        let elapsed = start.elapsed();
        TOTALS.with_borrow_mut(|totals| {
            let entry = totals.entry(self.name).or_default();
            entry.calls += 1;
            entry.total += elapsed;
            entry.max = entry.max.max(elapsed);
        });
    }
}

pub(crate) fn start(cx: &mut App) {
    if !enabled() {
        return;
    }
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(PERIOD).await;
            let taken = TOTALS.with_borrow_mut(std::mem::take);
            if taken.is_empty() {
                continue;
            }
            let mut rows: Vec<_> = taken.into_iter().collect();
            rows.sort_by_key(|(_, totals)| std::cmp::Reverse(totals.total));
            let line = rows
                .iter()
                .map(|(name, totals)| {
                    let average = totals.total.as_secs_f64() * 1000. / f64::from(totals.calls);
                    format!(
                        "{name} calls={} total={:.1}ms max={:.1}ms avg={average:.3}ms",
                        totals.calls,
                        totals.total.as_secs_f64() * 1000.,
                        totals.max.as_secs_f64() * 1000.,
                    )
                })
                .collect::<Vec<_>>()
                .join(" | ");
            eprintln!("[perf] {line}");
        }
    })
    .detach();
}
