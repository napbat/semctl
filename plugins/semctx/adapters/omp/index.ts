import type { ExtensionAPI, ExtensionContext } from "@oh-my-pi/pi-coding-agent";

const HOST = "omp";
const MAX_HOOK_OUTPUT_BYTES = 64 * 1024;
const SESSION_TIMEOUT_MS = 8_000;
const PROMPT_TIMEOUT_MS = 12_000;
const TOOL_TIMEOUT_MS = 6_000;

const ORIENTATION_MESSAGE = "ca.napbat.semctx.orientation";
const PROMPT_CONTEXT_MESSAGE = "ca.napbat.semctx.prompt-context";
const NUDGE_MESSAGE = "ca.napbat.semctx.nudge";

type SessionSource = "startup" | "resume" | "clear" | "compact";
type HookEventName = "SessionStart" | "UserPromptSubmit" | "PreToolUse";
type HookContext = Pick<ExtensionContext, "cwd" | "sessionManager" | "setTimeout" | "clearTimer">;
type HookTimer = Parameters<HookContext["clearTimer"]>[0];

interface HookChild {
	stdin: {
		write(data: string): number;
		end(): number;
	};
	stdout: ReadableStream<Uint8Array>;
	exited: Promise<number>;
	kill(): void;
}

export interface SemctlHookInput {
	host: typeof HOST;
	hook_event_name: HookEventName;
	cwd: string;
	session_id: string;
	prompt_id?: string;
	prompt?: string;
	source?: SessionSource;
	tool_name?: "Grep" | "Glob" | "Bash";
	tool_input?: Record<string, unknown>;
}

export interface HookInvoker {
	invoke(input: SemctlHookInput, ctx: HookContext, timeoutMs: number): Promise<string | undefined>;
	shutdown(): void;
}

async function readBounded(stream: ReadableStream<Uint8Array>): Promise<string | undefined> {
	const reader = stream.getReader();
	const chunks: Uint8Array[] = [];
	let total = 0;

	for (;;) {
		const { done, value } = await reader.read();
		if (done) break;
		total += value.byteLength;
		if (total > MAX_HOOK_OUTPUT_BYTES) {
			await reader.cancel();
			return undefined;
		}
		chunks.push(value);
	}

	const output = new Uint8Array(total);
	let offset = 0;
	for (const chunk of chunks) {
		output.set(chunk, offset);
		offset += chunk.byteLength;
	}
	return new TextDecoder().decode(output);
}

export function extractAdditionalContext(stdout: string): string | undefined {
	try {
		const parsed = JSON.parse(stdout) as {
			hookSpecificOutput?: { additionalContext?: unknown };
		};
		const context = parsed.hookSpecificOutput?.additionalContext;
		return typeof context === "string" && context.trim().length > 0 ? context : undefined;
	} catch {
		return undefined;
	}
}

function createSemctlHookInvoker(): HookInvoker {
	const children = new Set<HookChild>();

	return {
		async invoke(input, ctx, timeoutMs) {
			if (process.env.SEMCTX_HOOK_DISABLE !== undefined) return undefined;
			if (input.hook_event_name === "PreToolUse" && process.env.SEMCTX_NUDGE_DISABLE !== undefined) {
				return undefined;
			}

			let child: HookChild | undefined;
			let timer: HookTimer | undefined;
			try {
				child = Bun.spawn(["semctl", "hook"], {
					cwd: ctx.cwd,
					env: process.env,
					stdin: "pipe",
					stdout: "pipe",
					stderr: "ignore",
				});
				children.add(child);
				child.stdin.write(JSON.stringify(input));
				child.stdin.end();
				timer = ctx.setTimeout(() => {
					try {
						child?.kill();
					} catch {
						// A concurrent clean exit is already the desired state.
					}
				}, timeoutMs);

				const stdout = await readBounded(child.stdout);
				if (stdout === undefined) child.kill();
				const exitCode = await child.exited;
				return exitCode === 0 && stdout !== undefined ? extractAdditionalContext(stdout) : undefined;
			} catch {
				return undefined;
			} finally {
				if (timer !== undefined) ctx.clearTimer(timer);
				if (child !== undefined) children.delete(child);
			}
		},
		shutdown() {
			for (const child of children) {
				try {
					child.kill();
				} catch {
					// Best-effort shutdown must never tear down the OMP session.
				}
			}
			children.clear();
		},
	};
}

