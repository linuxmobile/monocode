# Embedded JavaScript assets

Four files in `crates/monocode-gpui/assets/` are JavaScript. They are **embedded
data**, not build source: the app loads them with `include_str!` into its own
QuickJS runtime at run time. Nothing in the build, test, install or release path
needs Node.js to use them, and they are committed so those paths never
regenerate anything.

| File | Size | Embedded by | Purpose |
| --- | --- | --- | --- |
| `native-editor.js` | 1.2 MB | `src/editor_analysis.rs` | CodeMirror editor bundle |
| `native-format.js` | 2.1 MB | `src/editor_analysis.rs` | Prettier formatter bundle |
| `settings-arcade.js` | 24 KB | `src/arcade.rs`, `src/composer_runner.rs` | Grid games and the composer runner |
| `editor-palettes.json` | 8 KB | `src/theme.rs` | Editor syntax-colour palettes |

## Why these are vendored rather than generated

They were produced by `scripts/native-editor/build.mjs` and
`scripts/native-settings/build-arcade.mjs`, which bundled TypeScript from
`host/src` with esbuild. `host/` is deleted, so those generators can no longer
run and the sources they bundled no longer exist.

Keeping the generators would have preserved a Node.js, npm and esbuild
dependency, plus a dependency on TypeScript sources that were themselves
deleted. The alternative — reimplementing esbuild and a TypeScript bundler —
would be a large amount of machinery for four assets that change rarely.

So they are treated the way any vendored third-party bundle is: committed,
documented, and replaced deliberately if that ever becomes necessary.

## Regenerating an asset

There is no longer a generator in this repository. If one of these needs to
change, the options, in order of preference:

1. Reimplement the needed behaviour in Rust. The formatter and the syntax
   highlighter are the likeliest candidates, and both have cheaper native
   alternatives than shipping a JavaScript engine bundle.
2. Rebuild it out of tree with the upstream tools (CodeMirror, Prettier, esbuild)
   against the interfaces in `src/editor_analysis.rs` and `src/arcade.rs`, then
   commit the result and its licence alongside the existing files.

Either way the licence notices must ship with it. They are already installed by
`scripts/install.sh` and packaged by `cargo xtask package`:

- `native-editor.LICENSE.txt` (CodeMirror and its extensions)
- `hugeicons.LICENSE`
- `dark-plus.LICENSE`
- `material/LICENSE`, installed as `material-icons.LICENSE`

## Interface contract

Each bundle exposes a global that Rust calls into. Changing one must keep that
global intact:

- `native-editor.js` — analysis and syntax data used by `editor_analysis.rs`
- `native-format.js` — formatting entrypoint used by `editor_analysis.rs`
- `settings-arcade.js` — `NativeArcade`, with `frame(...)` and `runnerFrame(...)`
- `editor-palettes.json` — palette objects deserialized by `theme.rs`

The tests in `editor_analysis.rs`, `arcade.rs`, `composer_runner.rs` and
`theme.rs` exercise these through the real bundles, so a broken replacement
fails the suite rather than shipping quietly.

## The other two JavaScript files

`crates/monocode-core/src/visual/sdk.js` and the examples in
`docs/native/examples/` are also embedded data, in the same sense:

- `visual/sdk.js` is evaluated into the QuickJS context that runs user-authored
  visual programs (`src/visual.rs`). It defines the `visual` API and the sandbox
  that removes `Atomics` and `SharedArrayBuffer`. It is part of the product's
  public surface, not a build tool.
- `docs/native/examples/visual-dashboard.js` and `visual-scene.js` are sample
  programs that `src/fixtures.rs` embeds as demonstration content.

Writing a visual program in JavaScript is a feature, so JavaScript appearing
there is expected. It is unaffected by the removal of Node.js.