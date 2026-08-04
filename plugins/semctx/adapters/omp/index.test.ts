import { describe, expect, test } from "bun:test";
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

	test("injects prompt candidates and a one-shot namespaced tool nudge", async () => {
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

	test("maps only OMP search tools and never propagates hook failures", async () => {
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
					input: toolName === "bash" ? { command: "rg needle" } : { pattern: "**/*.rs" },
				}),
			).toBeUndefined();
			expect(invoker.calls.at(-1)?.input.tool_name).toBe(expected);
		}
		const callsBeforeRead = invoker.calls.length;
		await harness.emit("tool_call", {
			toolCallId: "read",
			toolName: "read",
			input: { path: "src/main.rs" },
		});
		expect(invoker.calls).toHaveLength(callsBeforeRead);
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
