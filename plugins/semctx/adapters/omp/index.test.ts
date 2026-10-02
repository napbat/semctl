import { describe, expect, test } from "bun:test";
import * as path from "node:path";
import type { ExtensionAPI } from "@oh-my-pi/pi-coding-agent";
import {
	createSemctxExtension,
	extractAdditionalContext,
	type HookInvoker,
	type SemctlHookInput,
} from "./index";

type Handler = (event: Record<string, unknown>, ctx: TestContext) => unknown | Promise<unknown>;

interface TestContext {
	cwd: string;
	sessionManager: { getSessionId(): string };
	setTimeout(callback: () => void, ms?: number): number;
	clearTimer(timer: number): void;
}

class FakeInvoker implements HookInvoker {
	readonly calls: Array<{ input: SemctlHookInput; timeoutMs: number }> = [];
	shutdownCalled = false;

	constructor(
		private readonly respond: (input: SemctlHookInput) => string | undefined | Promise<string | undefined>,
	) {}

	async invoke(input: SemctlHookInput, _ctx: TestContext, timeoutMs: number) {
		this.calls.push({ input, timeoutMs });
		return await this.respond(input);
	}

	shutdown() {
		this.shutdownCalled = true;
	}
}

function makeHarness(invoker: HookInvoker) {
	const handlers = new Map<string, Handler[]>();
	const sent: Array<{
		message: Record<string, unknown>;
		options: Record<string, unknown> | undefined;
	}> = [];
	let label: string | undefined;
	const pi = {
		setLabel(value: string) {
			label = value;
		},
		on(event: string, handler: Handler) {
			const registered = handlers.get(event) ?? [];
			registered.push(handler);
			handlers.set(event, registered);
		},
		sendMessage(message: Record<string, unknown>, options?: Record<string, unknown>) {
			sent.push({ message, options });
		},
	} as unknown as ExtensionAPI;
	const ctx: TestContext = {
		cwd: "/repo",
		sessionManager: { getSessionId: () => "session-1" },
		setTimeout: () => 1,
		clearTimer: () => {},
	};

	createSemctxExtension(invoker)(pi);

	const emit = async (event: string, payload: Record<string, unknown> = {}) => {
		let result: unknown;
		for (const handler of handlers.get(event) ?? []) {
			result = await handler({ type: event, ...payload }, ctx);
		}
		return result;
	};

	return { emit, handlers, sent, label };
}

