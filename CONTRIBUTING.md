# Contributing

MonoCode is early and I’m the only maintainer, so small and focused lands much faster than large and ambitious. Past that, the door is open - bug reports and fixes are genuinely welcome.

Please don’t open PRs that add a new provider right now. The existing harnesses still need to agree on a few patterns, and a new adapter would copy whatever is there today. See [New providers](#new-providers).

## Project scope

MonoCode is now a Rust + GPUI desktop app targeting Linux on Wayland. The former
React/Tauri application has been removed from the repository. Keep changes
focused on `crates/monocode-core`, `crates/monocode-gpui` and the headless
remote host in `host/`.

Before starting a larger change, open an issue so we can agree on the scope and
avoid duplicated work.

## Get it running

You need a current stable Rust toolchain and at least one provider CLI installed
and logged in. Node.js is not needed. See [provider setup](docs/providers.md) for
installation and login instructions.

See [building from source](docs/building.md) for Linux/Wayland prerequisites and
packaging commands.

```bash
cargo xtask dev
```

## Where things live

- `crates/monocode-core/` - provider transports, persistence, accounts, Git/files, remote connections and shared services
- `crates/monocode-gpui/` - native shell, composer, transcript, editor, settings and platform UI
- `host/` - the headless remote host, its transport code and shared TypeScript models
- `xtask/` - build, packaging, release and dev-loop orchestration
- `scripts/` - the Bash installer and the embedded-asset generators

Provider protocol work belongs in `crates/monocode-core`. The native UI lives in
`crates/monocode-gpui`; keep provider I/O, parsing, file access and persistence
off the UI thread.

## Embedded editor and settings assets

The editor, formatter and settings arcade embed JavaScript bundles that are
committed in `crates/monocode-gpui/assets/`. They are embedded data, not build
source: nothing in the build, test or release path regenerates them, and no
Node.js is involved. See [embedded assets](docs/native/embedded-assets.md) for
what each one is, how it is wired to Rust, and what to do if one must change.

## Before you push

```bash
cargo test --locked --manifest-path xtask/Cargo.toml
cargo fmt --manifest-path crates/monocode-gpui/Cargo.toml --all --check
cargo clippy --locked --manifest-path crates/monocode-gpui/Cargo.toml \
    -p monocode-core -p monocode-gpui --all-targets --profile native-dev -- -D warnings
cargo test --locked --manifest-path crates/monocode-gpui/Cargo.toml -p monocode-core --profile native-dev
cargo test --locked --manifest-path crates/monocode-gpui/Cargo.toml -p monocode-gpui --profile native-dev -- --test-threads=1
```

All of those are Rust. There is no Node.js anywhere in the repository, so no
step of building, testing, installing or releasing needs it.

## New providers

I’m pausing new harnesses until the current ones share the same patterns -
session lifecycle, catalog probes, usage, approvals, and how slash commands and
skills are wired. A PR that adds another provider will be closed for now, even
if the work is good. Fixes, tests, and protocol bugs on Claude, Codex, Cursor,
Grok, OpenCode, Antigravity, Pi, omp, fx, Hermes Agent, and Devin are still the
best kind of contribution.

When the pause lifts, this section goes away.

## Pull requests

Keep a PR to one thing, and say what changed and why. The [PR template](.github/pull_request_template.md) covers the rest. If it changes the UI, a before/after screenshot helps a lot.

For anything that moves product direction - a new surface, new provider behavior, a refactor that changes the shape of the app - open an issue first. That’s not gatekeeping, I’d just rather you hear “I’m already halfway through that” before you write it than after. New providers are the exception: don’t send the adapter, even from an issue, until the pause above is gone.

I might close a PR, ask you to shrink it, or end up implementing the idea differently. That’s a call about scope and timing, not about you or the quality of your work.

Be kind: [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md). Security reports: [SECURITY.md](SECURITY.md).
