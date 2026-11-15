# Build from source

MonoCode is a Rust + GPUI desktop app. This branch targets Linux on Wayland.
You need a current stable Rust toolchain and at least one agent CLI installed and
signed in using [provider setup](providers.md).

Node.js is not needed for anything: not building, testing, installing, releasing,
or running the remote host. Everything is Rust, Cargo and Bash.

The GPUI app has its own Cargo workspace and lockfile under
`crates/monocode-gpui/`.

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
cargo xtask dev      # launch with a separate development database
```

To open a particular project:

```bash
cargo xtask dev -- --cwd /path/to/project
```

Type `r` then Enter in the terminal to rebuild and restart. The current app stays
open until the new build succeeds. Ctrl+C stops the app and runner. There is no
hot reload or automatic rebuild on save.

## Build

```bash
cargo xtask build --release
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
cargo test --locked --manifest-path xtask/Cargo.toml
cargo test --locked --manifest-path crates/monocode-gpui/Cargo.toml -p monocode-core --profile native-dev
cargo test --locked --manifest-path crates/monocode-gpui/Cargo.toml -p monocode-gpui --profile native-dev -- --test-threads=1
cargo test --locked --manifest-path crates/monocode-gpui/Cargo.toml -p monocode-host --profile native-dev
cargo fmt --manifest-path crates/monocode-gpui/Cargo.toml --all --check
cargo clippy --locked --manifest-path crates/monocode-gpui/Cargo.toml \
    -p monocode-core -p monocode-gpui -p monocode-host --all-targets --profile native-dev -- -D warnings
```

`scripts/perf/baseline.sh` records performance numbers for comparison over time.

## Remote host

The headless remote host is `crates/monocode-host`, a Rust binary built from the
same workspace. On the machine you want to reach:

```bash
cargo xtask build --release
target/release/monocode-host install    # systemd user service, no root needed
target/release/monocode-host pair       # issue a credential for a desktop
```

See [remote access](remote-access.md) for pairing, lifecycle commands and
deployment.

## Installing

```bash
./scripts/install.sh       # files under ~/.local, no root
./scripts/uninstall.sh     # keeps your data unless --purge
```

Desktop entries, icons and licence notices are installed alongside the binary.
