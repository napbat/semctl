// The shared engine for host integration tests. It builds the real `semctl`
// from this checkout, isolates it (logged out, offline, private state), starts
// the scripted model, and runs one host session per call.

import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { type HostAdapter, type HostContext, REPO_ROOT } from "./hosts";
import { type ScriptedTurn, startScriptedModel } from "./model_server";

const SESSION_TIMEOUT_MS = 90_000;

/** One `SEMCTX_HOOK_TRACE` line, as written by `src/commands/hook/trace.rs`. */
export interface TraceRecord {
	event: string;
	host: string;
	tool: string;
	outcome?: string;
	context: boolean;
}

export interface HostSession {
	exitCode: number;
	/** Host stdout and stderr, for failure messages. */
	output: string;
	trace: TraceRecord[];
	/** Raw agent-loop model requests, in order. */
	requests: string[];
}

/**
 * The host launch command. `SEMCTX_HOST_COMMAND_<ID>` overrides the default,
 * for example `SEMCTX_HOST_COMMAND_CODEX="bun x @openai/codex"`.
 */
export function hostCommand(adapter: HostAdapter): string[] {
	const override = process.env[`SEMCTX_HOST_COMMAND_${adapter.id.toUpperCase()}`]?.trim();
	return override ? override.split(/\s+/) : adapter.defaultCommand;
}

let semctlDir: string | undefined;

/** Build `semctl` once per test process and return the directory that holds it. */
function buildSemctl(): string {
	if (semctlDir !== undefined) return semctlDir;
	const build = Bun.spawnSync(["cargo", "build", "--locked", "--quiet"], {
		cwd: REPO_ROOT,
		stdout: "pipe",
		stderr: "pipe",
	});
	if (build.exitCode !== 0) throw new Error(`cargo build failed: ${build.stderr.toString()}`);
	semctlDir = path.join(process.env.CARGO_TARGET_DIR ?? path.join(REPO_ROOT, "target"), "debug");
	return semctlDir;
}

function makeWorkspace(root: string): string {
	const workspace = path.join(root, "workspace");
	fs.mkdirSync(path.join(workspace, "src"), { recursive: true });
	fs.writeFileSync(path.join(workspace, "src", "main.rs"), "fn main() {}\n");
	// semctl scopes searches to the enclosing Git repository.
	const init = Bun.spawnSync(["git", "init", "--quiet"], { cwd: workspace, stderr: "pipe" });
	if (init.exitCode !== 0) throw new Error(`git init failed: ${init.stderr.toString()}`);
	return workspace;
}

function readTrace(file: string): TraceRecord[] {
	if (!fs.existsSync(file)) return [];
	return fs
		.readFileSync(file, "utf8")
		.split("\n")
		.filter(line => line.length > 0)
		.map(line => JSON.parse(line));
}

/** Remove unset keys, so an `undefined` value unsets an inherited variable. */
function spawnEnv(env: Record<string, string | undefined>): Record<string, string> {
	return Object.fromEntries(
		Object.entries(env).filter((entry): entry is [string, string] => entry[1] !== undefined),
	);
}

export class HostSuite {
	private runs = 0;

	private constructor(
		private readonly adapter: HostAdapter,
		private readonly root: string,
		private readonly context: HostContext,
	) {}

	/** Prepare a workspace, the scripted model, and the host configuration. */
	static start(adapter: HostAdapter): HostSuite {
		const command = hostCommand(adapter);
		if (Bun.which(command[0]) === null) throw new Error(`${adapter.id}: \`${command[0]}\` is not on PATH`);

		const bin = buildSemctl();
		const root = fs.mkdtempSync(path.join(os.tmpdir(), `semctx-${adapter.id}-`));
		const home = path.join(root, "host-home");
		fs.mkdirSync(home);
		const env = spawnEnv({
			...process.env,
			// The checkout's semctl must shadow any installed copy.
			PATH: `${bin}${path.delimiter}${process.env.PATH ?? ""}`,
			// Logged out against an unreachable server: hooks stay offline.
			XDG_CONFIG_HOME: path.join(root, "semctl-config"),
			SEMCTX_SERVER: "http://127.0.0.1:9",
			SEMCTX_TOKEN: undefined,
			SEMCTX_HOOK_DISABLE: undefined,
			SEMCTX_NUDGE_DISABLE: undefined,
			SEMCTX_HOOK_UPDATE_CHECK: "0",
			SEMCTX_MCP_UPDATE_CHECK: "0",
			// A shared MCP daemon would outlive the session and hold its pipes open.
			SEMCTX_MCP_DAEMON: "off",
			// A due reminder is observable on the first broad search.
			SEMCTX_NUDGE_GRACE: "0",
		});
		const context: HostContext = {
			command,
			home,
			workspace: makeWorkspace(root),
			model: startScriptedModel(adapter.wire),
			env,
		};
		adapter.setup(context);
		return new HostSuite(adapter, root, context);
	}

	/** Run one print-mode session whose model replies with `turns`. */
	async run(prompt: string, turns: ScriptedTurn[]): Promise<HostSession> {
		this.runs += 1;
		const runDir = path.join(this.root, `run-${this.runs}`);
		const tempDir = path.join(runDir, "tmp");
		fs.mkdirSync(tempDir, { recursive: true });
		const tracePath = path.join(runDir, "trace.jsonl");
		this.context.model.script(turns);

		const child = Bun.spawn([...this.context.command, ...this.adapter.args(this.context, prompt)], {
			cwd: this.context.workspace,
			env: spawnEnv({
				...this.context.env,
				...this.adapter.env(this.context),
				// semctl keeps hook state under the temporary directory.
				TMP: tempDir,
				TEMP: tempDir,
				TMPDIR: tempDir,
				SEMCTX_HOOK_TRACE: tracePath,
			}),
			stdin: "ignore",
			stdout: "pipe",
			stderr: "pipe",
		});
		const timer = setTimeout(() => child.kill(), SESSION_TIMEOUT_MS);
		try {
			const [stdout, stderr, exitCode] = await Promise.all([
				new Response(child.stdout).text(),
				new Response(child.stderr).text(),
				child.exited,
			]);
			return {
				exitCode,
				output: `${stdout}\n${stderr}`,
				trace: readTrace(tracePath),
				requests: [...this.context.model.requests],
			};
		} finally {
			clearTimeout(timer);
		}
	}

	stop(): void {
		this.context.model.stop();
		// Windows can hold a handle for a moment after a host's child exits.
		fs.rmSync(this.root, { recursive: true, force: true, maxRetries: 10, retryDelay: 200 });
	}
}
