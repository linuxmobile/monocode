# Phase 1 — Repository Audit

Audit of JavaScript, TypeScript, Node.js, npm and JavaScript-runtime dependencies in
MonoCode, produced for the Linux-native Rust migration.

Audited revision: `2dd99794997866635b529e1115dc3dd65dbf4a4e`

No files were deleted or modified to produce this report.

> **Status: superseded.** This documents the repository as it stood at the
> audited revision, before any migration work. Everything described here as
> TypeScript or Node.js has since been removed: `host/` was deleted in Phase 9
> once `crates/monocode-host` served all 33 protocol methods, and the build
> scripts were replaced by `xtask` in Phase 7. Six embedded `.js` assets remain
> deliberately as product data; see
> [embedded assets](native/embedded-assets.md). Read this file as the starting
> inventory, not as a description of the current tree.

---

## 0. Executive summary

The migration described in `migration.md` was **already completed for the desktop
application**. The former React/Tauri app is not in the repository. The desktop client
is Rust + GPUI (`crates/monocode-gpui`), backed by a Rust service layer
(`crates/monocode-core`).

What remains is a **single, well-bounded** JavaScript surface:

| Component | Status | Runtime needed |
| --- | --- | --- |
| `crates/monocode-gpui` (desktop app) | Complete, native | None |
| `crates/monocode-core` (services) | Complete, native (73,343 LOC) | None |
| `host/` (headless **remote** host server) | TypeScript, 59,445 LOC | Node.js 24 |
| `scripts/*.mjs` (build, dev, packaging) | JavaScript | Node.js 22+ |
| `scripts/native-editor/`, `scripts/native-settings/` | TypeScript asset generators | Node.js + npm |
| Embedded editor/arcade bundles in `assets/*.js` | Generated JavaScript, run in Rust via QuickJS | None at runtime |

