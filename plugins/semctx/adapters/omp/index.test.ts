import { describe, expect, test } from "bun:test";
import type { ExtensionAPI } from "@oh-my-pi/pi-coding-agent";
import {
	createSemctxExtension,
	extractAdditionalContext,
	SEMCTX_ROUTING_SYSTEM_PROMPT,
	type HookInvoker,
	type HookContext,
	type SemctlHookInput,
} from "./index";

type Handler = (event: Record<string, unknown>, ctx: TestContext) => unknown | Promise<unknown>;

type TestContext = HookContext;
type TestTimer = ReturnType<TestContext["setTimeout"]>;

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

interface HarnessOptions {
	activeTools?: string[];
	allTools?: string[];
}

function makeHarness(invoker: HookInvoker, options: HarnessOptions = {}) {
	const handlers = new Map<string, Handler[]>();
	const sent: Array<{
		message: Record<string, unknown>;
		options: Record<string, unknown> | undefined;
	}> = [];
	let label: string | undefined;
	const activeTools = options.activeTools ?? ["read", "mcp__semctx_semctx_search_codebase"];
	const allTools = options.allTools ?? activeTools;
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
		getActiveTools() {
			return [...activeTools];
		},
		getAllTools() {
			return allTools.map(name => ({
				name,
				description: name,
				parameters: {},
				sourceInfo: {
					path: `<${name}>`,
					source: name.startsWith("mcp__") ? "mcp" : "builtin",
					scope: "temporary",
					origin: "top-level",
				},
			}));
		},
	} as unknown as ExtensionAPI;
	let sessionId = "session-1";
	const ctx: TestContext = {
		cwd: "/repo",
		sessionManager: { getSessionId: () => sessionId },
		setTimeout: () => ({}) as TestTimer,
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

	return {
		emit,
		handlers,
		sent,
		get label() {
			return label;
		},
		setSessionId(value: string) {
			sessionId = value;
		},
	};
}

