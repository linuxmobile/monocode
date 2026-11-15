import { expect, it } from "vitest";
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import {
  startDetached,
  systemdBooted,
  systemdUnit,
  uninstallService,
} from "./service";

it("keeps paths and environment content from injecting service configuration", () => {
  const options = {
    directory: "/Users/a & b/%folder",
    port: 3774,
    executable: '/runtime/a"b/node',
    entry: "/runtime/$name/host.mjs",
  };
  const unit = systemdUnit(options, "/bin:/a\n[Service]\nExecStart=/bad");
  expect(unit.match(/^ExecStart=/gm)).toHaveLength(1);
  expect(unit).toContain("%%folder");
  expect(unit).toContain("$$name");
  expect(unit).toContain("KillMode=control-group");
});

it("removes the service registration but keeps host data", async () => {
  const home = mkdtempSync(join(tmpdir(), "monocode-service-test-"));
  try {
    const service = join(home, ".config/systemd/user/monocode-host.service");
    const data = join(home, ".monocode-host/host.db");
    for (const file of [service, data]) {
      mkdirSync(dirname(file), { recursive: true });
      writeFileSync(file, "existing");
    }
    const calls: string[][] = [];
    const notes = await uninstallService({
      home,
      systemd: true,
      run: async (command, args) => {
        calls.push([command, ...args]);
        // A service that is not loaded must not block cleanup.
        if (args.includes("disable")) throw new Error("not loaded");
      },
    });
    expect(existsSync(service)).toBe(false);
    expect(readFileSync(data, "utf8")).toBe("existing");
    expect(calls).toContainEqual([
      "systemctl",
      "--user",
      "disable",
      "--now",
      "monocode-host.service",
    ]);
    expect(notes.join("\n")).toContain("loginctl disable-linger");
  } finally {
    rmSync(home, { recursive: true, force: true });
  }
});

it("detects whether systemd is the init system", () => {
  const root = mkdtempSync(join(tmpdir(), "monocode-systemd-test-"));
  try {
    expect(systemdBooted(root)).toBe(false);
    mkdirSync(join(root, "run/systemd/system"), { recursive: true });
    expect(systemdBooted(root)).toBe(true);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

it("leaves systemd alone when uninstalling from a host without it", async () => {
  const calls: string[][] = [];
  const notes = await uninstallService({
    systemd: false,
    run: async (command, args) => {
      calls.push([command, ...args]);
    },
  });
  expect(calls).toEqual([]);
  expect(notes).toEqual([]);
});

it.runIf(process.platform === "linux")(
  "starts the host in its own session so it outlives the SSH connection",
  async () => {
    const directory = mkdtempSync(join(tmpdir(), "monocode-detached-test-"));
    try {
      const entry = join(directory, "entry.mjs");
      const report = join(directory, "report.json");
      writeFileSync(
        entry,
        `import { readFileSync, writeFileSync } from "node:fs";
const session = readFileSync("/proc/self/stat", "utf8").split(") ")[1].split(" ")[3];
writeFileSync(${JSON.stringify(report)}, JSON.stringify({
  args: process.argv.slice(2), pid: process.pid, session: Number(session), path: process.env.PATH,
}));
console.log("started");
`,
      );
      startDetached(
        { directory, port: 4321, executable: process.execPath, entry },
        "/test/bin:/usr/bin",
      );
      for (let i = 0; i < 100 && !existsSync(report); i++)
        await new Promise((resolve) => setTimeout(resolve, 50));
      const child = JSON.parse(readFileSync(report, "utf8"));
      expect(child.args).toEqual([
        "serve",
        "--data-dir",
        directory,
        "--port",
        "4321",
      ]);
      // A session leader: hanging up the SSH session does not reach it.
      expect(child.session).toBe(child.pid);
      expect(child.path).toBe("/test/bin:/usr/bin");
      for (let i = 0; i < 100; i++) {
        if (
          readFileSync(join(directory, "host.log"), "utf8").includes("started")
        )
          break;
        await new Promise((resolve) => setTimeout(resolve, 50));
      }
      expect(readFileSync(join(directory, "host.log"), "utf8")).toContain(
        "started",
      );
    } finally {
      rmSync(directory, { recursive: true, force: true });
    }
  },
);

it("records why the host could not be started", async () => {
  const directory = mkdtempSync(join(tmpdir(), "monocode-detached-test-"));
  try {
    startDetached(
      {
        directory,
        port: 4321,
        executable: join(directory, "missing-node"),
        entry: join(directory, "entry.mjs"),
      },
      "/usr/bin",
    );
    const log = join(directory, "host.log");
    for (let i = 0; i < 100; i++) {
      if (readFileSync(log, "utf8").includes("Failed to start host")) break;
      await new Promise((resolve) => setTimeout(resolve, 50));
    }
    expect(readFileSync(log, "utf8")).toMatch(/Failed to start host: .*ENOENT/);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});