**The one real architectural decision in this phase:** `host/` is not legacy code left
behind by an incomplete port. It is the *shipped remote-host server* that the native
desktop app connects to over SSH + JSON-RPC to run agent sessions on another machine.
The Rust client (`crates/monocode-core/src/remote.rs`, 960 LOC) already speaks its
protocol. Deleting `host/` without a replacement silently removes the Remote
Sessions feature, which violates Guideline 2 ("do not remove existing functionality
simply to simplify the migration").

Consequences for the plan:

1. Phases 2–6 are largely **already done in `monocode-core`** for local operation and
   need to be assessed as "verify + port the remote-host role", not "write from scratch".
2. The genuine remaining work is: (a) port the remote host to Rust, (b) port build and
   packaging scripts to Bash/Rust `xtask`, (c) remove npm/Node from CI.
3. `migration.md` Phase 2.2 ("Implement the HTTP server using Axum") conflicts with the
   existing style: the current Rust code has **no async runtime and no HTTP server
   crate**. The remote host is the only place a server is needed.

---

## 1. Inventory of JavaScript / TypeScript surface

Counts from `git ls-files`, excluding `node_modules/`, `vendor/`, `target/`.

| Extension | Files |
| --- | --- |
| `.ts` | 169 |
| `.mjs` | 16 |
| `.js` | 6 |
| `.json` | 11 |

Breakdown of TypeScript/JavaScript by directory:

| Location | Files | Role |
| --- | --- | --- |
| `host/` (top level) | 20 non-test `.ts`/`.mjs` | Remote host server, CLI, engine, store, git, workspace, service |
| `host/src/` | 130 `.ts` | Shared models, provider harness adapters, feature models |
| `scripts/` | 9 `.mjs` | Build, dev, packaging, version bump |
| `scripts/native-editor/` | 5 (`.ts`/`.mjs`/`.json`) | CodeMirror + Prettier bundle generator |
| `scripts/native-settings/` | 1 `.mjs` | Arcade settings bundle generator |
| `crates/monocode-gpui/assets/*.js` | 3 | Generated bundles executed by QuickJS |
| `docs/native/examples/` | 2 `.js` | Examples only, not built |

Test files: **24** `.test.ts` / `.test.mjs` files (Vitest for `host/`, `node:test` for
`scripts/`).

---

## 2. Runtime dependencies on Node.js

### 2.1 `host/` — the only runtime Node.js dependency

`host/cli.ts` is packaged by esbuild into `build/host/monocode-host.mjs` and run as
`node build/host/monocode-host.mjs`, installed to `~/.monocode-host/bin/monocode-host`.

`host/cli.ts` additionally **prepends Node's own directory to `PATH`** so npm-based
provider CLIs can spawn Node subprocesses:

```ts
// host/cli.ts
process.env.PATH = [dirname(process.execPath), process.env.PATH ?? ""].join(delimiter);
```

This is exactly the behaviour Phase 3.1 forbids ("Do not download Node.js to execute
agents", "no JavaScript wrappers around native CLI executables"). It must be removed
with the host port.

Node built-in modules used by `host/` and `scripts/`:

| Module | Uses | Rust equivalent |
| --- | --- | --- |
| `node:path` | 41 | `std::path` |
| `node:fs` | 29 | `std::fs` |
| `node:os` | 28 | `std::env`, `hostname` crate |
| `node:child_process` | 25 | `tokio::process` / `std::process` |
| `node:fs/promises` | 19 | `tokio::fs` |
| `node:util` | 9 | `std` |
| `node:url` | 9 | `std::path` |
| `node:crypto` | 9 | `sha2`, `rand`, `base64` (already in core) |
| `node:test` | 5 | `#[test]`, `cargo test` |
| `node:http` | 5 | Axum (new dependency) |
| `node:assert/strict` | 5 | `assert!` / `assert_eq!` |
| `node:net` | 4 | `std::net` |
| `node:timers/promises` | 2 | `tokio::time` |
| `node:sqlite` | 2 | `rusqlite` (already in core) |
| `node:vm`, `node:readline`, `node:module`, `node:https`, `node:events` | 1 each | `rquickjs`, `std::io`, `std` |

### 2.2 Embedded JavaScript executed by Rust (no Node.js at runtime)

`rquickjs` (QuickJS) executes three generated bundles inside the native app:

| Bundle | Consumer | LOC |
| --- | --- | --- |
| `assets/native-editor.js` | `crates/monocode-gpui/src/editor_analysis.rs` | 1.2 MB |
| `assets/native-format.js` | `crates/monocode-gpui/src/editor_analysis.rs` | 2.1 MB |
| `assets/settings-arcade.js` | `arcade.rs`, `composer_runner.rs` | 22 KB |

Also `crates/monocode-core/src/visual.rs` uses `rquickjs` for visual/document scripts.

These are **build-time** generated but **runtime** executed. They do not require
Node.js on the user's machine, but they do keep a JavaScript engine (QuickJS) inside
the Rust binary and require npm at build time. See §7 for the decision.

---

## 3. Build-time and development-time dependencies on Node.js

`package.json` devDependencies:

| Package | Version | Purpose | Replacement |
| --- | --- | --- | --- |
| `typescript` | `~5.8.3` | Type-check `host/` | None needed after host port |
| `esbuild` | `0.28.2` | Bundle host CLI + editor assets | `cargo build`; editor assets need separate decision |
| `vitest` | `^3.2.4` | `host/` test runner | `cargo test` |
| `@types/node` | `26.5.0` | Types | None |

npm scripts that exist today:

| Script | Command | Replacement |
| --- | --- | --- |
| `host:build` | `tsc --noEmit && node host/build.mjs` | `cargo build -p monocode-host` |
| `host:package` | build + `node host/package.mjs` | `xtask host-package` or Bash |
| `host` | `node build/host/monocode-host.mjs` | Rust binary |
| `test:host` | `vitest run` | `cargo test` |
| `check:host` | `tsc` + `test:host` | `cargo clippy` + `cargo test` |
| `set-version` | `node scripts/bump-version.mjs` | Bash / `xtask set-version` |
| `native:dev` | `node scripts/dev-native.mjs` | Rust `xtask dev` or Bash |
| `native:build` | `node scripts/build-native.mjs` | **`cargo build --release` directly** |
| `native:test:dev` | `node --test scripts/*.test.mjs` | `cargo test` for the Rust equivalents |
| `native:test` | `cargo test …` | Unchanged |
| `native:check` | `npm run native:test:dev && cargo fmt/clippy/test` | Drop the `node --test` part |

`scripts/build-native.mjs` is a thin wrapper: it reads `cargo metadata`, then runs
`cargo build --release`. It can be replaced by a two-line Bash script.

`scripts/dev-native.mjs` plus `scripts/native-release.mjs`,
`scripts/package-native.mjs`, `scripts/bump-version.mjs` carry the real logic and need
porting to `xtask` (packaging, `.deb`/`.rpm`/AppImage generation, version bumping,
release file lists).

CI depends on Node.js in all three workflows:

| Workflow | Node usage |
| --- | --- |
| `.github/workflows/ci.yml` | `setup-node@v4` (24), `npm ci`, `npm run check:host`, `node --test host/runtime-download.test.mjs`, `npm run host:package` |
| `.github/workflows/native.yml` | `setup-node@v4` (22), `npm run native:test:dev` |
| `.github/workflows/native-release.yml` | `setup-node@v4` (22), `npm ci`, `node scripts/package-native.mjs`, `node scripts/native-release.mjs`, `node --input-type=module` version extraction (×2) |

---

## 4. Agent integrations that depend on JavaScript wrappers or Node execution

This is the area where the migration text and the current code diverge most.

### 4.1 Current state

There are **two independent provider implementations**:

| Implementation | Location | Used by |
| --- | --- | --- |
| Rust | `crates/monocode-core/src/provider/` (`provider.rs` 2,383 LOC, `acp.rs` 1,702, `pi.rs` 840, `opencode.rs` 841, `devin.rs` 774, `discovery.rs`, `lifecycle.rs`, `questions.rs`, `tasks.rs`, `rewind.rs`, `compaction.rs`, `limits.rs`, `follow_up.rs`) | Desktop app (local sessions) and remote client |
| TypeScript | `host/src/integrations/harness/core/` (13 files) + `host/src/integrations/harness/providers/` (11 providers, ~100 files) | `host/` remote server only |

The TS harness is **not imported by any Rust code**. The only Rust references to it are
prose comments (`crates/monocode-core/src/provider/acp.rs:2`,
`crates/monocode-core/src/transcript.rs:102`) noting that Rust files are ports of the
TS ones.

### 4.2 Per-provider comparison

| Provider | TS files | Rust transport | Node wrapper at runtime? |
| --- | --- | --- | --- |
| Claude Code | 6 | stream-json (`provider.rs`) | No |
| Codex | 9 | app-server JSON-RPC | No |
| Cursor | 6 | ACP | No |
| Grok Build | 5 | ACP | No |
| OpenCode | 8 | HTTP/SSE v1+v2 (`provider/opencode/`) | No |
| Pi | 10 | RPC (`provider/pi.rs`) | No |
| omp | 1 | RPC | No |
| fx | 4 | ACP | No |
| Hermes Agent | 3 | ACP | No |
| Antigravity | 3 | ACP | No |
| Devin | 5 | ACP (`provider/devin.rs`, `provider/devin/jsonc.rs`) | No |

**In the desktop app, every provider is already invoked as a direct native executable.**
No `npx`, `npm`, or Node wrapper is used. This satisfies Phase 3.1 already.

The exception is the **remote host**, where the TypeScript adapters launch the CLIs,
and `host/cli.ts` injects Node into `PATH` (see §2.1). Porting `host/` to Rust removes
this and makes the remote path consistent with the local one.

### 4.3 Binary resolution

`host/src/integrations/harness/core/child.ts` (602 LOC) resolves provider binaries and
manages child processes, duplicating the Rust `provider/discovery.rs` and
`process.rs`. This is the clearest duplicated logic (see §7.1).

---

## 5. Shared TypeScript modules imported by the host

`host/src/` is a shared library of 130 files consumed by `host/server.ts`,
`host/engine.ts` and the providers. It splits into:

| Group | Files | Content |
| --- | --- | --- |
| `integrations/harness/core/` | 13 | `acp.ts`, `jsonRpc.ts`, `child.ts`, `apply.ts` (1,360 LOC), `preview.ts` (1,086), `shellIntent.ts` (640), `streamText.ts`, `types.ts`, `nativeCommands.ts`, `abortTextPrompt.ts`, `acpSubagents.ts`, `availabilityState.ts`, `providerNames.ts` |
| `integrations/harness/providers/*` | ~100 | The 11 provider adapters listed in §4.2 |
| `features/sessions/model/` | 16 | `session.ts` (911), `models.ts` (1,041), `transcriptActivity.ts` (1,222), `attachments.ts`, `composerRunner.ts`, `contextUsage.ts`, `handoff.ts`, `inFlight.ts`, `monocodeToolCall.ts`, `plan.ts`, `projectProviders.ts`, `sessionTitle.ts`, `sessionWorkItem.ts`, `taskList.ts`, `usageLimit.ts`, `userQuestion.ts` |
| `features/inbox/model/` | 8 | `githubTasks.ts` (1,300), `jira.ts`, `gitlab.ts`, `azureDevOps.ts`, `linear.ts`, `ciRepair.ts`, `inboxSeen.ts`, `inboxSelfActivity.ts` |
| `features/files/editor/` | 5 | `editorGit.ts` (1,383), `editorEditing.ts`, `editorLanguage.ts`, `editorLint.ts`, `editorMetrics.ts` |
| `features/settings/model/` | 3 | `settings.ts` (1,516), `storageFlags.ts`, `workspaceThemes.ts` (621) |
| `features/source-control/model/` | 4 | `gitText.ts`, `lineDiff.ts`, `unifiedDiff.ts`, `worktrees.ts` |
| `features/terminal/arcade/` | 4 | `pacmanArcade.ts` (1,150), `gridArcade.ts`, `gridGames.ts`, `snakeArcade.ts` |
| `features/workspace/model/` | 1 | `layout.ts` (1,435) |
| `features/connections/model/` | 1 | **`protocol.ts` — the host/client wire protocol** |
| `platform/native/` | 3 | `fs.ts` (641), `invoke.ts`, `platform.ts` |
| `shared/lib/` | 3 | `jsonText.ts`, `paths.ts`, `remotePaths.ts` |
| Other feature models | 8 | projects (3), providers (3), terminal (1), quick-composer (1), files (1) |

All of these are **host-internal**. The only cross-boundary artefact is
`features/connections/model/protocol.ts`, which defines the JSON-RPC contract also
implemented in Rust.

---

## 6. Functionality that must be preserved

Derived from `host/`'s public surface and the Rust client that consumes it.

### 6.1 Wire protocol

`HOST_PROTOCOL_VERSION = 1`. Transport: HTTP POST to `/rpc` plus `/lifecycle`.
Auth: device token (`devices` table, SHA-derived hash). **33 RPC methods**, matched
1:1 by `crates/monocode-core/src/remote.rs`:

```
environment.describe   models.list            projects.list        projects.open
projects.browse        sessions.list          sessions.sync        sessions.syncChunk
sessions.get           sessions.update        sessions.delete      events.read
files.list             files.read             files.write          files.create
files.index            files.search           files.searchContent  attachments.upload
attachments.read       git.action             git.branches         git.createBranch
git.diff               git.fileDiff           git.index            git.switch
git.worktreeCreate     git.worktrees          commands.dispatch    workspace.run
devices.revokeSelf
```

`git.arbitrary` appears in `crates/monocode-core/src/remote.rs:846` and `:925` only as a
**negative test assertion** (`assert!(!supported_remote_method("git.arbitrary"))`). It is
intentionally unsupported. Resolved — no action needed (see D4).

### 6.2 Features that must survive the port

- **Session sync protocol:** `unchanged` / `snapshot` / `delta` / `chunked` variants
  with `blockRevisions`, and chunked transfer via `sessions.syncChunk` for oversized
  deltas (`host/sync-transfer.ts`, `protocol.ts`).
- **Persistence:** `node:sqlite` `DatabaseSync` database at `~/.monocode-host/host.db`
  with tables `metadata`, `projects`, `sessions`, `receipts`, `events`, `devices`.
  Schema is **host-specific** and not shared with the desktop `monocode.db`; user data
  must be migrated, not discarded.
- **Receipts:** signed receipts in the `receipts` table.
- **Single-owner locking:** `host/owner.ts` prevents two hosts on one data directory.
- **Service management:** `host/service.ts` installs/uninstalls a **systemd user
  service**, provides `connection-info`, and manages the detached process.
- **CLI commands:** `pair`, `devices`, `revoke`, `service`, `connection-info`,
  `version`, `help`, plus `--data-dir` and `--port` (default 3774).
- **Provider catalog discovery** with a 5-minute `CATALOG_MAX_AGE_MS` cache.
- **`workspace.run` / `commands.dispatch`** — the `/operator` agent-control endpoints.

### 6.3 Verified: the desktop app works without `host/`

`crates/monocode-gpui` never spawns `build/host/monocode-host.mjs`. The only Rust
references are the remote-session SSH bootstrap
(`crates/monocode-core/src/remote/ssh.rs:353`, `remote.rs:308`) and UI copy
(`settings/pages/connections.rs`). Removing `host/` disables Remote Sessions only.

---

## 7. Duplicated logic between the Rust app and the TypeScript host

| Concern | Rust (core) | TypeScript (host) | Verdict |
| --- | --- | --- | --- |
| Provider discovery | `provider/discovery.rs` | `harness/core/child.ts` | **Duplicate.** Port host to reuse Rust. |
| Child process mgmt | `process.rs` | `harness/core/child.ts` | **Duplicate.** |
| ACP transport | `provider/acp.rs` (1,702) | `harness/core/acp.ts`, `jsonRpc.ts` | **Duplicate.** |
| OpenCode transport | `provider/opencode/` (client + events) | `harness/providers/opencode/opencodeClient.ts`, `opencodeProtocol.ts`, `opencodeV2Events.ts` | **Duplicate.** |
| Pi / omp RPC | `provider/pi.rs` | `harness/providers/pi/`, `omp/` | **Duplicate.** |
| Devin | `provider/devin.rs` | `harness/providers/devin/devinProtocol.ts` | **Duplicate.** |
| Event/transcript normalization | `transcript.rs`, `provider/text_tests.rs` | `harness/core/streamText.ts`, per-provider `*Text.ts` | **Duplicate.** |
| Tool-call application | `tool/` | `harness/core/apply.ts` (1,360) | **Duplicate.** |
| Preview / diff rendering | `diff.rs`, `response.rs` | `harness/core/preview.ts` (1,086) | **Duplicate.** |
| Shell-intent parsing | `provider/commands.rs` | `harness/core/shellIntent.ts` (640) | **Duplicate.** |
| Sessions & models | `provider/tasks.rs`, `catalog.rs` | `sessions/model/session.ts`, `models.ts` | **Duplicate.** |
| SQLite persistence | `store.rs` + `store/*` (rusqlite) | `host/store.ts` (`node:sqlite`) | **Different schemas.** Not duplicate, but two engines for one job. |
| Git | `git.rs` (692), `scm.rs` (1,386) | `git-branches.ts`, `git-worktrees.ts` | **Duplicate.** |
| Files/workspace | `files.rs`, `search.rs` | `workspace.ts` (851), `workspace-commands.ts` (810), `browse.ts` | **Duplicate.** |
| Inbox providers | `inbox/{github,gitlab,jira,azure_devops}.rs` | `features/inbox/model/*` | **Duplicate.** |
| Attachments | `remote/attachments.rs` | `host/attachments.ts` | **Duplicate.** |
| Protocol definitions | `remote.rs` | `connections/model/protocol.ts` | **Two definitions, one wire protocol.** Needs a single source of truth. |

**Conclusion:** porting `host/` to Rust is not a rewrite — it is mostly *deletion*
followed by *wiring to existing `monocode-core` services*. That is the cheapest path
and the one that satisfies "reuse existing Rust domain models wherever possible".

---

## 8. Module → replacement mapping

### 8.1 `host/` top level

| Module | LOC | Replacement | Notes |
| --- | --- | --- | --- |
| `host/cli.ts` | ~350 | Rust: `crates/monocode-host/src/main.rs` (clap-style arg parsing, no new dep) | Keep `pair`, `devices`, `revoke`, `service`, `connection-info`, `version`, `help`, `--data-dir`, `--port` |
| `host/server.ts` | 629 | Rust: Axum router, `/rpc` + `/lifecycle` | 33 methods; delegate to core |
| `host/engine.ts` | 926 | Reuse `monocode-core` `provider.rs` + `transcript.rs` + `store` | Session lifecycle, event ingestion |
| `host/store.ts` | ~200 | Reuse `monocode-core` `store/` (**rusqlite**) | Must import the existing `~/.monocode-host/host.db` schema, not create a new one |
| `host/providers.ts` | — | Reuse `crates/monocode-core/src/provider/` | Delete duplicate adapters |
| `host/process.ts` | — | Reuse `crates/monocode-core/src/process.rs` | |
| `host/child-backend.ts` | — | Reuse `monocode-core` provider lifecycle | Was the Node child bridge; not needed |
| `host/workspace.ts` | 851 | Reuse `files.rs`, `search.rs`, `scm.rs` | |
| `host/workspace-commands.ts` | 810 | Reuse `provider/commands.rs` | |
| `host/git-branches.ts` | — | Reuse `git.rs` / `scm.rs` | |
| `host/git-worktrees.ts` | — | Reuse `worktree_management.rs` | |
| `host/attachments.ts` | — | Reuse `remote/attachments.rs` | |
| `host/sync-transfer.ts` | — | Reuse `remote.rs` chunking | |
| `host/browse.ts` | — | Reuse `files.rs` directory listing | |
| `host/owner.ts` | — | Rust file lock (`flock`) | |
| `host/service.ts` | — | Bash: systemd user unit install/remove | Phase 6 |
| `host/runtime-download.mjs` | — | **Delete.** Forbidden by Phase 3.1 (no Node.js download) | |
| `host/provider-guard.mjs` | — | **Delete.** Only needed to guard the Node host | |
| `host/build.mjs` | 20 | **Delete.** esbuild bundling is unnecessary for a Rust binary | |
| `host/package.mjs` | — | Rust `xtask` or Bash | |
| `host/env.d.ts`, `host/tsconfig.json`, `host/vitest.config.ts` | — | **Delete** | |

### 8.2 `host/src/` shared modules

| Module group | Replacement |
| --- | --- |
| `features/connections/model/protocol.ts` | Rust types in `crates/monocode-core/src/remote/protocol.rs`; generate from one source of truth so client and host cannot drift |
| `integrations/harness/core/*` | `crates/monocode-core/src/provider/{acp,discovery,lifecycle,commands}.rs` |
| `integrations/harness/providers/*` | `crates/monocode-core/src/provider/*` (already present for all 11) |
| `features/sessions/model/*` | `crates/monocode-core/src/{provider/attachments,transcript,session_title,provider/questions,provider/tasks}.rs` |
| `features/inbox/model/*` | `crates/monocode-core/src/inbox/*.rs` |
| `features/files/editor/*` | `crates/monocode-gpui/src/{file_editor,editor_syntax,editor_analysis}.rs` + tree-sitter |
| `features/settings/model/*` | `crates/monocode-core/src/{desktop_preferences,agent}.rs` |
| `features/source-control/model/*` | `crates/monocode-core/src/{diff,scm}.rs` |
| `features/terminal/arcade/*` | **Keep** as generated `assets/settings-arcade.js` for QuickJS (see §7) |
| `features/workspace/model/layout.ts` | `crates/monocode-gpui/src/{pane_layout,workspace}.rs` |
| `platform/native/*` | Native Rust std calls; **delete** |
| `shared/lib/*` | `std::path`, `serde_json`; **delete** |

### 8.3 `scripts/`

| Script | Replacement |
| --- | --- |
| `build-native.mjs` | `cargo build --release -p monocode-gpui` (or 5-line Bash) |
| `dev-native.mjs` | Rust `xtask dev` (rebuild + restart loop) |
| `bump-version.mjs` | Rust `xtask set-version` |
| `package-native.mjs` | Rust `xtask package` (deb/rpm/AppImage) |
| `native-release.mjs` | Rust `xtask release` |
| `dev-native.test.mjs`, `native-dependencies.test.mjs`, `native-release.test.mjs`, `package-native.test.mjs` | Convert to Rust `#[test]` in the xtask crate |
| `native-editor/build.mjs` + `format.ts` + `index.ts` + `package.json` | **Decision required** — see §9 |
| `native-settings/build-arcade.mjs` | **Decision required** — see §9 |
| `test-remote-ssh.py` | Keep as Python (permitted; Phase 7 allows Python where it has an advantage) |
| `scripts/perf` | Keep; verify contents |

### 8.4 Bundled JavaScript assets

| Asset | Consumer | Replacement option |
| --- | --- | --- |
| `assets/native-editor.js` (1.2 MB) | `editor_analysis.rs` | tree-sitter grammars (already 18 present) + Rust formatters |
| `assets/native-format.js` (2.1 MB) | `editor_analysis.rs` | `dprint`/`stylua`-style Rust formatters, or shell out to user-installed formatters |
| `assets/settings-arcade.js` (22 KB) | `arcade.rs`, `composer_runner.rs` | Rust rewrite of the arcade renderer |

---

## 9. Decisions required before Phase 2

These change scope materially and should be resolved before implementation.

### D1. Remote host: port to Rust, or drop the feature?

- **Port to Rust (recommended).** Keeps Remote Sessions working, removes the only
  runtime Node.js dependency, and reuses ~80 % of `monocode-core`. Cost: an Axum HTTP
  server (a new dependency family) and a second binary.
  - Pro: meets the migration's stated goal without losing a feature.
  - Con: introduces Tokio + Axum; the rest of the codebase is currently sync.
- **Drop Remote Sessions.** Deletes `host/` immediately (169 files gone).
  - Pro: fastest route to "no Node.js".
  - Con: loses a shipped feature; violates Guideline 2.

### D2. Async runtime

`migration.md` mandates Tokio + Axum. The existing Rust code has **neither** and is
synchronous. Options:

- **Tokio + Axum only in the new host crate.** Isolates the change; core stays sync.
  - Pro: no ripple through 73k LOC of working code.
  - Con: two concurrency styles in one repo.
- **Tokio across the workspace.**
  - Pro: uniform.
  - Con: very large refactor of working, tested code — high regression risk.

### D3. Embedded CodeMirror/Prettier bundles

- **Replace with Rust (tree-sitter + native formatters).** Removes 3.3 MB of JS assets
  and drops `rquickjs` from the editor path.
  - Pro: true "no JavaScript".
  - Con: CodeMirror provides language analysis breadth (PHP, Vue, Svelte, Less, Sass,
    SQL, XML) that tree-sitter grammars would need to reproduce; large effort.
- **Keep QuickJS bundles, generated at build time by npm.**
  - Pro: zero functional risk; no runtime Node.js.
  - Con: repo keeps `*.js` assets and a build-time npm step, which conflicts with
    Phase 9's "remove all `*.js` project source files" unless documented as an
    exception (Phase 9 does allow documented exceptions).

### D4. `git.arbitrary` — resolved

Investigated: `crates/monocode-core/src/remote.rs:925` asserts
`!supported_remote_method("git.arbitrary")`. The method is **deliberately unsupported**
and the host correctly omits it. No action required; the port must simply not add it.

---

## 10. What is already complete

To avoid redoing finished work:

| Phase item | Status | Evidence |
| --- | --- | --- |
| Rust desktop app | Done | `crates/monocode-gpui` (GPUI, Wayland) |
| Rust domain models | Done | `crates/monocode-core`, 73,343 LOC |
| Provider adapters (local) | Done | All 11 providers in `provider/` |
| Native process mgmt (local) | Done | `process.rs`, `provider/lifecycle.rs` |
| Session engine (local) | Done | `transcript.rs`, `store/`, `checkpoint.rs` |
| SQLite persistence (local) | Done | `rusqlite` in `store.rs` |
| Workspace/Git (local) | Done | `files.rs`, `git.rs`, `scm.rs`, `worktree_management.rs` |
| No-Tauri | Done | README: "The former React/Tauri app is no longer part of the repository" |
| Bash/Python tooling | Partial | `scripts/test-remote-ssh.py` already Python |
| Node.js removal | **Not done** | `host/`, `scripts/`, npm, 3 CI workflows |

---

## 11. Recommended migration order

1. **Resolve D1–D4.**
2. **Port `host/` to `crates/monocode-host`** (Phases 2.2, 3, 4, 5, 6 for the remote
   role). Reuse `monocode-core`; preserve the `host.db` schema and all 33 RPC methods.
   Verify with the existing Rust remote client against a fake provider executable.
3. **Port `host/` tests to Rust.** 24 TS test files; `engine.test.ts`, `server.test.ts`,
   `store.ts` recovery, `child-backend.test.ts`, `process.test.ts`, and the git/workspace
   suites carry real behavioural contracts.
4. **Replace `scripts/*.mjs`** with a Rust `xtask` + Bash; then fix CI.
5. **Decide D3** and remove or document the JS assets.
6. **Delete** `package.json`, `package-lock.json`, `host/`, `node_modules/`, esbuild,
   Vitest, `tsconfig`, `vitest.config.ts` (Phase 9).
7. **Validate** on a clean Linux box without Node.js (Phase 10).

---

## 12. Reference: full `host/` top-level inventory

Non-test runtime files (20):

```
host/attachments.ts      host/git-branches.ts       host/runtime-download.mjs
host/browse.ts           host/git-worktrees.ts      host/server.ts
host/build.mjs           host/owner.ts              host/service.ts
host/child-backend.ts    host/package.mjs           host/store.ts
host/cli.ts              host/process.ts            host/sync-transfer.ts
host/engine.ts           host/provider-guard.mjs    host/workspace-commands.ts
host/providers.ts        host/workspace.ts
```

Test files (24): `attachments`, `bootstrap`, `browse`, `child-backend`, `cli`, `engine`,
`git-branches`, `git-worktrees`, `opencode-transport`, `opencode-v2-transport`, `owner`,
`process`, `providers`, `provider-transport`, `server`, `service`, `sync-transfer`,
`workspace-commands`, `workspace` (`.test.ts`); plus `runtime-download.test.mjs` and
four `scripts/*.test.mjs`.
