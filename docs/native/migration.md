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
| `crates/monocode-host/` | *(planned)* Native replacement for `host/`; see [`docs/migration-audit.md`](../migration-audit.md) |
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

## Rendering and responsiveness

Keep provider I/O, parsing, file access and persistence off the UI thread.
Composer input should not rebuild the transcript. Transcript rows have stable
identities, bounded blocks and virtualized layout; streaming updates should
preserve the reader's position. Cancellation must remain responsive under load.

The editor embeds generated analysis and formatting bundles. After changing their
source services or dependencies, run `npm ci` and `npm run build` inside
`scripts/native-editor/`. Generated bundles and their license notices belong
in the repository.

## Framework patches

GPUI and its platform crate are pinned to `gpui-pre` 0.3.9. The local renderer
patch covers alpha blending, font matching, backdrop blur and shadows.

The vendored framework/platform crates remove the unused `zed-scap` dependency.
The native build rejects lockfiles that reintroduce it, and the `screen-capture`
feature remains disabled. Preserve these guards and the licenses in
[NOTICE](../../NOTICE).

See [preview limitations](parity.md) for remaining work.
