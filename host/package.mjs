import { createHash } from "node:crypto";
import { mkdir, readFile, writeFile, chmod, copyFile, rm } from "node:fs/promises";
import { execFileSync } from "node:child_process";
import { resolve, join } from "node:path";
import { ensureRuntimeArchive } from "./runtime-download.mjs";

// Official Node release checksums, pinned so release builds are reproducible.
// https://nodejs.org/dist/v24.21.0/SHASUMS256.txt
const nodeVersion = "24.21.0";
const runtimes = {
  "linux-arm64":
    "724282c3b43aec998aa9527380465b45d229e021b58035f5f4f63095eabfe5d5",
  "linux-x64":
    "6e1db87ef58b8819e5d5402eff1536491b18edd8eb7bee5ef7897876e88dc5ff",
};
const args = process.argv.slice(2);
const target = args.includes("--target")
  ? args[args.indexOf("--target") + 1]
  : `linux-${process.arch}`;
const targets = args.includes("--all") ? Object.keys(runtimes) : [target];
const output = resolve("build/host-packages");
const cache = resolve("build/host-runtime-cache");
await mkdir(output, { recursive: true });
await mkdir(cache, { recursive: true });
const { version } = JSON.parse(await readFile("package.json", "utf8"));
for (const target of targets) {
  if (!(target in runtimes))
    throw new Error(`Unsupported host target: ${target}`);
  const stem = `node-v${nodeVersion}-${target}`;
  const archive = join(cache, `${stem}.tar.gz`);
  await ensureRuntimeArchive({
    archive,
    url: `https://nodejs.org/dist/v${nodeVersion}/${stem}.tar.gz`,
    checksum: runtimes[target],
  });
  const folder = join(output, target);
  await rm(folder, { recursive: true, force: true });
  await mkdir(folder, { recursive: true });
  execFileSync("tar", [
    "-xzf",
    archive,
    "-C",
    folder,
    "--strip-components=1",
    `${stem}/bin/node`,
    `${stem}/LICENSE`,
  ]);
  await copyFile("build/host/monocode-host.mjs", join(folder, "host.mjs"));
  await copyFile("host/provider-guard.mjs", join(folder, "provider-guard.mjs"));
  await copyFile("LICENSE", join(folder, "MONOCODE-LICENSE"));
  await writeFile(
    join(folder, "version.json"),
    JSON.stringify({ version, nodeVersion, target }),
  );
  await writeFile(
    join(folder, "monocode-host"),
    '#!/bin/sh\nset -eu\nDIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)\nexec "$DIR/bin/node" "$DIR/host.mjs" "$@"\n',
  );
  await chmod(join(folder, "monocode-host"), 0o755);
  if (target === `linux-${process.arch}`) {
    const actual = execFileSync(join(folder, "bin/node"), [join(folder, "host.mjs"), "--version"], {
      encoding: "utf8",
    }).trim();
    if (actual !== version)
      throw new Error("Packaged host failed its executable smoke test");
  }
  const filename = `monocode-host-${target}.tar.gz`;
  await rm(join(output, filename), { force: true });
  execFileSync("tar", ["-czf", join(output, filename), "-C", folder, "."]);
  const hash = createHash("sha256")
    .update(await readFile(join(output, filename)))
    .digest("hex");
  await writeFile(join(output, `${filename}.sha256`), `${hash}  ${filename}\n`);
  console.log(`${filename} (${version}, Node ${nodeVersion})`);
}
