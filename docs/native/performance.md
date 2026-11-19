# Performance: measuring and what costs what

Numbers below were measured on a 16-core Linux/Wayland machine with the
release build (`cargo build --release --manifest-path
crates/monocode-gpui/Cargo.toml -p monocode-gpui`), driving the replay provider
from `scripts/perf/fake-claude-stream.py`. They are baselines, not targets.

## Measure before changing anything

```sh
# main-thread CPU per second, plus the app's own hot paths
MONOCODE_PERF_TRACE=1 MONOCODE_FRAME_LOG=/tmp/frames.log \
  scripts/perf/frame-run.sh crates/monocode-gpui/target/release/monocode-gpui 20
```

- `MONOCODE_FRAME_LOG=<file>` appends one line per busy second. GPUI Fast
  exposes no frame counters, so a Linux build logs `fps=0` with `main=<cpu%>`:
  that is the number to keep low.
- `MONOCODE_PERF_TRACE=1` prints `[perf] name calls= total= max= avg=` lines
  for scopes added with `perf_trace::Scope::new("name")`. Instrument the
  function you suspect instead of guessing.
- `MONOCODE_PERF_SCROLL=<px/s>` scrolls a finished reply on every display
  frame (`MODE=scroll` in `frame-run.sh`), for scrolling rather than streaming.

Whole-process CPU and RSS without opening a window log:

```sh
# replays one reply, prints CPU% and RSS once per second
sh scripts/perf/frame-run.sh   # then sample /proc/<pid>/status VmRSS
```

## Baseline (release, replay streaming at 25 deltas/s)

| Phase | Main-thread CPU | RSS |
|---|---|---|
| idle, one blank chat | 0% | 165 MiB |
| idle, 1.2 MiB transcript loaded | 0% | 211 MiB |
| streaming a reply, before `pulse::land` | 32.6% mean of one core | 195 → 207 MiB |
| streaming a reply, after `pulse::land` | 24.5% mean of one core | 195 → 207 MiB |
| scrolling a finished reply (`MONOCODE_PERF_SCROLL=1500`) | 24.0% | 206 MiB |
| streaming with deltas at 10/s | 21–23% | 203 MiB |
| idle, 12.5 MiB / 3000-turn session open | 0.7% | 205 MiB |

The app's own code is only ~4–5% of that: `transcript.render` 15 ms/s,
`shell.render` 12 ms/s, `transcript.rebuild` 7 ms/s, `conversation.render`
3 ms/s. The rest is GPUI Fast's element diffing, layout, text shaping, prepaint
and paint, redone for every whole-window frame. Each drawn frame costs roughly
4–5 ms of main-thread CPU on this machine, so the two levers that matter are
**fewer frames** and **fewer invalidated views per frame**, not micro-optimising
`render`.

Streaming frames per second (`transcript.render` calls per second, replay at 25
deltas/s) went from 54–69 to 31–46 once updates landed inside the pulse tick.
The remaining frames are the spinners' own 30 Hz beat, which is the floor for a
live reply.

Idle is genuinely idle: no timer keeps drawing when nothing is happening.

## What was changed

- `perf_trace` (new): opt-in scoped timings, one line per second, free when
  `MONOCODE_PERF_TRACE` is unset.
- `frame_log`: main-thread CPU now reads `/proc/self/task/<tid>/stat` on Linux
  (it was a Mach-only stub that returned `None`, so `MONOCODE_FRAME_LOG` wrote
  nothing at all), and a busy second is logged even when frame counters are
  absent.
- Transcript rebuild caches "does this tool row open an agent panel" per source
  row and drops the verdict when the row is patched, instead of re-parsing
  every tool row's JSON on every streamed patch.
- `transcript::bench_tests` measures the streaming patch path
  (`cargo test -p monocode-gpui --profile native-dev -- --ignored --nocapture
  perf_streaming`).
