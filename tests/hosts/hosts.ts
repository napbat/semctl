// One adapter per supported coding-agent host. An adapter launches the host
// with the semctx plugin from this checkout, isolates the host's own
// configuration, points the host at the scripted model, and maps each
// host-neutral search intent to the host's native tool call.

import * as fs from "node:fs";
import * as path from "node:path";
import type { ScriptedModel, ScriptedToolCall, WireFormat } from "./model_server";

export const REPO_ROOT = path.resolve(import.meta.dir, "../..");
export const PLUGIN_ROOT = path.join(REPO_ROOT, "plugins", "semctx");

/**
 * A search a model can make, independent of host tool names. The expected
 * `semctl hook` outcome for each intent is the same on every host.
 */
export type SearchIntent =
	/** Search the whole repository. */
	| "broad-search"
	/** Search one known file. */
	| "single-file-search"
	/** Run a shell search in a subdirectory where the target is one file. */
	| "subdirectory-shell-search"
	/** Search host-internal resources instead of repository files. */
	| "internal-url-search"
	/** Use a tool that is not a search. */
	| "non-search";

/** Paths and settings that one host suite owns. */
export interface HostContext {
	/** The host launch command, for example `["omp"]`. */
	command: string[];
	/** A private directory for the host's own configuration. */
	home: string;
	/** The repository the host session works in. */
	workspace: string;
	model: ScriptedModel;
	/** The base environment, with semctl isolation already applied. */
	env: Record<string, string>;
}

export interface HostAdapter {
	id: "omp" | "claude" | "codex";
	/** The `host` value that this host's hook payload carries. */
	hookHost: string;
	wire: WireFormat;
	defaultCommand: string[];
	/** Why this platform cannot run the host's semctx integration, if it cannot. */
	unsupported?: string;
	/** One-time host configuration for a suite. */
	setup(context: HostContext): void;
	/** Arguments after the launch command for one print-mode session. */
	args(context: HostContext, prompt: string): string[];
	/** Host-specific environment for one session. */
	env(context: HostContext): Record<string, string | undefined>;
	/** The native tool call for an intent, or `undefined` when the host has no such call. */
	toolCall(intent: SearchIntent): ScriptedToolCall | undefined;
}

function checked(argv: string[], options: { env: Record<string, string> }): void {
	const result = Bun.spawnSync(argv, { ...options, stdout: "pipe", stderr: "pipe" });
	if (result.exitCode !== 0) {
		throw new Error(`\`${argv.join(" ")}\` failed: ${result.stderr.toString()}${result.stdout.toString()}`);
	}
}

const omp: HostAdapter = {
	id: "omp",
	hookHost: "omp",
	wire: "openai-chat",
	defaultCommand: ["omp"],
	setup({ home, model }) {
		fs.writeFileSync(
			path.join(home, "models.yml"),
			[
				"providers:",
				"  scripted:",
				`    baseUrl: ${model.baseUrl}`,
				"    api: openai-completions",
				"    auth: none",
				"    models:",
				"      - id: scripted",
				"        name: Scripted",
				"        reasoning: false",
				"        input: [text]",
				"",
			].join("\n"),
		);
	},
	args(_context, prompt) {
		return [
			"--print",
			"--mode",
			"json",
			"--no-session",
			// A package directory resolves through its `package.json`
			// `omp.extensions` manifest, as a marketplace install does, but it
			// writes no plugin state.
			"--no-extensions",
			"--extension",
			PLUGIN_ROOT,
			"--no-skills",
			"--no-rules",
			"--no-lsp",
			"--no-title",
			"--yolo",
			"--model",
			"scripted/scripted",
			prompt,
		];
	},
	env({ home }) {
		return { PI_CODING_AGENT_DIR: home };
	},
	toolCall(intent) {
		switch (intent) {
			case "broad-search":
				return { name: "glob", arguments: { path: "src/**/*.rs" } };
			case "single-file-search":
				// The adapter must remove OMP's line selector for semctl to see one file.
				return { name: "grep", arguments: { pattern: "fn main", path: "src/main.rs:1-1" } };
			case "subdirectory-shell-search":
				return { name: "bash", arguments: { command: "rg needle main.rs", cwd: "src" } };
			case "internal-url-search":
				return { name: "grep", arguments: { pattern: "session", path: "omp://**/*.md" } };
			case "non-search":
				return { name: "read", arguments: { path: "src/main.rs" } };
		}
	},
};

