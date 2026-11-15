//! A frame readout for profiling: launched with `MONOCODE_FRAME_LOG=<file>`,
//! the app appends one line per second it drew anything, so you can see how
//! many frames streaming or scrolling costs and which views keep asking for
//! them.
//! `MONOCODE_FRAME_LOG=/tmp/monocode-frames.log target/release/monocode-gpui`
//!
//! - `fps`: draws per second, split into `layout` (content changed),
//!   `paint` (only animation steps) and `refresh` (whole-window redraws).
//! - `draw`: average and slowest time inside GPUI's draw, in milliseconds.
//! - `main`: main-thread CPU over the second, the number to keep low.
//! - `views`: entity views rendered vs. cached ones reused.
//! - `by`: the views whose own `notify` caused frames, with frame counts.
//! - after `|`: counts the app adds with `frame_stats::count`.
use gpui_kit::App;

/// GPUI Fast has no frame-stats hook, so the log reports zero draw counters
/// until a Fast-native measurement replaces this shim.
mod frame_stats {
    use std::time::Duration;

    #[derive(Default)]
    pub struct FrameStats {
        pub frames: u32,
        pub layout_frames: u32,
        pub paint_frames: u32,
        pub refresh_frames: u32,
        pub draw_time: Duration,
        pub max_draw: Duration,
        pub views_rendered: usize,
        pub views_reused: usize,
        pub origins: Vec<(String, u32)>,
        pub counters: Vec<(String, u32)>,
    }

    pub fn enable() {}
    pub fn take() -> FrameStats {
        FrameStats::default()
    }
    #[allow(dead_code)]
    pub fn add(_name: &str, _value: u32) {}
    #[allow(dead_code)]
    pub fn count(_name: &str) {}
}
use std::{
    io::Write,
    time::{Duration, Instant},
};

const PERIOD: Duration = Duration::from_secs(1);
const ORIGINS: usize = 6;

pub(crate) fn start(cx: &mut App) {
    let Some(path) = std::env::var_os("MONOCODE_FRAME_LOG") else {
        return;
    };
    frame_stats::enable();
    cx.spawn(async move |cx| {
        // The task runs on the main thread, so this is the thread to measure.
        let mut cpu = MainThreadCpu::new();
        let mut last = Instant::now();
        loop {
            cx.background_executor().timer(PERIOD).await;
            let stats = frame_stats::take();
            let now = Instant::now();
            let main = cpu.percent_since_last(now.duration_since(last));
            last = now;
            if stats.frames == 0 {
                continue;
            }
            let line = format_line(&stats, main);
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
            {
                let _ = writeln!(file, "{line}");
            }
        }
    })
    .detach();
}

fn format_line(stats: &frame_stats::FrameStats, main: Option<f64>) -> String {
    let ms = |d: Duration| d.as_secs_f64() * 1000.;
    let average = ms(stats.draw_time) / f64::from(stats.frames.max(1));
    let by = stats
        .origins
        .iter()
        .take(ORIGINS)
        .map(|(name, frames)| format!("{name}={frames}"))
        .collect::<Vec<_>>()
        .join(" ");
    let counters = stats
        .counters
        .iter()
        .map(|(name, count)| format!(" {name}={count}"))
        .collect::<String>();
    let main = main.map_or_else(|| "?".into(), |main| format!("{main:.1}%"));
    format!(
        "{} fps={} layout={} paint={} refresh={} draw={average:.2}/{:.2}ms busy={:.1}% \
         main={main} views={}/{} by: {by}{}",
        chrono::Local::now().format("%H:%M:%S"),
        stats.frames,
        stats.layout_frames,
        stats.paint_frames,
        stats.refresh_frames,
        ms(stats.max_draw),
        100. * stats.draw_time.as_secs_f64() / PERIOD.as_secs_f64(),
        stats.views_rendered,
        stats.views_reused,
        if counters.is_empty() {
            String::new()
        } else {
            format!(" |{counters}")
        },
    )
}

/// CPU time of the thread that created it, read through Mach.
struct MainThreadCpu {
    last: Option<Duration>,
}

impl MainThreadCpu {
    fn new() -> Self {
        let mut cpu = Self {
            // SAFETY: names the calling thread's Mach port without taking a
            // new right, so there is nothing to deallocate.
            last: None,
        };
        cpu.last = cpu.used();
        cpu
    }

    fn percent_since_last(&mut self, wall: Duration) -> Option<f64> {
        let used = self.used()?;
        let previous = self.last.replace(used)?;
        let busy = used.saturating_sub(previous);
        Some(100. * busy.as_secs_f64() / wall.as_secs_f64().max(f64::EPSILON))
    }

    fn used(&self) -> Option<Duration> {
        None
    }
}
