import assert from "node:assert/strict";
import { chmod, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { packageNative, previewVersion } from "./package-native.mjs";

test("Linux preview packaging emits the deb and the AppImage", async (t) => {
  const directory = await mkdtemp(join(tmpdir(), "native packaging with spaces-"));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const tools = join(directory, "tools");
  const release = join(directory, "target/release");
  await mkdir(tools);
  await mkdir(release, { recursive: true });
  const binary = join(release, "monocode-gpui");
  await writeFile(
    binary,
    "#!/bin/sh\nprintf 'Monocode Native 0.16.0-preview.2\\n'\n",
  );
  await chmod(binary, 0o755);
  // The packaging tools are not installed here, so stand-ins write the files
  // the script expects and record what they were asked to do.
  for (const name of ["dpkg-deb", "appimagetool"]) {
    const tool = join(tools, name);
    await writeFile(
      tool,
      `#!/usr/bin/env node
const fs = require('node:fs');
const path = require('node:path');
const name = path.basename(process.argv[1]);
const args = process.argv.slice(2);
if (name === 'dpkg-deb') fs.writeFileSync(args.at(-1), 'deb');
if (name === 'appimagetool') fs.writeFileSync(args.at(-1), 'appimage');
`,
    );
    await chmod(tool, 0o755);
  }
  const out = join(directory, "output");
  const originalPath = process.env.PATH;
  t.after(() => {
    process.env.PATH = originalPath;
  });
  process.env.PATH = `${tools}:${originalPath}`;

  const artifacts = await packageNative({
    kind: "linux",
    binary,
    out,
    version: "0.16.0-preview.2",
    arch: "x86_64",
  });
  assert.equal(artifacts.length, 2);
  assert.ok(
    artifacts.some(
      (artifact) =>
        artifact.target === "linux-x86_64-deb" &&
        artifact.filename === "monocode-native_0.16.0-preview.2_amd64.deb",
    ),
  );
  assert.ok(
    artifacts.some(
      (artifact) =>
        artifact.target === "linux-x86_64-appimage" &&
        artifact.filename.endsWith(".AppImage"),
    ),
  );
  const metadata = JSON.parse(
    await readFile(join(out, "artifacts-linux-x86_64.json"), "utf8"),
  );
  assert.equal(metadata.length, 2);
});

test("the package refuses a binary that reports another version", async (t) => {
  const directory = await mkdtemp(join(tmpdir(), "native packaging with spaces-"));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const binary = join(directory, "monocode-gpui");
  await writeFile(binary, "#!/bin/sh\nprintf 'Monocode Native 0.16.0-preview.3\\n'\n");
  await chmod(binary, 0o755);
  await assert.rejects(
    () =>
      packageNative({
        kind: "linux",
        binary,
        out: join(directory, "out"),
        version: "0.16.0-preview.2",
        arch: "x86_64",
      }),
    /executable version does not match/,
  );
});

test("preview versions keep the package version fields", () => {
  assert.equal(previewVersion("0.16.0-preview.2").base, "0.16.0");
  assert.equal(previewVersion("0.16.0-preview.2").sequence, "2");
  assert.throws(() => previewVersion("0.16.0"));
});
