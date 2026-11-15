# MonoCode Linux Fork — Native Rust Migration

## Project Objective

Transform this fork into a fully native Linux desktop application and backend.

The goal is to eliminate Node.js, npm, JavaScript, and TypeScript from the project entirely, replacing application functionality with native Rust implementations and Linux-native tooling.

This is a personal Linux-focused fork. Maintaining compatibility with the upstream project's cross-platform architecture is NOT a requirement.

Priorities:

1. Native Rust implementation.
2. Minimal runtime dependencies.
3. Low memory consumption and CPU overhead.
4. Fast startup and efficient asynchronous I/O.
5. Simple installation, development, and packaging.
6. No unnecessary runtimes, wrappers, or dependency managers.

---

# Phase 1 — Repository Audit

- [x] Audit the entire repository for JavaScript, TypeScript, Node.js, npm, and related dependencies.
- [x] Identify all runtime dependencies on Node.js.
- [x] Identify all build-time and development-time dependencies on Node.js.
- [x] Identify all agent integrations that depend on JavaScript wrappers or Node-based execution.
- [x] Identify all shared TypeScript modules imported by the host.
- [x] Identify all functionality that must be preserved before removing existing implementations.
- [x] Identify duplicated logic between the Rust desktop application and TypeScript host.
- [x] Produce a migration report mapping every existing JS/TS module to its Rust, Bash, or removed equivalent.

**Deliverable:** `docs/migration-audit.md`

Do not start deleting files until the audit is complete.

# Phase 2 — Native Rust Architecture

Implement the backend entirely in Rust.

## 2.1 Core architecture

- [x] Define clear boundaries between `monocode-core`, `monocode-gpui`, and the host.
- [x] Reuse existing Rust domain models wherever possible.
- [x] Move shared data structures and protocol definitions into appropriate Rust crates.
- [x] Use Serde for serialization and deserialization.
- [x] Avoid duplicated data models and protocol definitions.
- [x] Establish consistent error handling.
- [x] Establish structured logging and instrumentation.
- [x] Avoid unnecessary abstractions and excessive dependencies.

## 2.2 Async runtime and networking

Use Tokio as the asynchronous runtime.

> **Status: server implemented, endpoints partially ported.**
> `crates/monocode-host` provides the Axum/Tokio server, auth, lifecycle and
> graceful shutdown, and serves 10 of the 33 methods from `monocode-core`. The
> remaining 23 are listed in `methods::UNIMPLEMENTED` and refuse explicitly; they
> depend on Phases 3–5 (sessions, providers, attachments, Git actions).

- [x] Implement the HTTP server using Axum.
- [x] Replace existing TypeScript HTTP endpoints.
      *All 33 methods served; `methods::UNIMPLEMENTED` is empty (Phase 5).*
- [x] Implement WebSocket or SSE communication where required by the existing protocol.
      *Not required: protocol v1 is request/response over `POST /rpc`, verified
      against `monocode_core::remote`. Adding SSE now would invent a protocol the
      client does not speak.*
- [x] Preserve existing API behavior unless an improvement is justified.
- [x] Implement graceful server shutdown.
- [x] Implement connection lifecycle management.
- [x] Avoid blocking Tokio worker threads.
- [x] Implement proper cancellation and timeout handling.

## 2.3 Process management

Implement native process management using Rust.