function hiddenMessage(customType: string, content: string) {
	return {
		customType,
		content,
		display: false,
		attribution: "agent" as const,
	};
}

function hookToolName(toolName: string): SemctlHookInput["tool_name"] {
	switch (toolName) {
		case "grep":
			return "Grep";
		case "glob":
			return "Glob";
		case "bash":
			return "Bash";
		default:
			return undefined;
	}
}

export function createSemctxExtension(invoker: HookInvoker = createSemctlHookInvoker()) {
	return function semctxExtension(pi: ExtensionAPI): void {
		const instanceId = `${process.pid}-${Date.now().toString(36)}`;
		let promptGeneration = 0;
		let activePromptId = "";
		let pendingNudge: string | undefined;

		pi.setLabel("semctx");

		const allocatePromptId = (ctx: HookContext) => {
			promptGeneration += 1;
			return `${ctx.sessionManager.getSessionId()}:${instanceId}:${promptGeneration}`;
		};
		const safeInvoke = async (input: SemctlHookInput, ctx: HookContext, timeoutMs: number) => {
			try {
				return await invoker.invoke(input, ctx, timeoutMs);
			} catch {
				return undefined;
			}
		};
		const resetTurnState = () => {
			activePromptId = "";
			pendingNudge = undefined;
		};
		const sendOrientation = async (source: SessionSource, ctx: HookContext) => {
			resetTurnState();
			const context = await safeInvoke(
				{
					host: HOST,
					hook_event_name: "SessionStart",
					cwd: ctx.cwd,
					session_id: ctx.sessionManager.getSessionId(),
					source,
				},
				ctx,
				SESSION_TIMEOUT_MS,
			);
			if (context !== undefined) {
				pi.sendMessage(hiddenMessage(ORIENTATION_MESSAGE, context), {
					deliverAs: "nextTurn",
				});
			}
		};

		pi.on("session_start", async (_event, ctx) => sendOrientation("startup", ctx));
		pi.on("session_switch", async (event, ctx) =>
			sendOrientation(event.reason === "new" ? "startup" : "resume", ctx),
		);
		pi.on("session_branch", async (_event, ctx) => sendOrientation("clear", ctx));
		pi.on("session_tree", async (_event, ctx) => sendOrientation("clear", ctx));
		pi.on("session_compact", async (_event, ctx) => sendOrientation("compact", ctx));

		pi.on("before_agent_start", async (event, ctx) => {
			pendingNudge = undefined;
			activePromptId = allocatePromptId(ctx);
			const context = await safeInvoke(
				{
					host: HOST,
					hook_event_name: "UserPromptSubmit",
					cwd: ctx.cwd,
					session_id: ctx.sessionManager.getSessionId(),
					prompt_id: activePromptId,
					prompt: event.prompt,
				},
				ctx,
				PROMPT_TIMEOUT_MS,
			);
			return context === undefined
				? undefined
				: { message: hiddenMessage(PROMPT_CONTEXT_MESSAGE, context) };
		});

		pi.on("tool_call", async (event, ctx) => {
			const toolName = hookToolName(event.toolName);
			if (toolName === undefined) return undefined;
			activePromptId ||= allocatePromptId(ctx);
			const context = await safeInvoke(
				{
					host: HOST,
					hook_event_name: "PreToolUse",
					cwd: ctx.cwd,
					session_id: ctx.sessionManager.getSessionId(),
					prompt_id: activePromptId,
					tool_name: toolName,
					tool_input: event.input,
				},
				ctx,
				TOOL_TIMEOUT_MS,
			);
			if (context !== undefined) pendingNudge = context;
			return undefined;
		});

		pi.on("context", event => {
			const context = pendingNudge;
			if (context === undefined) return undefined;
			pendingNudge = undefined;
			return {
				messages: [
					...event.messages,
					{
						role: "custom" as const,
						...hiddenMessage(NUDGE_MESSAGE, context),
						timestamp: Date.now(),
					},
				],
			};
		});

		pi.on("agent_end", () => {
			pendingNudge = undefined;
		});
		pi.on("session_shutdown", () => {
			resetTurnState();
			invoker.shutdown();
		});
	};
}

export default createSemctxExtension();
