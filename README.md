<h1 align="center">Monocode</h1>

<p align="center">
  <strong>A GUI for your coding agents.</strong>
</p>

<p align="center">
  <img width="1680" height="1050" alt="MonoCode with agent sessions open side by side" src="https://github.com/user-attachments/assets/89e04066-69de-473d-9aab-88b9ca3e3b7b" />
</p>

> [!NOTE]
> This branch is the Linux Wayland build, written in Rust with GPUI. The former
> React/Tauri app is no longer part of the repository.

Works with your subscriptions on Claude Code, Codex, Cursor, Grok Build, OpenCode, Antigravity, Pi, omp, fx, Hermes Agent, and Devin. If they’re installed and logged in, MonoCode can run them. Tabs are sessions. The composer is the input. MonoCode does not sell tokens.

## Install

Install and sign in to at least one supported agent first. See [provider setup](docs/providers.md) for instructions.

Download the Linux `.deb`, `.rpm`, or AppImage from the
[native preview releases](https://github.com/hardbeat920/monocode/releases).
See [Linux setup](docs/install.md) for dependencies and package instructions.

## Get started

Open a project folder, choose an agent, and send a message. Each tab is a session. Split panes to work with sessions side by side.

- Keep conversations organized by project.
- Use separate Git worktrees for parallel tasks.
- Edit files and review changes in the app.
- Start a message with `/operator` to let an agent open sessions and manage worktrees and notes.

Monos are experimental agents that work across your projects. They keep a memory and can run scheduled tasks you set up with them.

MonoCode is still early. If something breaks, [report a bug](https://github.com/hardbeat920/monocode/issues).

## Guides

- [Provider setup](docs/providers.md)
- [Linux installation](docs/install.md)
- [Agent access with /operator](docs/agent-access.md)
- [Remote sessions, experimental](docs/remote-access.md)
- [Native architecture](docs/native/migration.md)
- [Known limitations](docs/native/parity.md)

## Build from source

You’ll need Node.js 22+ and a stable Rust toolchain on Linux with Wayland. See
[build setup](docs/building.md) for platform dependencies and packaging.

The mobile app is a separate Expo project in [`mobile/`](mobile/README.md). Run it with `cd mobile && npm install && npm start`.

```bash
npm run native:dev     # launch with a separate development database
npm run native:build   # create an optimized build
npm run native:check   # run native checks
```

Type `r` then Enter in the development terminal to rebuild and restart; there is
no hot reload. The release app saves changes to your installed MonoCode data.

Small, focused contributions are welcome. Read [CONTRIBUTING.md](CONTRIBUTING.md) before opening a pull request.

## Contributors

Thanks to everyone who contributes to MonoCode!

[![MonoCode contributors](https://contrib.rocks/image?repo=hardbeat920/monocode)](https://github.com/hardbeat920/monocode/graphs/contributors)

Contributor image by [contrib.rocks](https://contrib.rocks).

## License

[MIT](LICENSE). Provider names and logos are trademarks of their owners. See [NOTICE](NOTICE).
