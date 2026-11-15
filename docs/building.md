# Build from source

MonoCode is a Rust + GPUI desktop app. This branch targets Linux on Wayland.
You need Node.js 22+, a current stable Rust toolchain, and at least one agent CLI
installed and signed in using [provider setup](providers.md).

The GPUI app has its own Cargo workspace and lockfile under
`crates/monocode-gpui/`. It does not need the repository's npm dependencies for a
normal build.

## Linux prerequisites

Install a C toolchain plus the Wayland, font, audio and D-Bus development
headers. On Debian/Ubuntu:

```bash
sudo apt-get update
sudo apt-get install --no-install-recommends -y \
  build-essential clang cmake ninja-build pkg-config \
  libfontconfig1-dev libfreetype6-dev libxkbcommon-dev \
  libwayland-dev libegl1-mesa-dev libvulkan-dev \
  libasound2-dev libdbus-1-dev
```

Run under a Wayland session. Wayland is the supported display path on this
branch.

## Run locally

From the repository root:

```bash
npm install          # only needed for host checks and bundled asset generators
npm run native:dev   # launch with a separate development database
```

To open a particular project:

```bash
npm run native:dev -- --cwd /path/to/project
```

Type `r` then Enter in the terminal to rebuild and restart. The current app stays
open until the new build succeeds. Ctrl+C stops the app and runner. There is no
hot reload or automatic rebuild on save.

## Build

```bash
npm run native:build
```

The executable is written under
`crates/monocode-gpui/target/release/monocode-gpui`. The release app reads and
writes your installed MonoCode data, so back up your data with the app closed
before testing.

Linux packaging for this branch is built by the
[native release workflow](../.github/workflows/native-release.yml) and produces
`.deb`, `.rpm` and AppImage artifacts. See [Linux installation](install.md).

## Checks

```bash
npm run check
```

This runs the host typecheck and tests, the native script tests, Rust formatting,
strict Clippy and the native test suites. Use `npm run check:host` or
`npm run native:check` to run one half.

## Remote host

The headless remote host lives in [`host/`](../host/) and is built from the same
repository. See [remote access](remote-access.md) for its packaging and
deployment instructions.
