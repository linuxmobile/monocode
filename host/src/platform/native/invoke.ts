/**
 * The headless host has no desktop command bridge. These commands used to run
 * in the desktop webview; the GPUI app talks to `monocode-host` directly
 * instead. Keeping one rejecting implementation makes accidental use explicit
 * without pulling a webview bridge into the host build.
 */
export type UnlistenFn = () => void;

/** Install a command handler for tests or embedders. */
let handler:
  | (<T>(command: string, args?: Record<string, unknown>) => Promise<T>)
  | undefined;

export function setInvokeHandler(
  next: typeof handler,
): void {
  handler = next;
}

export function invoke<T>(
  command: string,
  args?: Record<string, unknown>,
): Promise<T> {
  if (!handler) {
    return Promise.reject(
      new Error(`Native command "${command}" is unavailable in the headless host`),
    );
  }
  return handler<T>(command, args);
}
