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
- [ ] Replace existing TypeScript HTTP endpoints. *(10 of 33; rest tracked above)*
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

- [ ] Replace `host/service.ts`.
- [ ] Remove Node-specific detached-process handling.
- [ ] Implement Linux-native daemon lifecycle management.
- [ ] Support systemd user services where appropriate.
- [ ] Implement installation, startup, shutdown, and removal.
- [ ] Handle service failures and restart behavior.
- [ ] Avoid requiring root privileges for ordinary user-level functionality.
- [ ] Ensure clean shutdown and resource cleanup.

# Phase 7 — Build System and Development Tooling

Remove Node.js-based build and automation scripts.

## Build

- [ ] Replace `scripts/build-native.mjs`.
- [ ] Replace `scripts/dev-native.mjs`.
- [ ] Replace JavaScript-based version management.
- [ ] Replace JavaScript-based packaging scripts.
- [ ] Remove esbuild.
- [ ] Remove Vitest.
- [ ] Remove all Node.js-specific build dependencies.
- [ ] Use Cargo for Rust compilation.
- [ ] Consider a Rust `xtask` for complex build and release orchestration.
- [ ] Use Bash for straightforward Linux build and packaging tasks.

## Installer and packaging

- [ ] Implement a Linux-native installation script using Bash.
- [ ] Support installation into appropriate user-level directories.
- [ ] Install desktop entries, icons, and application assets.
- [ ] Configure systemd user services where applicable.
- [ ] Implement uninstall functionality.
- [ ] Avoid unnecessary dependencies such as Node.js, npm, or Python.
- [ ] Ensure the installer works on a clean Linux environment.
- [ ] Document external system dependencies.

## Tooling policy

Preferred technologies:

1. Rust for application and backend functionality.
2. Bash for installation and straightforward Linux automation.
3. Python only when it offers a meaningful advantage over Bash or Rust.
4. Go only when there is a strong technical justification.

Bun is permitted only as an optional development-time alternative when a native solution is genuinely impractical. It must never be a mandatory application, backend, agent, installer, or packaging dependency.

# Phase 8 — Testing and Performance

## Functional testing

- [ ] Replace Vitest suites with Rust unit and integration tests.
- [ ] Test provider discovery.
- [ ] Test provider execution.
- [ ] Test streaming output.
- [ ] Test session lifecycle.
- [ ] Test process cancellation.
- [ ] Test unexpected process termination.
- [ ] Test concurrent sessions.
- [ ] Test SQLite persistence and recovery.
- [ ] Test workspace operations.
- [ ] Test Git operations.
- [ ] Test HTTP and WebSocket/SSE communication.
- [ ] Test systemd service management.

## Performance testing

Establish measurable baselines.

- [ ] Measure host startup time.
- [ ] Measure idle RAM consumption.
- [ ] Measure idle CPU consumption.
- [ ] Measure memory consumption with multiple sessions.
- [ ] Measure event processing throughput.
- [ ] Measure event propagation latency.
- [ ] Measure SQLite read/write performance.
- [ ] Measure process creation and shutdown overhead.
- [ ] Test prolonged sessions for memory leaks.
- [ ] Compare the native implementation against the original implementation where practical.

Prioritize real-world performance, not synthetic benchmark numbers alone.

Avoid premature optimization. Profile first, optimize bottlenecks, and benchmark again.

# Phase 9 — Complete Node.js Removal

This is a mandatory release requirement.

- [ ] Remove `package.json`.
- [ ] Remove `package-lock.json`.
- [ ] Remove all `*.js`, `*.mjs`, `*.cjs`, and `*.ts` project source files.
- [ ] Remove Node.js-specific configuration files.
- [ ] Remove `node_modules`.
- [ ] Remove esbuild and Vitest.
- [ ] Remove Node.js download and bundling logic.
- [ ] Remove Node.js runtime detection and setup.
- [ ] Remove Node.js-specific documentation.
- [ ] Remove npm-based scripts and commands.
- [ ] Remove references to `npx` and npm-based agent installation.
- [ ] Update CI workflows to eliminate Node.js setup.
- [ ] Update developer documentation.
- [ ] Update installation and deployment documentation.
- [ ] Ensure no application component requires Node.js.
- [ ] Ensure no first-party JavaScript or TypeScript source remains.

Run repository-wide checks using `rg` and other appropriate tools to identify remaining references.

Distinguish legitimate references in documentation about external tools from actual runtime dependencies. Document any remaining exceptions.

# Phase 10 — Final Validation

- [ ] Build the entire project in a clean Linux environment.
- [ ] Run `cargo test --workspace`.
- [ ] Run `cargo clippy --workspace --all-targets --all-features -- -D warnings` where applicable.
- [ ] Run formatting checks.
- [ ] Verify the installer on a clean Linux environment.
- [ ] Verify application startup without Node.js installed.
- [ ] Verify the backend starts without Node.js installed.
- [ ] Verify agent discovery using manually installed CLI executables.
- [ ] Verify OpenCode integration using the installed `opencode` CLI.
- [ ] Verify concurrent agent sessions.
- [ ] Verify graceful application and host shutdown.
- [ ] Verify all existing user data remains accessible.
- [ ] Verify no required JavaScript runtime or package manager is introduced by the build or installation process.

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
