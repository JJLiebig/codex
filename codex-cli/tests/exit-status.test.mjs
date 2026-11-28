// Run with CODEX_TEST_BINARY=/absolute/path/to/codex node --test codex-cli/tests/exit-status.test.mjs.
import assert from "node:assert/strict";
import { execFileSync, spawn, spawnSync } from "node:child_process";
import { once } from "node:events";
import {
  copyFile,
  mkdir,
  mkdtemp,
  rm,
  symlink,
  writeFile,
} from "node:fs/promises";
import { constants, tmpdir } from "node:os";
import path from "node:path";
import { createInterface } from "node:readline";
import test from "node:test";

async function installWrapper(directory, executable) {
  const arch = process.arch === "arm64" ? "aarch64" : "x86_64";
  const target = `${arch}-${process.platform === "darwin" ? "apple-darwin" : "unknown-linux-musl"}`;
  const bin = path.join(directory, "vendor", target, "bin");
  await mkdir(bin, { recursive: true });
  await symlink(executable, path.join(bin, "codex"));
  const scripts = path.join(directory, "bin");
  await mkdir(path.join(scripts, "codex_plus_plus"), { recursive: true });
  await writeFile(
    path.join(directory, "package.json"),
    '{"name":"@openai/codex","type":"module"}',
  );
  for (const name of [
    "codex.js",
    "launcher.js",
    "codex_plus_plus/windows_upstream_launcher.js",
  ]) {
    await copyFile(
      new URL(`../bin/${name}`, import.meta.url),
      path.join(scripts, name),
    );
  }
  return path.join(scripts, "codex.js");
}

test(
  "the token-free npm spawn fallback forwards a second SIGTERM",
  { skip: process.platform === "win32" },
  async () => {
    const directory = await mkdtemp(path.join(tmpdir(), "codex-npm-signal-"));
    try {
      const wrapper = await installWrapper(directory, process.execPath);
      const preload = path.join(directory, "fallback.cjs");
      await writeFile(preload, "process.execve = undefined;");
      const env = { ...process.env };
      delete env.OSC7577_TOKEN;
      const child = spawn(
        process.execPath,
        [
          "--require",
          preload,
          wrapper,
          "-e",
          `if (process.env.OSC7577_TOKEN !== undefined) process.exit(92);
         let signals = 0;
         process.on("SIGTERM", () => {
           if (++signals === 2) process.exit(17);
           console.log("received first SIGTERM");
         });
         setTimeout(() => process.exit(1), 5000);
         console.log("ready");`,
        ],
        { env },
      );
      const acknowledgements = createInterface({ input: child.stdout });
      acknowledgements.on("line", () => child.kill("SIGTERM"));
      assert.deepEqual(await once(child, "exit"), [17, null]);
    } finally {
      await rm(directory, { recursive: true, force: true });
    }
  },
);

test(
  "the Linux npm fallback refuses a pane credential before starting a child",
  { skip: process.platform !== "linux" },
  async () => {
    const directory = await mkdtemp(path.join(tmpdir(), "codex-npm-token-"));
    try {
      const wrapper = await installWrapper(directory, process.execPath);
      const preload = path.join(directory, "fallback.cjs");
      await writeFile(preload, "process.execve = undefined;");
      const token = "dummy-terminal-resume-token";
      const refused = spawnSync(
        process.execPath,
        ["--require", preload, wrapper, "-e", "process.exit(92);"],
        {
          env: { ...process.env, OSC7577_TOKEN: token },
          encoding: "utf8",
          timeout: 30_000,
        },
      );
      assert.equal(refused.error, undefined);
      assert.deepEqual([refused.status, refused.signal], [1, null]);
      assert.match(
        refused.stderr,
        /current Node\.js release or standalone Codex\+\+/,
      );
      assert.ok(!refused.stderr.includes(token));
    } finally {
      await rm(directory, { recursive: true, force: true });
    }
  },
);

test(
  "the npm wrapper preserves numeric exits and child signals",
  {
    skip: process.platform === "win32",
  },
  async () => {
    assert.ok(
      process.env.CODEX_TEST_BINARY,
      "Set CODEX_TEST_BINARY to a built Codex binary",
    );
    const directory = await mkdtemp(path.join(tmpdir(), "codex-npm-exit-"));
    let child;
    let killTimer;
    try {
      const wrapper = await installWrapper(
        directory,
        path.resolve(process.env.CODEX_TEST_BINARY),
      );
      const home = path.join(directory, "home");
      await mkdir(home);
      const env = { ...process.env, CODEX_HOME: home, CODEX_SQLITE_HOME: home };
      delete env.OSC7577_TOKEN;

      // A normal CLI argument error must retain its numeric exit status.
      const invalid = spawnSync(
        process.execPath,
        [wrapper, "--invalid-codex-option"],
        { env, timeout: 30_000, killSignal: "SIGKILL" },
      );
      assert.equal(invalid.error, undefined);
      assert.deepEqual([invalid.status, invalid.signal], [2, null]);

      for (const signal of ["SIGHUP", "SIGXFSZ"]) {
        // Wait for the real migration prompt before terminating a process.
        child = spawn(process.execPath, [wrapper, "migrate-nfs-db"], {
          env,
          detached: true,
        });
        const processGroup = child.pid;
        killTimer = setTimeout(() => {
          try {
            process.kill(-processGroup, "SIGKILL");
          } catch (error) {
            if (error.code !== "ESRCH") throw error;
          }
        }, 30_000);
        const exited = once(child, "exit");
        const lines = createInterface({ input: child.stderr });
        let prompted = false;
        for await (const line of lines) {
          if (line.endsWith("[y/N]:")) {
            prompted = true;
            break;
          }
        }
        assert.ok(prompted, "migration must reach its confirmation prompt");
        const processes = execFileSync("ps", ["-axo", "pid=,ppid="], {
          encoding: "utf8",
        });
        const children = processes
          .trim()
          .split("\n")
          .map((line) => line.trim().split(/\s+/).map(Number))
          .filter(([, parent]) => parent === child.pid);
        const usesExecve = typeof process.execve === "function";
        assert.equal(
          children.length,
          usesExecve ? 0 : 1,
          "execve must replace the wrapper; spawn must create one native child",
        );
        if (usesExecve || signal === "SIGHUP") {
          // Signal the native process directly, or let the wrapper forward it.
          child.kill(signal);
          assert.deepEqual(await exited, [null, signal]);
        } else {
          // A native child can die from a signal that Node ignores by default.
          process.kill(children[0][0], signal);
          assert.deepEqual(await exited, [
            128 + constants.signals[signal],
            null,
          ]);
        }
        clearTimeout(killTimer);
      }
    } finally {
      clearTimeout(killTimer);
      if (child && child.exitCode === null && child.signalCode === null) {
        process.kill(-child.pid, "SIGKILL");
        await once(child, "exit");
      }
      await rm(directory, { recursive: true, force: true });
    }
  },
);
