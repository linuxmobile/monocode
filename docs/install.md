# Linux installation

MonoCode targets Linux on Wayland. Install the packaged `.deb`, `.rpm` or
AppImage from the native preview [GitHub Releases](https://github.com/hardbeat920/monocode/releases),
then sign in to at least one [supported provider](providers.md).

## Ubuntu / Debian

Install the downloaded package with apt so its runtime dependencies are
installed too:

```bash
sudo apt install ./MonoCodeNative_*.deb
```

## Fedora / Enterprise Linux

```bash
sudo dnf install ./MonoCodeNative-*.rpm
```

## AppImage

Keep the AppImage somewhere you can write to, such as `~/Applications`, make it
executable, and run it:

```bash
chmod +x MonoCodeNative_*.AppImage
./MonoCodeNative_*.AppImage
```

## Wayland

Run MonoCode from a Wayland session. The app uses the system compositor and does
not require XWayland. If your compositor session is X11-only, start a Wayland
session before launching the app.

For local build dependencies and packaging commands, see
[building from source](building.md).