const claude: HostAdapter = {
	id: "claude",
	hookHost: "",
	wire: "anthropic-messages",
	defaultCommand: ["claude"],
	setup() {},
	args(_context, prompt) {
		return [
			"--print",
			"--verbose",
			"--output-format",
			"stream-json",
			"--no-session-persistence",
			"--permission-mode",
			"bypassPermissions",
			"--plugin-dir",
			PLUGIN_ROOT,
			"--model",
			"claude-sonnet-4-5",
			prompt,
		];
	},
	env({ home, model }) {
		return {
			CLAUDE_CONFIG_DIR: home,
			ANTHROPIC_BASE_URL: model.baseUrl.replace(/\/v1$/, ""),
			ANTHROPIC_API_KEY: "scripted",
			ANTHROPIC_AUTH_TOKEN: undefined,
			CLAUDE_CODE_OAUTH_TOKEN: undefined,
			CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC: "1",
		};
	},
	toolCall(intent) {
		switch (intent) {
			case "broad-search":
				return { name: "Glob", arguments: { pattern: "src/**/*.rs" } };
			case "single-file-search":
				return { name: "Grep", arguments: { pattern: "fn main", path: "src/main.rs" } };
			case "non-search":
				return { name: "Read", arguments: { file_path: "src/main.rs" } };
			// Claude's Bash tool has no working-directory argument, and Claude has
			// no internal URL scheme.
			case "subdirectory-shell-search":
			case "internal-url-search":
				return undefined;
		}
	},
};

const codex: HostAdapter = {
	id: "codex",
	hookHost: "",
	wire: "openai-responses",
	defaultCommand: ["codex"],
	unsupported:
		process.platform === "win32"
			? "Codex does not load plugin hooks on Windows (openai/codex#24453)"
			: undefined,
	setup({ command, env, home }) {
		const codexEnv = { ...env, CODEX_HOME: home };
		// A local marketplace installs a copy of this checkout's plugin.
		checked([...command, "plugin", "marketplace", "add", REPO_ROOT], { env: codexEnv });
		checked([...command, "plugin", "add", "semctx@semctx"], { env: codexEnv });
	},
	args({ model, workspace }, prompt) {
		return [
			"exec",
			"-c",
			'model="scripted"',
			"-c",
			'model_provider="scripted"',
			"-c",
			'model_providers.scripted.name="Scripted"',
			"-c",
			`model_providers.scripted.base_url="${model.baseUrl}"`,
			"-c",
			'model_providers.scripted.env_key="SEMCTX_SCRIPTED_API_KEY"',
			"-c",
			'model_providers.scripted.wire_api="responses"',
			"--ephemeral",
			"--json",
			// Hook trust is interactive. This suite runs only the hooks from
			// this checkout, which the test author has already reviewed.
			"--dangerously-bypass-hook-trust",
			"--sandbox",
			"read-only",
			"--cd",
			workspace,
			prompt,
		];
	},
	env({ home }) {
		return { CODEX_HOME: home, SEMCTX_SCRIPTED_API_KEY: "scripted" };
	},
	toolCall(intent) {
		switch (intent) {
			case "broad-search":
				return { name: "exec_command", arguments: { cmd: "rg --files src" } };
			case "single-file-search":
				return { name: "exec_command", arguments: { cmd: "rg needle src/main.rs" } };
			case "non-search":
				return { name: "exec_command", arguments: { cmd: "echo semctx" } };
			// Codex 0.160 drops `exec_command.workdir` from the PreToolUse payload:
			// `cwd` is the session root and `tool_input` holds only `command`. The
			// hook cannot see the subdirectory, and Codex has no internal URLs.
			case "subdirectory-shell-search":
			case "internal-url-search":
				return undefined;
		}
	},
};

export const HOST_ADAPTERS: HostAdapter[] = [omp, claude, codex];