function deferred<T>() {
	let resolve!: (value: T | PromiseLike<T>) => void;
	const promise = new Promise<T>(done => {
		resolve = done;
	});
	return { promise, resolve };
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

	test("injects prompt candidates and a one-shot namespaced tool nudge", async () => {
		const responses: Record<string, string> = {
			UserPromptSubmit: "candidate hits",
			PreToolUse: "prefer `mcp__semctx_semctx_search_codebase`",
		};
		const invoker = new FakeInvoker(input => responses[input.hook_event_name]);
		const harness = makeHarness(invoker);

		const baseSystemPrompt = ["base OMP prompt"];
		const promptResult = (await harness.emit("before_agent_start", {
			prompt: "where is authentication handled?",
			systemPrompt: baseSystemPrompt,
		})) as { message: Record<string, unknown>; systemPrompt: string[] };
		expect(promptResult.message).toMatchObject({
			customType: "ca.napbat.semctx.prompt-context",
			content: "candidate hits",
			display: false,
		});
		expect(promptResult.systemPrompt).toEqual([...baseSystemPrompt, SEMCTX_ROUTING_SYSTEM_PROMPT]);
		const promptCall = invoker.calls[0].input;
		expect(promptCall.prompt).toBe("where is authentication handled?");
		expect(promptCall.prompt_id).toBeString();

		await harness.emit("tool_call", {
			toolCallId: "tool-1",
			toolName: "grep",
			input: { pattern: "authenticate" },
		});
		const toolCall = invoker.calls[1].input;
		expect(toolCall).toMatchObject({
			host: "omp",
			hook_event_name: "PreToolUse",
			prompt_id: promptCall.prompt_id,
			tool_name: "Grep",
			tool_input: { pattern: "authenticate" },
		});

		const contextResult = (await harness.emit("context", {
			messages: [{ role: "user", content: "question", timestamp: 1 }],
		})) as { messages: Array<Record<string, unknown>> };
		expect(contextResult.messages.at(-1)).toMatchObject({
			role: "custom",
			customType: "ca.napbat.semctx.nudge",
			content: "prefer `mcp__semctx_semctx_search_codebase`",
			display: false,
			attribution: "agent",
		});
		expect(await harness.emit("context", { messages: [] })).toBeUndefined();
	});

	test("routes when semctx is mounted under xd and stays silent when an allowlist excludes it", async () => {
		const semctxTool = "mcp__semctx_semctx_search_codebase";
		const invoker = new FakeInvoker(() => undefined);
		const mounted = makeHarness(invoker, {
			activeTools: ["read", "write", "lsp"],
			allTools: ["read", "write", "lsp", semctxTool],
		});

		const mountedResult = (await mounted.emit("before_agent_start", {
			prompt: "trace authentication",
			systemPrompt: ["base", `- xd://${semctxTool}`],
		})) as { systemPrompt: string[] };
		expect(mountedResult.systemPrompt.at(-1)).toBe(SEMCTX_ROUTING_SYSTEM_PROMPT);

		const excluded = makeHarness(invoker, {
			activeTools: ["read", "grep", "glob", "lsp"],
			allTools: ["read", "grep", "glob", "lsp", semctxTool],
		});
		expect(
			await excluded.emit("before_agent_start", {
				prompt: "trace authentication",
				systemPrompt: ["base without semctx catalog"],
			}),
		).toBeUndefined();
	});

	test("does not mistake another MCP namespace for the OMP semctx marketplace server", async () => {
		const similarlyNamedTool = "mcp__semctx_other_search_codebase";
		const invoker = new FakeInvoker(() => undefined);
		const harness = makeHarness(invoker, {
			activeTools: ["read", similarlyNamedTool],
			allTools: ["read", similarlyNamedTool],
		});

		expect(
			await harness.emit("before_agent_start", {
				prompt: "trace authentication",
				systemPrompt: ["base"],
			}),
		).toBeUndefined();
		const callsAfterPrompt = invoker.calls.length;
		await harness.emit("tool_call", {
			toolCallId: "similarly-named",
			toolName: similarlyNamedTool,
			input: { query: "authentication" },
		});
		expect(invoker.calls).toHaveLength(callsAfterPrompt);
	});

	test("maps OMP search and semctx tools without propagating hook failures", async () => {
		const invoker = new FakeInvoker(() => {
			throw new Error("hook unavailable");
		});
		const harness = makeHarness(invoker);

		for (const [toolName, expected, input] of [
			["grep", "Grep", { pattern: "Store" }],
			["glob", "Glob", { pattern: "**/*.rs" }],
			["bash", "Bash", { command: "rg needle" }],
		] as const) {
			expect(
				await harness.emit("tool_call", {
					toolCallId: toolName,
					toolName,
					input,
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

		const callsBeforeNativeTools = invoker.calls.length;
		await harness.emit("tool_call", {
			toolCallId: "read",
			toolName: "read",
			input: { path: "src/main.rs" },
		});
		await harness.emit("tool_call", {
			toolCallId: "lsp",
			toolName: "lsp",
			input: { action: "references", file: "src/lib.rs", symbol: "Store" },
		});
		expect(invoker.calls).toHaveLength(callsBeforeNativeTools);
		expect(await harness.emit("context", { messages: [] })).toBeUndefined();
	});

	test("forwards semctx compliance and later searches to the shared rearm policy", async () => {
		const semctxTool = "mcp__semctx_semctx_search_codebase";
		let semctxUsed = false;
		let broadSearchesAfterSemctx = 0;
		const invoker = new FakeInvoker(input => {
			if (input.tool_name === semctxTool) {
				semctxUsed = true;
				return "unexpected compliance output";
			}
			if (semctxUsed && input.tool_name === "Grep") {
				broadSearchesAfterSemctx += 1;
				return broadSearchesAfterSemctx === 3 ? "rearmed search nudge" : undefined;
			}
			return undefined;
		});
		const harness = makeHarness(invoker);
		await harness.emit("before_agent_start", { prompt: "trace authentication", systemPrompt: [] });

		await harness.emit("tool_call", {
			toolCallId: "semctx",
			toolName: semctxTool,
			input: { query: "authentication" },
		});
		for (let search = 1; search <= 3; search += 1) {
			await harness.emit("tool_call", {
				toolCallId: `grep-${search}`,
				toolName: "grep",
				input: { pattern: "authentication" },
			});
			const context = await harness.emit("context", { messages: [] });
			if (search < 3) {
				expect(context).toBeUndefined();
			} else {
				expect(context).toMatchObject({
					messages: [{ content: "rearmed search nudge" }],
				});
			}
		}
		expect(
			invoker.calls
				.filter(call => call.input.hook_event_name === "PreToolUse")
				.map(call => call.input.tool_name),
		).toEqual([semctxTool, "Grep", "Grep", "Grep"]);
	});

	test("drops a delayed built-in nudge after semctx is used", async () => {
		const semctxTool = "mcp__semctx_semctx_search_codebase";
		const delayedNudge = deferred<string | undefined>();
		const invoker = new FakeInvoker(input => {
			if (input.tool_name === "Glob") return delayedNudge.promise;
			return undefined;
		});
		const harness = makeHarness(invoker);
		await harness.emit("before_agent_start", { prompt: "trace authentication", systemPrompt: [] });

		const broadSearch = harness.emit("tool_call", {
			toolCallId: "glob",
			toolName: "glob",
			input: { pattern: "**/*.rs" },
		});
		await harness.emit("tool_call", {
			toolCallId: "semctx",
			toolName: semctxTool,
			input: { query: "authentication" },
		});
		delayedNudge.resolve("stale nudge");
		await broadSearch;

		expect(await harness.emit("context", { messages: [] })).toBeUndefined();
	});

	test("never forwards native LSP calls to the semctl hook", async () => {
		const invoker = new FakeInvoker(() => "unexpected nudge");
		const harness = makeHarness(invoker);
		await harness.emit("before_agent_start", { prompt: "change Store", systemPrompt: [] });
		const callsAfterPrompt = invoker.calls.length;

		await harness.emit("tool_call", {
			toolCallId: "lsp-1",
			toolName: "lsp",
			input: { action: "references", file: "src/lib.rs", symbol: "Store" },
		});

		expect(invoker.calls).toHaveLength(callsAfterPrompt);
		expect(await harness.emit("context", { messages: [] })).toBeUndefined();
	});

	test("drops hook results that finish after a session boundary", async () => {
		const oldOrientation = deferred<string | undefined>();
		const invoker = new FakeInvoker(input => {
			if (input.session_id === "session-1") return oldOrientation.promise;
			return "new session orientation";
		});
		const harness = makeHarness(invoker);

		const oldStart = harness.emit("session_start");
		harness.setSessionId("session-2");
		await harness.emit("session_switch", { reason: "resume" });
		oldOrientation.resolve("stale session orientation");
		await oldStart;

		expect(harness.sent).toHaveLength(1);
		expect(harness.sent[0].message.content).toBe("new session orientation");
	});

	test("drops a delayed tool nudge after the next prompt starts", async () => {
		const delayedNudge = deferred<string | undefined>();
		const invoker = new FakeInvoker(input => {
			if (input.hook_event_name === "PreToolUse") return delayedNudge.promise;
			return undefined;
		});
		const harness = makeHarness(invoker);

		await harness.emit("before_agent_start", { prompt: "first prompt", systemPrompt: [] });
		const oldToolCall = harness.emit("tool_call", {
			toolCallId: "tool-1",
			toolName: "glob",
			input: { pattern: "**/*.rs" },
		});
		await harness.emit("before_agent_start", { prompt: "second prompt", systemPrompt: [] });
		delayedNudge.resolve("stale nudge");
		await oldToolCall;

		expect(await harness.emit("context", { messages: [] })).toBeUndefined();
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