describe("semctx OMP extension", () => {
	test("maps session boundaries and queues hidden orientation", async () => {
		const invoker = new FakeInvoker(() => "repository orientation");
		const harness = makeHarness(invoker);

		await harness.emit("session_start");
		await harness.emit("session_switch", { reason: "resume" });
		await harness.emit("session_branch");
		await harness.emit("session_tree");
		await harness.emit("session_compact");

		expect(harness.label).toBe("semctx");
		expect(invoker.calls.map(call => call.input.source)).toEqual([
			"startup",
			"resume",
			"clear",
			"clear",
			"compact",
		]);
		for (const call of invoker.calls) {
			expect(call.input).toMatchObject({
				host: "omp",
				hook_event_name: "SessionStart",
				cwd: "/repo",
				session_id: "session-1",
			});
		}
		expect(harness.sent).toHaveLength(5);
		expect(harness.sent[0]).toEqual({
			message: {
				customType: "ca.napbat.semctx.orientation",
				content: "repository orientation",
				display: false,
				attribution: "agent",
			},
			options: { deliverAs: "nextTurn" },
		});
	});

	test("injects prompt candidates and returns passive tool context", async () => {
		const responses: Record<string, string> = {
			UserPromptSubmit: "candidate hits",
			PreToolUse: "prefer `mcp__semctx_semctx_search_codebase`",
		};
		const invoker = new FakeInvoker(input => responses[input.hook_event_name]);
		const harness = makeHarness(invoker);

		const promptResult = (await harness.emit("before_agent_start", {
			prompt: "where is authentication handled?",
			systemPrompt: [],
		})) as { message: Record<string, unknown> };
		expect(promptResult.message).toMatchObject({
			customType: "ca.napbat.semctx.prompt-context",
			content: "candidate hits",
			display: false,
		});
		const promptCall = invoker.calls[0].input;
		expect(promptCall.prompt).toBe("where is authentication handled?");
		expect(promptCall.prompt_id).toBeString();

		const toolResult = await harness.emit("tool_call", {
			toolCallId: "tool-1",
			toolName: "grep",
			input: { pattern: "authenticate" },
		});
		expect(toolResult).toEqual({
			additionalContext: "prefer `mcp__semctx_semctx_search_codebase`",
		});
		const toolCall = invoker.calls[1].input;
		expect(toolCall).toMatchObject({
			host: "omp",
			hook_event_name: "PreToolUse",
			prompt_id: promptCall.prompt_id,
			tool_name: "Grep",
			tool_input: { pattern: "authenticate" },
		});

		expect(harness.handlers.has("context")).toBeFalse();
	});

	test("returns each tool call's passive context independently", async () => {
		const invoker = new FakeInvoker(input => input.tool_name);
		const harness = makeHarness(invoker);
		const [grepResult, globResult] = await Promise.all([
			harness.emit("tool_call", { toolName: "grep", input: { pattern: "needle" } }),
			harness.emit("tool_call", { toolName: "glob", input: { path: "*.rs" } }),
		]);
		expect(grepResult).toEqual({ additionalContext: "Grep" });
		expect(globResult).toEqual({ additionalContext: "Glob" });
		expect(invoker.calls[0].input.prompt_id).toBe(invoker.calls[1].input.prompt_id);
	});

	test("maps OMP search and semctx tools without propagating hook failures", async () => {
		const invoker = new FakeInvoker(() => {
			throw new Error("hook unavailable");
		});
		const harness = makeHarness(invoker);

		for (const [toolName, expected] of [
			["glob", "Glob"],
			["bash", "Bash"],
		] as const) {
			expect(
				await harness.emit("tool_call", {
					toolCallId: toolName,
					toolName,
					input: toolName === "bash" ? { command: "rg needle" } : { path: "**/*.rs" },
				}),
			).toBeUndefined();
			expect(invoker.calls.at(-1)?.input.tool_name).toBe(expected);
		}
		const semctxTool = "mcp__semctx_semctx_search_codebase";
		await harness.emit("tool_call", {
			toolCallId: "semctx",
			toolName: semctxTool,
			input: { query: "authentication" },
		});
		expect(invoker.calls.at(-1)?.input).toMatchObject({
			hook_event_name: "PreToolUse",
			tool_name: semctxTool,
			tool_input: { query: "authentication" },
		});

		const callsBeforeRead = invoker.calls.length;
		await harness.emit("tool_call", {
			toolCallId: "read",
			toolName: "read",
			input: { path: "src/main.rs" },
		});
		expect(invoker.calls).toHaveLength(callsBeforeRead);
	});

	test("translates OMP tool inputs to the hook wire shape", async () => {
		const invoker = new FakeInvoker(() => undefined);
		const harness = makeHarness(invoker);
		const sent = async (toolName: string, input: Record<string, unknown>) => {
			await harness.emit("tool_call", { toolName, input });
			const { tool_name, tool_input, cwd } = invoker.calls.at(-1)?.input ?? {};
			return { tool_name, tool_input, cwd };
		};

		expect(await sent("glob", { path: "src/**/*.rs", limit: 5 })).toEqual({
			tool_name: "Glob",
			tool_input: { pattern: "src/**/*.rs" },
			cwd: "/repo",
		});
		expect((await sent("glob", {})).tool_input).toEqual({ pattern: "**/*" });
		expect((await sent("glob", { path: "/" })).tool_input).toEqual({ pattern: "**/*" });
		expect((await sent("glob", { path: "src/**/*.rs; tests/*.rs" })).tool_input).toEqual({
			pattern: "src/**/*.rs;tests/*.rs",
		});

		expect((await sent("grep", { pattern: "fn main", path: "src/main.rs:10-20" })).tool_input).toEqual({
			pattern: "fn main",
			path: "src/main.rs",
		});
		expect((await sent("grep", { pattern: "x", path: "C:\\repo\\lib.rs:5-9,40+3" })).tool_input).toEqual({
			pattern: "x",
			path: "C:\\repo\\lib.rs",
		});
		expect((await sent("grep", { pattern: "x", path: "src; tests" })).tool_input).toEqual({
			pattern: "x",
			path: "src; tests",
		});

		expect(await sent("bash", { command: "rg needle", cwd: "crates/core" })).toEqual({
			tool_name: "Bash",
			tool_input: { command: "rg needle", cwd: "crates/core" },
			cwd: path.resolve("/repo", "crates/core"),
		});
		expect((await sent("bash", { command: "rg needle" })).cwd).toBe("/repo");
	});

	test("skips searches of OMP internal and web URLs", async () => {
		const invoker = new FakeInvoker(() => "nudge");
		const harness = makeHarness(invoker);
		for (const [toolName, input] of [
			["grep", { pattern: "session", path: "omp://**/*.md" }],
			["grep", { pattern: "x", path: "src; local://notes" }],
			["grep", { pattern: "x", path: "https://example.com/doc" }],
			["glob", { path: "skill://semctx/**" }],
			["glob", { path: "omp:/tools" }],
			["bash", { command: "rg needle", cwd: "local://scratch" }],
		] as const) {
			expect(await harness.emit("tool_call", { toolName, input })).toBeUndefined();
		}
		expect(invoker.calls).toHaveLength(0);

		await harness.emit("tool_call", { toolName: "glob", input: { path: "C:/repo/src/*.rs" } });
		expect(invoker.calls).toHaveLength(1);
	});

	test("shuts down child work with the session", async () => {
		const invoker = new FakeInvoker(() => undefined);
		const harness = makeHarness(invoker);
		await harness.emit("session_shutdown");
		expect(invoker.shutdownCalled).toBeTrue();
	});
});

describe("hook output parsing", () => {
	test("accepts only non-empty additional context", () => {
		expect(
			extractAdditionalContext(
				JSON.stringify({ hookSpecificOutput: { additionalContext: "candidate context" } }),
			),
		).toBe("candidate context");
		expect(extractAdditionalContext("{}")).toBeUndefined();
		expect(extractAdditionalContext("not-json")).toBeUndefined();
	});
});