- A completed context compaction folds the turns before it into one summary row.
  Only presentation is hidden: source rows, provider context and the saved
  transcript are unchanged, and "Show earlier turns" opens them again. The
  verdict is cached per source row and dropped when that row is patched.
- Wrapped-line counts for a prompt's trailing chunk are cached by message
  content, so shaping text is not repeated on every frame a prompt is visible.
  Row and code element ids now derive from a row's domain key, not its list
  index, so a folded range cannot mis-key them.
- The new-provider-session history fallback collects rows before joining them
  instead of prepending to a `String` one row at a time, and counts separators
  against its 32 KiB budget.

## Known costs, in the order they are worth fixing

1. ~~A frame per streamed delta, on top of the 30 Hz animation beat.~~ Fixed:
   the update queue hands each streamed update to `pulse::land`, which runs it
   inside the clock's next tick before the tick notifies the loops, so the
   reply draws in the spinners' frame instead of one of its own (-23% CPU).
2. **Ancestor re-render on any transcript change.** `cx.notify()` on the
   transcript marks the conversation dirty and GPUI Fast marks its ancestors
   dirty too, so `conversation.render` and `shell.render` run for every
   notification (50–60/s while a reply streams or the transcript scrolls). The
   sidebar, tab strip and project rail are cached views, but a parent re-render
   is what lets their retained layout be dropped: that is why
   `typing_and_streaming_do_not_rebuild_sidebar_or_tabs` and
   `project_rail_and_transcript_stay_cached_during_independent_updates` fail on
   this tree. The reuse decision lives in GPUI Fast's retained-layout cache
   (`Window::dirty_views`, `reusable_cached`), not in this crate, so making
   those two tests pass means fixing or upgrading the renderer.
3. **`patch_view` re-places markers over the whole tail** of a streamed reply
   on every delta (`visuals::placements` over `self.source[start..]`). One
   message is cheap; a long reply near the end of a turn pays for the whole
   message at 25 Hz.
4. **Blob-backed shared sessions rewrite their whole history on each save.**
   `shared_write_rows` parses all of `blocks_json`, merges, re-encodes it and
   re-hashes it every 650 ms while streaming (`store/shared.rs`). Indexed
   sessions limit this to the newest 10 turns. Migrating a blob session to
   indexed paging changes row indices, so it belongs in an offline import,
   not in a live save path.

## Not the problem

- Idle CPU, GPU trimming, image caches and the syntax-highlight cache are all
  already bounded; the caches hold ≤2 MiB markdown, ≤500 KiB highlights,
  ≤96 MiB decoded images.
- Provider streaming itself is off the UI thread: worker threads stay at
  ~0.3% of a core while a reply streams.

## Memory

Measured with `scripts/perf/memory.sh <db>` (idle RSS sampled from
`/proc/<pid>/status`), a store built by writing N turns of ~4 KiB markdown into
one session:

| Loaded transcript | RSS |
|---|---|
| blank session | 165 MiB |
| 1.2 MiB (300 turns) | 205 MiB |
| 12.5 MiB (3000 turns) | 205 MiB |

The transcript loads by page (`history::page_start`, `history_start`), so a
huge session costs what the open page costs, not its whole history. Images are
capped at 96 MiB decoded, markdown at 2 MiB, highlighting at 500 KiB, and
closed sessions keep at most 8 loaded histories and 32 MiB
(`trim_session_histories`). Two things are therefore worth checking before
believing a large RSS number is a leak:

- **Many sessions at once.** Each loaded session holds its history page plus
  markdown parsers and highlights; opening twenty chats is the one way to reach
  hundreds of MiB. Watch `rss` while opening tabs.
- **Not the app's heap.** GPU glyph atlases and driver allocations are not in
  the app's own accounting. Compare RSS against `/proc/<pid>/smaps_rollup` for
  a large anonymous mapping before attributing it to the transcript.

Nothing in this tree reproduced the reported +800 MiB; the numbers above are
the baseline to compare against when reproducing it.
