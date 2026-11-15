import { execFile, spawn } from "node:child_process";
import { promisify } from "node:util";
import { appendFileSync, closeSync, existsSync, openSync } from "node:fs";
import { mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { homedir, userInfo } from "node:os";
import { join } from "node:path";
const exec = promisify(execFile);
type ServiceOptions = {
  directory: string;
  port: number;
  executable: string;
  entry: string;
};

export async function connectionInfo(
  directory: string,
): Promise<{ port: number; pid: number }> {
  const state = JSON.parse(
    await readFile(join(directory, "running.json"), "utf8"),
  );
  if (
    !Number.isInteger(state.port) ||
    state.port < 1 ||
    state.port > 65535 ||
    typeof state.secret !== "string" ||
    !/^[\w-]{43}$/.test(state.secret)
  ) {
    throw new Error("Invalid host state");
  }
  const response = await fetch(`http://127.0.0.1:${state.port}/lifecycle`, {
    method: "POST",
    headers: { Authorization: `Bearer ${state.secret}` },
    body: JSON.stringify({ action: "status" }),
    signal: AbortSignal.timeout(2000),
  });
  if (!response.ok) throw new Error("Host is not ready");
  return { port: state.port, pid: state.pid };
}

// systemd expands % specifiers in both settings; $ variables only in ExecStart.
const unitQuote = (value: string) =>
  `"${value.replaceAll("\\", "\\\\").replaceAll('"', '\\"').replaceAll("%", "%%").replaceAll("\n", "\\n")}"`;
export function systemdUnit(options: ServiceOptions, path: string): string {
  return `[Unit]
Description=MonoCode Host
After=network.target

[Service]
ExecStart=${[options.executable, options.entry, "serve", "--data-dir", options.directory, "--port", String(options.port)].map((value) => unitQuote(value.replaceAll("$", () => "$$"))).join(" ")}
Environment=${unitQuote(`PATH=${path}`)}
Restart=on-failure
RestartSec=5
UMask=0077
KillMode=control-group
TimeoutStopSec=20

[Install]
WantedBy=default.target
`;
}

// SSH sessions may lack the user bus variables systemctl --user needs.
const systemdEnvironment = (uid: number): NodeJS.ProcessEnv => {
  const runtime = process.env.XDG_RUNTIME_DIR ?? `/run/user/${uid}`;
  return {
    ...process.env,
    XDG_RUNTIME_DIR: runtime,
    DBUS_SESSION_BUS_ADDRESS:
      process.env.DBUS_SESSION_BUS_ADDRESS ?? `unix:path=${runtime}/bus`,
  };
};

// sd_booted(3): this directory exists only when systemd is the init system.
// Containers and microVMs such as Firecracker often boot without it.
export const systemdBooted = (root = "/"): boolean =>
  existsSync(join(root, "run/systemd/system"));

/**
 * Starts the host in its own session (setsid), so it outlives the SSH
 * connection that started it. Nothing restarts it if it exits; every connect
 * runs `service install` again, which starts it when it is not answering.
 */
export function startDetached(options: ServiceOptions, path: string): void {
  const log = openSync(join(options.directory, "host.log"), "a", 0o600);
  try {
    const child = spawn(
      options.executable,
      [
        options.entry,
        "serve",
        "--data-dir",
        options.directory,
        "--port",
        String(options.port),
      ],
      {
        detached: true,
        stdio: ["ignore", log, log],
        env: { ...process.env, PATH: path },
      },
    );
    // A spawn failure surfaces below as a host that never answers; record why
    // in host.log, which the startup error points to.
    child.on("error", (error) => {
      try {
        appendFileSync(
          join(options.directory, "host.log"),
          `Failed to start host: ${error.message}\n`,
          { mode: 0o600 },
        );
      } catch {
        /* the startup error still names host.log */
      }
    });
    child.unref();
  } finally {
    closeSync(log);
  }
}

type Run = (
  command: string,
  args: string[],
  env?: NodeJS.ProcessEnv,
) => Promise<unknown>;

/**
 * Removes the login service or scheduled task so the host no longer starts
 * automatically, stopping it where the service manager owns the process.
 * Never deletes the data directory: sessions, logs and device credentials stay
 * until the user removes them explicitly. Returns follow-up notes.
 */
export async function uninstallService(
  system: {
    home?: string;
    systemd?: boolean;
    run?: Run;
  } = {},
): Promise<string[]> {
  const home = system.home ?? homedir();
  const run: Run =
    system.run ??
    ((command, args, env = process.env) =>
      exec(command, args, { env, timeout: 30_000, maxBuffer: 128 * 1024 }));
  const ignore = () => undefined;
  // Without systemd nothing was registered; the caller stops the running host.
  if (!(system.systemd ?? systemdBooted())) return [];
  const user = userInfo();
  const env = systemdEnvironment(user.uid);
  await run(
    "systemctl",
    ["--user", "disable", "--now", "monocode-host.service"],
    env,
  ).catch(ignore);
  await rm(join(home, ".config/systemd/user/monocode-host.service"), {
    force: true,
  });
  await run("systemctl", ["--user", "daemon-reload"], env).catch(ignore);
  return [
    `Lingering is still enabled for ${user.username}; other user services may rely on it. To turn it off: loginctl disable-linger ${user.username}`,
  ];
}

export async function installService(
  options: ServiceOptions,
): Promise<{ port: number; pid: number }> {
  // Never replace a running host: connecting must not interrupt agent turns.
  try {
    return await connectionInfo(options.directory);
  } catch {
    /* install/start */
  }
  const path = [
    ...new Set([
      process.env.PATH ?? "",
      join(homedir(), ".local/bin"),
      "/usr/local/bin",
      "/usr/bin",
      "/bin",
    ]),
  ].join(":");
  const run = async (command: string, args: string[], env = process.env) =>
    exec(command, args, { env, timeout: 15_000, maxBuffer: 128 * 1024 });
  if (!systemdBooted()) {
    startDetached(options, path);
  } else {
    const user = userInfo();
    const env = systemdEnvironment(user.uid);
    try {
      await run(
        "loginctl",
        ["enable-linger", user.username, "--no-ask-password"],
        env,
      );
      const linger = await run(
        "loginctl",
        ["show-user", user.username, "--property=Linger", "--value"],
        env,
      );
      if (linger.stdout.trim() !== "yes") throw new Error("linger disabled");
    } catch {
      throw new Error(
        `This host needs systemd user services and lingering to keep sessions running after SSH disconnects. An administrator can enable it with: sudo loginctl enable-linger ${user.username}`,
      );
    }
    const folder = join(homedir(), ".config/systemd/user");
    await mkdir(folder, { recursive: true });
    const file = join(folder, "monocode-host.service");
    try {
      await readFile(file);
    } catch {
      await writeFile(file, systemdUnit(options, path), { mode: 0o600 });
    }
    await run("systemctl", ["--user", "daemon-reload"], env);
    await run(
      "systemctl",
      ["--user", "enable", "--now", "monocode-host.service"],
      env,
    );
  }
  for (let attempt = 0; attempt < 50; attempt++) {
    try {
      return await connectionInfo(options.directory);
    } catch {
      /* starting */
    }
    await new Promise((resolve) => setTimeout(resolve, 200));
  }
  throw new Error(
    !systemdBooted()
      ? `The host did not start. Check ${join(options.directory, "host.log")}.`
      : `The host service was installed but did not start. Check ${join(options.directory, "host.log")}.`,
  );
}
