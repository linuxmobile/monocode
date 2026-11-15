# Native architecture

MonoCode is a Rust + GPUI desktop app targeting Linux on Wayland. The former
React/Tauri implementation has been removed; the native app is the only desktop
client.

## Code layout

| Location | Responsibility |
| --- | --- |
| `crates/monocode-core/` | Provider transports, persistence, accounts, Git/files, remote connections and shared services |
| `crates/monocode-gpui/` | Native shell, composer, transcript, editor, settings and platform UI |
| `crates/monocode-gpui/Cargo.toml` | Independent native Cargo workspace, profiles and dependency patches |
| `host/` | Headless remote host and the shared TypeScript models it serves |
| `crates/monocode-host/` | Native replacement for `host/`: Axum server, device pairing, host database |
| `scripts/dev-native.mjs` | Development build and manual restart runner |
| `scripts/build-native.mjs` | Optimized build |
| `scripts/native-editor/`, `scripts/native-settings/` | Generators for embedded JavaScript assets |
| `vendor/gpui-pre*/` | Pinned framework and the Linux platform patch |

`monocode-core` has no GPUI dependency. Keep provider and storage behavior in
core services, with UI work in GPUI views. Development uses isolated data;
release builds use the installed desktop database. See the
[native guide](README.md#data-and-configuration) for paths and commands.

### Crate boundaries

The split is enforced, not conventional:

- **`monocode-core`** is UI-independent. It must not import `gpui`, depend on the
  GPUI workspace, or reach for a window, view or entity type. Anything that
  needs to draw belongs in the GPUI crate.
- **`monocode-gpui`** owns presentation and platform integration. It calls
  `monocode_core::…` services; core must never call back into it.
- **Shared data and protocols** live in `monocode-core`, serialized with Serde.
  Do not restate a type in the app or a host to match core: import it.
- **`host/`** is the headless remote server. It is divided from core by a
  versioned JSON-RPC contract (`HOST_PROTOCOL_VERSION`), not by shared code.

When a new protocol field or enum is needed, add it once in core and derive
`Serialize`/`Deserialize` there. Duplicating a wire type is how the client and
host drift apart.

## Logging and instrumentation

One `log` facade spans both crates. `monocode-core` depends on `log` and the app
installs the single sink, so a message from a provider transport and a message
from GPUI reach the same file with the same timestamp and target.

- Log files live under `$XDG_STATE_HOME/monocode/`, falling back to
  `~/.local/state/monocode/`. Logs are state, not user data.
- The sink currently records warnings and above, with per-message rate limiting
  so a failing draw does not write a line per frame. Raise the filter with
  `log::set_max_level` when diagnosing.
- Call `log::warn!`/`log::error!` with the failing operation and its subject
  (session id, provider, path). Never log secrets, tokens or transcript text.
- Use a target that names the module (`monocode_core::provider`) so a log line
  identifies its origin.

## Error handling

Services return `Result<T, String>` today; 625 signatures use it. Keep that
convention inside core until a crate-wide migration to typed errors is decided:

- `Err(String)` carries a message written for a user, not a debug dump.
- Convert foreign errors at the boundary with `.map_err(|e| e.to_string())`, so
  no `std::io::Error` or provider error type leaks into the public API.
- Do not panic on recoverable conditions. `expect`/`unwrap` are for invariants
  that a caller physically cannot violate, and for tests.
- `thiserror` is listed in the workspace dependencies but is not yet used. Do
  not add it piecemeal; adopt it crate by crate only if typed errors become a
  requirement.

Provider transports use Codex app-server, Claude stream-json, OpenCode HTTP/SSE,
ACP for Cursor/Grok/Antigravity/fx/Hermes/Devin, and Pi/omp RPC. They reuse the
providers' CLI authentication and conversation IDs.

## The remote host

`crates/monocode-host` serves one machine to a paired desktop. It replaces the
TypeScript `host/` and speaks the same protocol version 1, so
`monocode_core::remote` is the reference client for any change here.

- **Transport.** Axum over Tokio, bound to loopback. `POST /rpc` carries the
  protocol; `POST /lifecycle` is local administration (`status`, `stop`) and
  requires the secret from `running.json`, which is deliberately not a paired
  device credential.
- **Security.** Any request carrying an `Origin` header is refused, only those
  two paths exist, and every call needs a paired token. Reading the body
  re-authenticates, because a device can be revoked mid-request.
- **Data.** `<data-dir>/host.db`, schema-compatible with the previous host so
  an existing database and its `environmentId` survive the migration. Tokens
  are stored only as SHA-256 hashes; `pair` is the one moment the token exists
  in clear.
- **Honesty about scope.** Methods in `methods::METHODS` are served and are
  advertised as capabilities. Methods still being ported are listed in
  `UNIMPLEMENTED` and answer `The host cannot serve … yet`. A test asserts the
  two lists never overlap and that every advertised method has a handler, so a
  capability cannot claim more than the server does.
- **Shutdown.** SIGTERM, Ctrl-C, and `lifecycle`/`stop` all drain connections,
  remove `running.json`, and exit; there is no `kill -9` path.

Startup, port and pairing match the previous host: `monocode-host serve`
listens on `127.0.0.1:3774` by default, and `--data-dir` moves the state.

## Process management

Provider children are owned by `monocode_core::process`. It is synchronous on
purpose: provider I/O is stdin/stdout over a pipe, so a dedicated reader, writer
and stderr thread per child is cheaper and simpler than moving the whole core
onto an async runtime. The host keeps its async runtime and moves only blocking
work to the blocking pool.

- `isolate` puts each child in its own process group and clears the inherited
  signal mask. A GUI executor can leave `SIGCHLD` blocked, and `exec` preserves
  that mask, so a provider started under it would never reap its own children.
- Closing stdin is the polite stop. `retire` then waits `EXIT_GRACE`, sends
  `SIGTERM` to the leader, waits `TERM_GRACE`, and finally `SIGKILL`s the group.
  The windows exist because Claude Code writes a rotated OAuth token on exit;
  killing it early loses the token and signs the user out.
- A child that calls `setsid` forms its own group and is outside that guarantee
  by construction. Everything else in the leader's group dies with it;
  `shutdown_all` runs the same escalation for every registered child on quit.
- No provider is launched through a shell. `Command::new` receives the binary
  and an argument vector; the only `-c` in the tree is git's own flag.
- Core runs only fixed argument lists (`models --json`, `status --json`) and
  never executes caller-supplied arguments, so it needs no exec allowlist. An
  allowlist belongs wherever untrusted arguments arrive, which is the host, and
  one must exist before the host exposes provider execution.

## Rendering and responsiveness

Keep provider I/O, parsing, file access and persistence off the UI thread.
Composer input should not rebuild the transcript. Transcript rows have stable
identities, bounded blocks and virtualized layout; streaming updates should
preserve the reader's position. Cancellation must remain responsive under load.

The editor embeds generated analysis and formatting bundles, and the settings
screen embeds a generated game bundle. These are committed build artifacts: the
app embeds them with `include_str!`, so nothing needs regenerating to build,
test or release.

Their generators (`scripts/native-editor/`, `scripts/native-settings/`) still
require Node, npm and esbuild, and still read TypeScript sources from `host/src`.
They are the last Node dependency in the repository and are removed together with
`host/` rather than ported, because the sources they bundle disappear at the same
time. Until then, regenerating an asset needs Node; building and shipping the app
does not.

## Task runner

Build and release orchestration lives in the `xtask` crate:

```
cargo xtask set-version 0.17.0
cargo xtask dev                 # build, run, and rebuild when you type r
cargo xtask build --release
cargo xtask package linux <binary> <out-dir>
cargo xtask release <directory>
```

`xtask` parses and writes structured files (Cargo manifests, artifact indexes,
the update manifest), where a real parser beats a shell script. Straightforward
Linux work is plain Bash: `scripts/install.sh` and `scripts/uninstall.sh` copy
files under `~/.local` and never need root.

## Framework patches

GPUI and its platform crate are pinned to `gpui-pre` 0.3.9. The local renderer
patch covers alpha blending, font matching, backdrop blur and shadows.

The vendored framework/platform crates remove the unused `zed-scap` dependency.
The native build rejects lockfiles that reintroduce it, and the `screen-capture`
feature remains disabled. Preserve these guards and the licenses in
[NOTICE](../../NOTICE).

See [preview limitations](parity.md) for remaining work.
