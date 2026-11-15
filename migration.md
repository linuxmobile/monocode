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

- [ ] Replace `child-backend.ts`.
- [ ] Use `tokio::process::Command` for launching external processes.
- [ ] Implement stdin, stdout, and stderr handling.
- [ ] Implement streaming output processing.
- [ ] Implement process lifecycle tracking.
- [ ] Implement graceful termination and forced termination.
- [ ] Handle Unix signals correctly.
- [ ] Handle process groups and child-process cleanup.
- [ ] Prevent orphaned agent processes.
- [ ] Handle unexpected process termination.
- [ ] Support concurrent agent sessions.
- [ ] Avoid executing commands through a shell when direct process execution is sufficient.

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

- [ ] Replace `host/providers.ts` with native Rust implementations.
- [ ] Replace TypeScript provider adapters currently imported from `src/integrations/harness/providers/`.
- [ ] Audit every provider's actual CLI interface and communication protocol.
- [ ] Prefer direct executable invocation.
- [ ] Implement provider discovery through the user's `PATH`.
- [ ] Support custom executable paths where appropriate.
- [ ] Detect missing executables and provide clear error messages.
- [ ] Implement provider availability and version detection.
- [ ] Implement provider-specific argument construction.
- [ ] Implement stdin/stdout communication.
- [ ] Implement event parsing and normalization.
- [ ] Implement cancellation and process supervision.
- [ ] Preserve existing capabilities such as approval requests, tool events, session status, and streaming responses.
- [ ] Add integration tests using fake executables.

### Important

Do not assume that every provider shares the same communication protocol.

Implement provider-specific adapters behind a common Rust interface.

External CLI implementation details and their own runtime requirements are outside MonoCode's scope. The application must not install or manage those runtimes.

# Phase 4 — Session Engine and Persistence

- [ ] Replace `host/engine.ts`.
- [ ] Implement session lifecycle management in Rust.
- [ ] Implement event ingestion and normalization.
- [ ] Implement session state management.
- [ ] Implement event buffering and flushing.
- [ ] Implement snapshot generation.
- [ ] Implement session recovery after unexpected shutdowns.
- [ ] Preserve existing persistence semantics.
- [ ] Replace `host/store.ts`.
- [ ] Use SQLite through `rusqlite` or another justified native Rust implementation.
- [ ] Avoid blocking asynchronous workers with synchronous database operations.
- [ ] Use a dedicated database thread or actor where appropriate.
- [ ] Implement transactional writes.
- [ ] Preserve existing database migrations and user data.
- [ ] Implement database integrity and recovery tests.

Do not introduce a different database unless there is a measurable architectural benefit.

# Phase 5 — Workspace and Git

- [ ] Replace `host/workspace.ts`.
- [ ] Implement native filesystem operations.
- [ ] Implement file discovery and workspace management.
- [ ] Preserve existing file attachment functionality.
- [ ] Replace Git-related TypeScript modules.
- [ ] Implement branch management.
- [ ] Implement worktree management.
- [ ] Implement repository status and change detection.
- [ ] Prefer invoking the installed Git executable directly.
- [ ] Preserve Git configuration, hooks, and user environment.
- [ ] Handle filesystem errors and permission issues correctly.
- [ ] Add integration tests using temporary repositories.

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