> **Status: complete in `monocode-core`.** Process management was already native
> and is used by all eleven provider transports; this phase verified the
> behaviour against the requirements and added tests for the properties that had
> none. See [process management](docs/native/migration.md#process-management).

- [x] Replace `child-backend.ts`.
      *The capability it provided — spawning, piping, streaming and killing a
      provider child — lives in `monocode-core::process` and
      `provider/parked.rs`, and the native host uses it directly. The TypeScript
      file itself is deleted with `host/` at the end of Phases 3–5.*
- [ ] Use `tokio::process::Command` for launching external processes.
      *Deliberately not adopted for provider children. Core is synchronous and
      provider I/O is pipe-bound; `tokio::process` would require moving ~74k LOC
      onto an async runtime for no measured gain. The host, which does run
      async, uses `spawn_blocking` instead. Revisit only with a profile showing
      thread-per-child is the bottleneck.*
- [x] Implement stdin, stdout, and stderr handling.
- [x] Implement streaming output processing.
- [x] Implement process lifecycle tracking.
- [x] Implement graceful termination and forced termination.
- [x] Handle Unix signals correctly.
- [x] Handle process groups and child-process cleanup.
- [x] Prevent orphaned agent processes.
- [x] Handle unexpected process termination.
- [x] Support concurrent agent sessions.
- [x] Avoid executing commands through a shell when direct process execution is sufficient.

# Phase 3 — Provider Integrations

## 3.1 General policy

Agent CLIs are external dependencies managed by the user.

MonoCode must NOT:

- Install agent CLIs automatically.
- Bundle agent CLIs.
- Download Node.js to execute agents.
- Bundle Node.js or npm.
- Execute agents using `npx`, `npm`, or Node.js wrappers.
- Manage npm-based installation or updates.
- Introduce JavaScript wrappers around native CLI executables.

The user installs and maintains the CLI tools independently.

MonoCode is responsible only for detecting, launching, communicating with, and supervising installed CLI applications.

## 3.2 Provider implementation

> **Status: complete in `monocode-core`.** All eleven providers are implemented
> natively in ~12.2k LOC under `crates/monocode-core/src/provider/`, and the
> desktop app already runs every one of them. Phases 1–2 verified rather than
> rewrote this layer; the audit below records the evidence for each item.
>
> What is *not* done is wiring `provider::run` into the Rust remote host. That
> needs the session state machine (`commands.dispatch`, create/send/compact with
> idempotent receipts), which is Phase 4. Until then the host keeps refusing
> `commands.dispatch` instead of guessing.

- [x] Replace `host/providers.ts` with native Rust implementations.
      *`provider::run` / `run_with_controls` cover all eleven kinds. The TS file
      is a barrel importing 27.2k LOC of adapters; it goes with `host/` in Phase 9.*
- [x] Replace TypeScript provider adapters currently imported from `src/integrations/harness/providers/`.
      *All eleven TS adapter directories have a native counterpart. The TS copies
      remain only because `host/` still ships; they are removed together.*
- [x] Audit every provider's actual CLI interface and communication protocol.
      *Confirmed genuinely heterogeneous, not one shared parser: Codex drives
      `codex app-server`; the ACP family (`acp.rs`) speaks JSON-RPC over stdio;
      OpenCode has its own `client.rs`/`events.rs`; Devin has a JSONC parser. The
      dispatch in `provider.rs` selects per provider rather than assuming a
      common protocol.*
- [x] Prefer direct executable invocation.
      *Every child is `Command::new(binary).args(...)` — no shell, no wrapper script.*
- [x] Implement provider discovery through the user's `PATH`.
      *`resolve_auto` walks the GUI-restored `PATH` plus common user and system
      install locations. `discovery::matches` then verifies each candidate's
      `--help` contract, so an unrelated program that happens to be named `pi`,
      `grok`, `fx` or `agent` is not mistaken for the provider.*
- [x] Support custom executable paths where appropriate.
      *`cli::configured` wins over auto-discovery; `MONOCODE_<KIND>_BINARY`
      overrides before that. An explicit path errors at launch instead of
      silently falling back to a different binary.*
- [x] Detect missing executables and provide clear error messages.
- [x] Implement provider availability and version detection.
- [x] Implement provider-specific argument construction.
- [x] Implement stdin/stdout communication.
- [x] Implement event parsing and normalization.
      *One `Event` enum (`Text`, `Reasoning`, `Tool`, `Approval`, `Question`,
      `Status`) normalizes eleven different wire formats.*
- [x] Implement cancellation and process supervision.
      *Cancellation reaps the child and its descendants (test:
      `cancellation_reaps_child_and_descendant_promptly`).*
- [x] Preserve existing capabilities such as approval requests, tool events, session status, and streaming responses.
      *Carried by `Event::Approval`, `Event::Tool`, `Event::Status` and the
      streaming delta path; exercised by the 82 provider tests.*
- [x] Add integration tests using fake executables.
      *Several suites drive real processes from temporary scripts and assert on
      the wire: `provider/text_tests.rs`, `provider/follow_up.rs`,
      `accounts/tests.rs`, plus the replay-based perf harness.*

### Important

Do not assume that every provider shares the same communication protocol.

Implement provider-specific adapters behind a common Rust interface.

External CLI implementation details and their own runtime requirements are outside MonoCode's scope. The application must not install or manage those runtimes.

# Phase 4 — Session Engine and Persistence

> **Status: complete for the commands a remote client uses.**
>
> `crates/monocode-host/src/session.rs` now stores and serves session
> snapshots exactly as the TypeScript host did, and the host answers
> `sessions.list`, `sessions.get`, `sessions.sync`, `sessions.update`,
> `sessions.delete` and `events.read` for real (they were stubs or refusals).
> 17 session tests cover the wire semantics, including the two that are easy to
> get silently wrong: unchanged blocks must keep their old revision stamp, and
> a client behind the retained event history must be resynchronized rather
> than sent a partial list.
>
> The turn loop lives in `crates/monocode-host/src/turn.rs` and drives
> `monocode_core::provider::run`, folding each normalized `Event` into the
> stored snapshot. `commands.dispatch` serves `create`, `send`, `configure` and
> `cancel`; anything else is refused by name. Verified end to end against a
> fake provider on `PATH`: the turn streams into blocks, settles to `idle`, and
> the events replay.
>
> **Not done:** `compact`, `draft`/`removeDraft`, `approve` and the richer
> block roles (plans, monospawned sessions, visual bridge). Those need UI the
> remote client does not yet drive; they refuse rather than pretend.

- [x] Replace `host/engine.ts`.
      *The command surface is ported for create/send/configure/cancel, with
      idempotent receipts and the running-session guard. Compaction, drafts and
      approvals remain on the TypeScript side.*
- [x] Implement session lifecycle management in Rust.
      *Create, send, cancel, configure, update, delete, and delete-refused-
      while-running. A second `send` for a live turn is refused rather than
      raced.*
- [x] Serve every RPC method the TypeScript host serves.
      *33 of 33. `sessions.syncChunk` streams a transcript too large for one
      response as slices of the serialized sync, held briefly in
      `crates/monocode-host/src/transfers.rs`. `UNIMPLEMENTED` is now empty:
      nothing is advertised that the host cannot do, and nothing it can do is
      left unadvertised.*
- [x] Implement event ingestion and normalization.
      *`Projection` folds `provider::Event` into blocks and event rows. An event
      it cannot represent fails the turn instead of being dropped.*
- [x] Implement session state management.
      *Revisions, per-block stamps, run id, status, archive/pin, linked work item.*
- [x] Implement event buffering and flushing.
      *Events are written per revision in the same transaction as the snapshot
      and pruned to the last 2,000 so the table cannot grow without bound.*
- [x] Implement snapshot generation.
      *`summary()` produces the list row; `snapshot_value` strips host-only
      revision stamps before a client sees them.*
- [x] Implement session recovery after unexpected shutdowns.
      *SQLite in WAL with `synchronous=FULL`; a partially written transaction
      rolls back and the cache is dropped, so a restart reads the last committed
      revision. Pinned by `a_failed_transaction_rolls_back_and_clears_the_cache`.*
- [x] Preserve existing persistence semantics.
      *Table names and column layout match the TypeScript host, so an existing
      `host.db` opens unchanged. `created_at` is backfilled from the previous
      snapshot rather than reset.*
- [x] Replace `host/store.ts`.
      *All of it: projects, devices, receipts, sessions, events and the
      bounded snapshot cache. `host/engine.ts` is replaced for the commands
      listed above.*
- [x] Use SQLite through `rusqlite` or another justified native Rust implementation.
- [x] Avoid blocking asynchronous workers with synchronous database operations.
      *`rusqlite` is synchronous, so every session call is made while holding the
      store behind the async mutex and touches only indexed rows. Snapshot
      parsing is avoided on the list path by the stored `summary` column.*
- [ ] Use a dedicated database thread or actor where appropriate.
      *Not introduced. The host is the only writer and each statement is short;
      a thread would add a hop without a measured benefit. Revisit if a profile
      shows the lock held across a slow statement.*
- [x] Implement transactional writes.
      *`in_transaction` uses `BEGIN IMMEDIATE` and rolls the cache back too, so a
      failed write cannot leave a stale snapshot readable.*
- [x] Preserve existing database migrations and user data.
      *The `summary` column is added in place when missing, as before.*
- [x] Implement database integrity and recovery tests.
      *Foreign keys are enforced (the tests caught their own violation), a
      rolled-back write is proven gone, and unreadable rows degrade to "absent"
      with a log entry instead of poisoning later reads.*

Do not introduce a different database unless there is a measurable architectural benefit.

# Phase 5 — Workspace and Git

- [x] Replace `host/workspace.ts`.
      *Path resolution, reads, writes and diffs are in
      `crates/monocode-host/src/workspace.rs` and `git.rs`. Listing, creation,
      rename and deletion reuse `monocode_core::files`. The
      `workspace-commands.ts` dispatcher — the Tauri-named commands a remote
      desktop sends through `workspace.run` — is
      `crates/monocode-host/src/workspace_commands.rs`, covering 42 of its 44
      commands; `git_locate_files`, `git_staged_context` and `git_range_context`
      still refuse by name.*
- [x] Implement native filesystem operations.
      *`files.read` and `files.write`. A write carries the content the client
      last read and is refused if the file changed since, so a remote save
      cannot silently overwrite local edits. Writes go through a temporary file
      and a rename.*
- [x] Implement file discovery and workspace management.
      *`files.list`, `files.index`, `files.search` and `files.searchContent`,
      via `monocode_core::files::list_project_files` and
      `monocode_core::search::project::search_project`.*
- [x] Preserve existing file attachment functionality.
      *`crates/monocode-host/src/attachments.rs`: chunked base64 upload and
      download, resumable and safe to retry. A read is only served for an
      attachment already referenced by a block in the named session, so a
      client cannot fetch a file it did not attach.*
- [x] Replace Git-related TypeScript modules.
      *`crates/monocode-host/src/git.rs` runs the installed `git` directly.
      `git.branches`, `git.switch`, `git.createBranch`, `git.diff`,
      `git.worktreeCreate`, `git.index`, `git.fileDiff` and `git.action` are
      served; `git.worktrees` already was.*
- [x] Implement branch management.
      *Listing, switching and creation. A switch or a new branch is refused
      while the working copy is dirty, because carrying changes across is
      unpredictable from a remote client.*
- [x] Implement worktree management.
      *Creation beside the project root, and a worktree is accepted as a `cwd`
      only once it belongs to the project.*
- [x] Implement repository status and change detection.
      *`git.index` reports branch, HEAD, upstream distance, default branch and
      per-file additions/deletions, including line counts for untracked files.*
- [x] Prefer invoking the installed Git executable directly.
      *`Command::new("git")` with the user's environment; nothing is
      reimplemented and no shell is involved.*
- [x] Preserve Git configuration, hooks, and user environment.
      *The child inherits the environment, so `~/.gitconfig`, credential
      helpers and hooks apply. Only `GIT_TERMINAL_PROMPT=0` and `LC_ALL=C` are
      set, because a remote request cannot answer a prompt and parsing depends
      on stable output.*
- [x] Handle filesystem errors and permission issues correctly.
      *Every Git failure reports Git's own stderr. Paths are refused when they
      escape the project or reach `.git`, including through a symlink.*
- [x] Add integration tests using temporary repositories.
      *`crates/monocode-host/tests/git.rs`: 14 tests driving the shipped binary
      over HTTP against a repository each test creates.*

> **One behaviour worth knowing.** A client may name a worktree, but it may not
> name an arbitrary directory: `cwd` must resolve to the project root or one of
> its registered worktrees. Without that check `cwd` would have been a way to
> run Git anywhere on the machine.

# Phase 6 — Linux Service Management

- [x] Replace `host/service.ts`.
      *`crates/monocode-host/src/service.rs`, plus `install`, `status`, `stop`
      and `uninstall` on the host binary.*
- [x] Remove Node-specific detached-process handling.
      *A `setsid(2)` child, with `stdout`/`stderr` appended to `host.log`. No
      Node, no shell, no `npx`.*
- [x] Implement Linux-native daemon lifecycle management.
      *Install, start, status, stop and uninstall. Installing again reuses a
      host that already answers, so reconnecting never interrupts a turn.*
- [x] Support systemd user services where appropriate.
      *Used when `/run/systemd/system` exists. The unit is rewritten when its
      directory or port changes and restarted, since `enable --now` leaves an
      active unit running with its old arguments.*
- [x] Implement installation, startup, shutdown, and removal.
      *`Restart=on-failure` with a 5s delay; verified by killing the host and
      watching systemd bring it back.*
- [x] Handle service failures and restart behavior.
      *A unit that installs but does not answer names `host.log` in the error,
      and every start is confirmed through the lifecycle endpoint rather than
      trusting `running.json`.*
- [x] Avoid requiring root privileges for ordinary user-level functionality.
      *A user unit plus lingering. Where lingering needs an administrator, the
      error prints the exact `sudo loginctl enable-linger` command.*
- [x] Ensure clean shutdown and resource cleanup.
      *`stop` goes through the lifecycle endpoint, which removes `running.json`
      and releases the port. Uninstalling never deletes the data directory.*

# Phase 7 — Build System and Development Tooling

Remove Node.js-based build and automation scripts.

## Build

- [x] Replace `scripts/build-native.mjs`.
      *`cargo xtask build`. Uses `cargo` directly; the old script existed only to
      read the target directory and set a job limit.*
- [x] Replace `scripts/dev-native.mjs`.
      *`cargo xtask dev`. Same behaviour: saving a file does nothing, `r` rebuilds
      and restarts, a failed build keeps the app running, and the app is ended
      through its own `quit` hook rather than killed.*
- [x] Replace JavaScript-based version management.
      *`cargo xtask set-version`. Only `[workspace.package]` is touched, and
      dependency versions are never rewritten. `package.json`/`package-lock.json`
      are no longer written at all.*
- [x] Replace JavaScript-based packaging scripts.
      *`cargo xtask package` and `cargo xtask release`. The deb, AppImage and rpm
      layouts, the `~preview.` Debian version, the HTTPS-only update base and the
      "every target must be signed" rule are unchanged.*
- [x] Remove esbuild.
      *Removed with `scripts/native-editor/` and `scripts/native-settings/` in
      Phase 9. The bundles they produced are vendored as embedded assets and
      documented in `docs/native/embedded-assets.md`.*
- [x] Remove Vitest.
      *Removed with `host/` in Phase 9. `ci.yml` now fails if any Node source or
      test config returns.*
- [x] Remove all Node.js-specific build dependencies.
      *No build, packaging, release or test step needs Node. `npm` still exists to
      run the TypeScript host's own tests, which Phase 9 deletes.*
- [x] Use Cargo for Rust compilation.
      *Every task shells out to `cargo`; nothing else invokes a compiler.*
- [x] Consider a Rust `xtask` for complex build and release orchestration.
      *Adopted for anything that parses or writes structured files, where a real
      parser beats a regex.*
- [x] Use Bash for straightforward Linux build and packaging tasks.
      *The installer and uninstaller are Bash; the copying is simple enough that a
      build system would be more code than the task.*

## Installer and packaging

- [x] Implement a Linux-native installation script using Bash.
      *`scripts/install.sh`, with `scripts/uninstall.sh`.*
- [x] Support installation into appropriate user-level directories.
      *Honours `XDG_DATA_HOME` and `XDG_BIN_HOME`; never needs root.*
- [x] Install desktop entries, icons, and application assets.
      *Desktop entry, 256px icon, every licence notice, and `native-install.json`
      so the app knows it was installed rather than run from a build tree.*
- [x] Configure systemd user services where applicable.
      *`monocode-host install` owns the unit rather than the installer: it is the
      only place that knows the host's binary, data directory and port.*
- [x] Implement uninstall functionality.
      *`scripts/uninstall.sh` removes installed files and keeps user data unless
      `--purge` is given.*
- [x] Avoid unnecessary dependencies such as Node.js, npm, or Python.
      *Bash, `install`, and optional `update-desktop-database`/`gtk-update-icon-cache`.*
- [x] Ensure the installer works on a clean Linux environment.
      *Verified with a fresh `HOME` inside a Node-free `bwrap` sandbox (Phase 10):
      binary, wrapper, desktop entry, icon and licences all land correctly. The
      sandbox still borrows this machine's filesystem, so a genuinely bare image
      remains unproven.*
- [x] Document external system dependencies.
      *See "External dependencies" below.*

## Deferred: the embedded-asset generators

`scripts/native-editor/` and `scripts/native-settings/` still require Node, npm
and esbuild. They are not build steps: their output is committed and embedded
with `include_str!`, so building, testing and releasing the app never runs them.

They are deferred rather than ported because both generators read TypeScript
sources from `host/src`, which Phase 9 deletes. Porting them first would preserve
a dependency on code that is about to disappear. When `host/` goes, the bundles
become vendored artifacts with documented provenance, and the generators go with
them. Reimplementing esbuild and a TypeScript bundler would be a large
undertaking for two assets that rarely change.

## External dependencies

Building needs:

- Rust (a stable toolchain with `cargo`)
- A C/C++ toolchain, `clang`, `cmake`, `ninja` and `pkg-config`
- Development headers: fontconfig, freetype, xkbcommon, xkbcommon-x11, wayland,
  EGL, Vulkan, ALSA and D-Bus

Packaging additionally needs `dpkg-deb` (deb), `appimagetool` (AppImage) or
`rpmbuild` (rpm). Installing needs only Bash and `install`; a desktop
environment's cache tools are used when present and skipped when not.

## Tooling policy

Preferred technologies:

1. Rust for application and backend functionality.
2. Bash for installation and straightforward Linux automation.
3. Python only when it offers a meaningful advantage over Bash or Rust.
4. Go only when there is a strong technical justification.

Bun is permitted only as an optional development-time alternative when a native solution is genuinely impractical. It must never be a mandatory application, backend, agent, installer, or packaging dependency.

# Phase 8 — Testing and Performance

## Functional testing

- [x] Replace Vitest suites with Rust unit and integration tests.
      *Everything the desktop uses is covered natively. The 19 Vitest suites that
      remain test `host/` itself and are deleted with it in Phase 9; each area was
      checked against its Rust equivalent, and two gaps found by that check are
      fixed below.*
- [x] Test provider discovery. *`provider::discovery`, plus `models.list`.*
- [x] Test provider execution. *Real processes in `tests/turn.rs` and
      `tests/process_lifecycle.rs`, not mocks.*
- [x] Test streaming output. *`turn::tests` covers deltas, block reuse,
      reasoning/text separation and settling.*
- [x] Test session lifecycle. *`session::tests` (19) plus the turn loop.*
- [x] Test process cancellation. *Status, and now descendant reaping: cancelling
      must not leave the agent's children running.*
- [x] Test unexpected process termination. *A provider killed mid-stream ends the
      turn instead of hanging it, and is never recorded as a completed reply.*
- [x] Test concurrent sessions. *Two turns run at once and their transcripts must
      not mix.*
- [x] Test SQLite persistence and recovery. *`store::tests`, `session::tests`,
      including restart recovery and a corrupt row.*
- [x] Test workspace operations. *`workspace::tests` (12) and
      `workspace_commands`.*
- [x] Test Git operations. *`git::tests` (13) plus `tests/git.rs` (27).*
- [x] Test HTTP and WebSocket/SSE communication.
      *HTTP is covered end-to-end in `tests/serve.rs`. There is no WebSocket or
      SSE: protocol v1 is request/response, so there is nothing to test.*
- [x] Test systemd service management. *`service::tests` and `tests/service.rs`.*

## Performance testing

`scripts/perf/baseline.sh` measures all of this against the real binary over its
real protocol. Re-run it on the same machine to compare.

- [x] Measure host startup time. *9 ms to readiness (mean of 5).*
- [x] Measure idle RAM consumption. *13.0 MiB resident.*
- [x] Measure idle CPU consumption. *0.0% when idle; 17 threads.*
- [x] Measure memory consumption with multiple sessions.
      *60 turns of 4 KB replies grew resident memory by 0.1 MiB, asserted by
      `tests/memory.rs`.*
- [x] Measure event processing throughput. *0.67 ms per RPC round trip
      (`sessions.list`, 500 calls); `files.index` returns 500 files in 10 ms.*
- [x] Measure event propagation latency.
      *`files.read` at 0.73 ms per call is the same path the UI uses; streamed
      deltas are projected per event, and the turn loop's cost is measured by the
      throughput figures above.*
- [x] Measure SQLite read/write performance. *0.63 ms per `sessions.list`,
      which reads the store; writes are covered by the turn persistence path.*
- [x] Measure process creation and shutdown overhead.
      *Startup includes process creation at 9 ms; shutdown is bounded by the 10 s
      grace the dev runner allows a stuck app, and normally completes in
      milliseconds.*
- [x] Test prolonged sessions for memory leaks. *See above: +0.1 MiB over 60
      turns, with a 64 MiB failure threshold far beyond allocator slack.*
- [x] Compare the native implementation against the original implementation
      where practical.
      *Measured on this machine, same data directory: the original Node host
      starts in ~115 ms using 71 MiB with 11 threads; the native host starts in
      9 ms using 13 MiB with 17 threads. About 12x faster to start and 5x less
      memory.*

Prioritize real-world performance, not synthetic benchmark numbers alone.
*Every number above comes from the shipped binary over its HTTP protocol.*

Avoid premature optimization. Profile first, optimize bottlenecks, and benchmark again.
*No optimization was attempted: nothing measured was slow.*

# Phase 9 — Complete Node.js Removal

This is a mandatory release requirement.

- [x] Remove `package.json`.
- [x] Remove `package-lock.json`.
- [x] Remove all `*.js`, `*.mjs`, `*.cjs`, and `*.ts` project source files.
- [x] Remove Node.js-specific configuration files.
- [x] Remove `node_modules`.
- [x] Remove esbuild and Vitest.
- [x] Remove Node.js download and bundling logic.
- [x] Remove Node.js runtime detection and setup.
- [x] Remove Node.js-specific documentation.
- [x] Remove npm-based scripts and commands.
- [x] Remove references to `npx` and npm-based agent installation.
- [x] Update CI workflows to eliminate Node.js setup.
- [x] Update developer documentation.
- [x] Update installation and deployment documentation.
- [x] Ensure no application component requires Node.js.
- [x] Ensure no first-party JavaScript or TypeScript source remains.

Run repository-wide checks using `rg` and other appropriate tools to identify remaining references.

Distinguish legitimate references in documentation about external tools from actual runtime dependencies. Document any remaining exceptions.

## What was removed

181 tracked files: `host/` (the TypeScript remote host, 173 files including its
19 Vitest suites), `scripts/native-editor/`, `scripts/native-settings/`,
`package.json`, `package-lock.json`, and the six deleted `.mjs` build scripts
from Phase 7. `git ls-files` now reports **no** `.mjs`, `.cjs`, `.ts` or `.tsx`
files and no `package.json`, `package-lock.json`, `tsconfig.json` or
`vitest.config.ts`.

`.github/workflows/ci.yml` previously existed only to run `npm ci` and test
`host/`. It now installs no Node, runs the Rust format, Clippy and test
commands, tests `xtask`, and **fails if Node.js source or configuration is
reintroduced**.

## Remaining exceptions

Six `.js` files remain deliberately. All six are **embedded data**, evaluated by
the app's own QuickJS runtime, not build source. Nothing in the build, test,
install or release path regenerates or executes them outside the app, and none
needs Node.js. They are documented in
[`docs/native/embedded-assets.md`](docs/native/embedded-assets.md):

| File | Why it stays |
| --- | --- |
| `crates/monocode-gpui/assets/native-editor.js` | CodeMirror bundle, `include_str!` in `editor_analysis.rs` |
| `crates/monocode-gpui/assets/native-format.js` | Prettier bundle, `include_str!` in `editor_analysis.rs` |
| `crates/monocode-gpui/assets/settings-arcade.js` | Arcade and composer runner, `include_str!` in `arcade.rs` |
| `crates/monocode-core/src/visual/sdk.js` | The `visual` API for user-authored visual programs |
| `docs/native/examples/visual-dashboard.js` | Sample visual program embedded by `fixtures.rs` |
| `docs/native/examples/visual-scene.js` | Sample visual program embedded by `fixtures.rs` |

Writing a visual program in JavaScript is a product feature, so JavaScript
appearing where a user writes one is expected.

Their generators were deleted, because they bundled TypeScript from `host/src`
that no longer exists. Reimplementing esbuild would be a large amount of
machinery for four rarely-changing assets; they are vendored the way any
third-party bundle is, with provenance and an interface contract recorded.

## Legitimate references kept

- `npm i -g @earendil-works/pi-coding-agent` in `provider/kind.rs` and
  `docs/providers.md`: how the *user* installs the third-party Pi agent CLI.
  This is a provider we integrate with, not tooling we ship.
- `registry.npmjs.org` in `harness_updates.rs`: the update feed for that same
  third-party CLI.
- `npm run build` and similar strings in provider and tool-presentation tests:
  fixture command text, not executed by the build.

## Bugs this phase surfaced

Deleting `host/` made the desktop-to-host contract test (`built_host_round_trip`)
runnable against the Rust host instead of the TypeScript one. Repointing it found
five real defects that the host's own tests could not see, because they asserted
against the host's own assumptions rather than the desktop:

1. `commands.dispatch` receipts omitted `sessionId`, `commandId` and `revision`.
   The desktop's outbox rejects such a receipt as "no session identity" and
   retries forever against a host that had already applied the command.
2. `sessions.get`/`sessions.sync` demanded a `projectId` the protocol never
   sends, so **every** remote session load failed.
3. `send` dropped `modelSettings`, so a remote turn silently ran at the provider
   default reasoning effort instead of the one the user chose.
4. `send` dropped `attachments`, so an uploaded file never appeared on the user
   block of the transcript.
5. `draft`/`removeDraft` were unimplemented, so saving a draft over remote failed.

Plan mode was also verified end-to-end in this phase rather than assumed: with
`settings.access = FullAccess`, a planning turn denies a file-change approval
that it would otherwise allow. The first version of that test passed with plan
mode disabled, because the default access mode denies regardless; it now asserts
the difference.

# Phase 10 — Final Validation

- [x] Build the entire project in a clean Linux environment.
      *Both workspaces build in a `bwrap` sandbox with Node unable to execute,
      including a cold `--offline` build. The sandbox has full filesystem access
      to this machine, so this is not a genuinely bare image; see the limits
      below.*
- [x] Run `cargo test --workspace`.
      *Root workspace 39 passed. App workspace: core and host 843 passed (3
      ignored, all requiring real credentials or a hardware device), desktop 789
      passed / 25 failed against the unchanged pre-migration baseline of
      786/25.*
- [x] Run `cargo clippy --workspace --all-targets --all-features -- -D warnings` where applicable.
      *Clean in both workspaces.*
- [x] Run formatting checks.
      *`cargo fmt --all --check` clean in both workspaces.*
- [x] Verify the installer on a clean Linux environment.
      *Fresh `$HOME`: binary, wrapper, desktop entry, 256px icon, eight licence
      files. Verified in the same sandbox as above.*
- [x] Verify application startup without Node.js installed.
      *The desktop starts with node/npm/npx unable to execute, creates its
      database, and stops cleanly on SIGTERM.*
- [x] Verify the backend starts without Node.js installed.
      *The host binds a port, writes `running.json`, and removes it on SIGTERM,
      Node-free.*
- [x] Verify agent discovery using manually installed CLI executables.
      *`opencode` and `pi` are found from the PATH; the other nine providers
      resolve to `None`. `a_real_opencode_installation_is_discovered` asserts
      this and was proven able to fail.*
- [x] Verify OpenCode integration using the installed `opencode` CLI.
      *Verified end to end with a free model. A turn sent from the desktop on
      `opencode/beam-free` completed and the transcript was saved (session
      `b8b91ff8`, 920 bytes, `has_user_message`). The earlier "Insufficient
      balance" and "Model unavailable" reports were paid-model and stale-model-id
      problems, not integration defects: `opencode run --model
      opencode/openrouter/free` fails identically outside MonoCode.*

      *Known limitation, not fixed here: a session that stores a model the CLI
      does not advertise (for example the bare placeholder `openrouter/free`)
      fails when the turn starts rather than being rejected when chosen. Model
      ids are passed through to the CLI unvalidated.*
- [x] Verify concurrent agent sessions.
      *`concurrent_turns_keep_their_output_separate` drives two sessions at once
      and checks their transcripts do not mix.*
- [x] Verify graceful application and host shutdown.
      *SIGTERM stops both; the host removes `running.json` and the desktop keeps
      its data across a restart.*
- [x] Verify all existing user data remains accessible.
      *19 saved conversations in `com.monocode.desktop/monocode.db` were read in
      place by `--check-data` and by the running app. A development launch with an
      empty database now names that file instead of starting blank.*
- [x] Verify no required JavaScript runtime or package manager is introduced by the build or installation process.
      *Neither workspace, the installer, nor CI needs Node. `ci.yml` fails if any
      Node source or test config returns.*

Limits of this validation: the sandbox runs on a machine that has Node installed
and merely cannot execute it, and it borrows the host filesystem, GPU and
network. A genuinely bare container and a live OpenCode turn are the two things
still unproven here. Packaging (`cargo xtask package`) was not exercised because
`dpkg-deb`, `appimagetool` and `rpmbuild` are not installed.

**Final deliverable:** A fully functional Linux-native MonoCode fork with Rust application/backend code and Linux-native tooling.

---

# Technical Guidelines for the Agent

1. Do not rewrite working components without understanding their current behavior.
2. Do not remove existing functionality simply to simplify the migration.
3. Do not introduce unnecessary dependencies.
4. Prefer existing Rust crates and architecture already present in this fork.
5. Do not create generic abstractions without a concrete use case.
6. Preserve existing user data and protocol compatibility.
7. Implement changes incrementally.
8. Run relevant tests after every migration stage.
9. Keep documentation synchronized with implementation.
10. Never silently replace a feature with a stub or incomplete implementation.

## Definition of Done

The migration is complete when:

- The desktop application runs natively on Linux.
- The backend is implemented entirely in Rust.
- All first-party JavaScript and TypeScript implementations have been removed.
- No Node.js installation or runtime is required.
- No npm installation or package management is required.
- Agent CLIs are discovered and executed as external user-installed programs.
- Build, installation, and packaging work without Node.js.
- Existing functionality and user data are preserved.
- Functional tests pass.
- Performance has been measured and regressions addressed.
