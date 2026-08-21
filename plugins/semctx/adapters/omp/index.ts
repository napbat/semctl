import type { ExtensionAPI, ExtensionContext } from "@oh-my-pi/pi-coding-agent";

const HOST = "omp";
const OMP_SEMCTX_TOOL_PREFIX = "mcp__semctx_semctx_";
const MAX_HOOK_OUTPUT_BYTES = 64 * 1024;
const SESSION_TIMEOUT_MS = 8_000;
const PROMPT_TIMEOUT_MS = 12_000;
const TOOL_TIMEOUT_MS = 6_000;

const ORIENTATION_MESSAGE = "ca.napbat.semctx.orientation";
const PROMPT_CONTEXT_MESSAGE = "ca.napbat.semctx.prompt-context";
const NUDGE_MESSAGE = "ca.napbat.semctx.nudge";

// OMP's base prompt treats every LSP navigation action as mandatory whenever
// the LSP tool is present. Semctx is more specific for indexed, repository-wide
// retrieval, so append this narrow exception after OMP's base blocks. Keep the
// full routing table and degraded-state policy in the shared skill; this block
// only resolves the host-level precedence conflict and preserves LSP's live
// editor/edit-validation responsibilities.
export const SEMCTX_ROUTING_SYSTEM_PROMPT = `<semctx-tool-routing>
The connected semctx MCP tools and OMP's LSP are complementary. This is the specific exception to the generic LSP-first navigation policy above:
- For indexed repository discovery, use semctx first for semantic search, definitions, references, callers, implementations, type hierarchy, call paths/graphs, data flow, dependency structure, and cross-codebase queries.
- Keep LSP for diagnostics, hover, code actions, formatting/import fixes, live edit validation, unsupported semctx languages, and fallback when semctx reports unavailable, stale, or incomplete coverage.
- Before changing an exported symbol, use semctx for repository blast radius, then still run LSP references against the live workspace before editing.
- Use Read for exact current bytes and edit sites; a stale semctx hit must be re-read locally.
- Prefer LSP for ordinary rename/refactor actions. Use semctx symbolic edits when their guarded transaction, safe-delete analysis, or undo support is specifically useful, then validate with LSP diagnostics.
- For delegated semctx discovery, use OMP's general-purpose task agent, which inherits MCP. Fixed-tool specialist agents omit MCP; give them semctx findings as context rather than delegating the initial lookup to them.
- Never index an unindexed repository without explicit user opt-in.
- When applicable, read skill://codebase-retrieval for the detailed tool routing and degraded-state rules.
</semctx-tool-routing>`;

type SessionSource = "startup" | "resume" | "clear" | "compact";
type HookEventName = "SessionStart" | "UserPromptSubmit" | "PreToolUse";
export type HookContext = Pick<ExtensionContext, "cwd" | "setTimeout" | "clearTimer"> & {
	sessionManager: Pick<ExtensionContext["sessionManager"], "getSessionId">;
};
type HookTimer = Parameters<HookContext["clearTimer"]>[0];

interface HookChild {
	stdin: {
		write(data: string): number | Promise<number>;
		end(): number | Promise<number>;
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
	tool_name?: string;
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
				const spawned: HookChild = Bun.spawn(["semctl", "hook"], {
					cwd: ctx.cwd,
					env: process.env,
					stdin: "pipe",
					stdout: "pipe",
					stderr: "ignore",
				});
				child = spawned;
				children.add(spawned);
				timer = ctx.setTimeout(() => {
					try {
						child?.kill();
					} catch {
						// A concurrent clean exit is already the desired state.
					}
				}, timeoutMs);
				await spawned.stdin.write(JSON.stringify(input));
				await spawned.stdin.end();

				const stdout = await readBounded(spawned.stdout);
				if (stdout === undefined) spawned.kill();
				const exitCode = await spawned.exited;
				return exitCode === 0 && stdout !== undefined ? extractAdditionalContext(stdout) : undefined;
			} catch {
				try {
					child?.kill();
				} catch {
					// The child may already have exited or been killed by the deadline.
				}
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
	// OMP persists these as custom-message entries. Its memory backends retain
	// only primary user/assistant conversation turns, so indexed source snippets
	// do not leak into long-term memory as durable facts. Tool nudges below are
	// even shorter lived: the `context` event injects them for one provider call.
	return {
		customType,
		content,
		display: false,
		attribution: "agent" as const,
	};
}

function hookToolName(toolName: string): SemctlHookInput["tool_name"] {
	if (isSemctxMcpToolName(toolName)) return toolName;
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

function isSemctxMcpToolName(toolName: string): boolean {
	return toolName.startsWith(OMP_SEMCTX_TOOL_PREFIX);
}

function semctxToolsAreExposed(pi: ExtensionAPI, systemPrompt: readonly string[]): boolean {
	// Discoverable MCP tools normally live under xd:// rather than in
	// getActiveTools(). Conversely, getAllTools() also includes tools excluded by
	// an explicit --tools allowlist. Requiring either a top-level activation or a
	// prompt-catalog mention distinguishes both presentation modes from an
	// installed-but-disabled semctx server.
	try {
		const active = new Set(pi.getActiveTools());
		const renderedPrompt = systemPrompt.join("\n");
		return pi.getAllTools().some(tool => {
			const isSemctxMcp = tool.sourceInfo.source === "mcp" && isSemctxMcpToolName(tool.name);
			return isSemctxMcp && (active.has(tool.name) || renderedPrompt.includes(tool.name));
		});
	} catch {
		// Tool discovery must never make an OMP turn fail.
		return false;
	}
}

export function createSemctxExtension(invoker: HookInvoker = createSemctlHookInvoker()) {
	return function semctxExtension(pi: ExtensionAPI): void {
		const instanceId = `${process.pid}-${Date.now().toString(36)}`;
		let lifecycleGeneration = 0;
		let promptGeneration = 0;
		let activePromptId = "";
		let pendingNudge: string | undefined;
		let semctxUseGeneration = 0;

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
			semctxUseGeneration = 0;
		};
		const sendOrientation = async (source: SessionSource, ctx: HookContext) => {
			lifecycleGeneration += 1;
			const generation = lifecycleGeneration;
			const sessionId = ctx.sessionManager.getSessionId();
			resetTurnState();
			const context = await safeInvoke(
				{
					host: HOST,
					hook_event_name: "SessionStart",
					cwd: ctx.cwd,
					session_id: sessionId,
					source,
				},
				ctx,
				SESSION_TIMEOUT_MS,
			);
			if (
				context !== undefined &&
				generation === lifecycleGeneration &&
				ctx.sessionManager.getSessionId() === sessionId
			) {
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
			semctxUseGeneration = 0;
			const generation = lifecycleGeneration;
			const sessionId = ctx.sessionManager.getSessionId();
			const promptId = allocatePromptId(ctx);
			activePromptId = promptId;
			const context = await safeInvoke(
				{
					host: HOST,
					hook_event_name: "UserPromptSubmit",
					cwd: ctx.cwd,
					session_id: sessionId,
					prompt_id: promptId,
					prompt: event.prompt,
				},
				ctx,
				PROMPT_TIMEOUT_MS,
			);
			const current =
				generation === lifecycleGeneration &&
				activePromptId === promptId &&
				ctx.sessionManager.getSessionId() === sessionId;
			if (!current) return undefined;

			const routingEnabled =
				process.env.SEMCTX_HOOK_DISABLE === undefined &&
				semctxToolsAreExposed(pi, event.systemPrompt) &&
				!event.systemPrompt.includes(SEMCTX_ROUTING_SYSTEM_PROMPT);
			if (context === undefined && !routingEnabled) return undefined;

			return {
				...(context === undefined
					? {}
					: { message: hiddenMessage(PROMPT_CONTEXT_MESSAGE, context) }),
				...(routingEnabled
					? { systemPrompt: [...event.systemPrompt, SEMCTX_ROUTING_SYSTEM_PROMPT] }
					: {}),
			};
		});

		pi.on("tool_call", async (event, ctx) => {
			const isSemctxTool = isSemctxMcpToolName(event.toolName);
			if (isSemctxTool) {
				// Forward compliance so semctl owns cooling/rearm policy. The local
				// generation only invalidates a built-in nudge that was already in
				// flight when newer semctx evidence arrived.
				semctxUseGeneration += 1;
				pendingNudge = undefined;
			}
			const toolName = hookToolName(event.toolName);
			if (toolName === undefined) return undefined;
			activePromptId ||= allocatePromptId(ctx);
			const generation = lifecycleGeneration;
			const complianceGeneration = semctxUseGeneration;
			const sessionId = ctx.sessionManager.getSessionId();
			const promptId = activePromptId;
			const context = await safeInvoke(
				{
					host: HOST,
					hook_event_name: "PreToolUse",
					cwd: ctx.cwd,
					session_id: sessionId,
					prompt_id: promptId,
					tool_name: toolName,
					tool_input: { ...event.input },
				},
				ctx,
				TOOL_TIMEOUT_MS,
			);
			if (
				!isSemctxTool &&
				complianceGeneration === semctxUseGeneration &&
				context !== undefined &&
				generation === lifecycleGeneration &&
				activePromptId === promptId &&
				ctx.sessionManager.getSessionId() === sessionId
			) {
				pendingNudge = context;
			}
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
			lifecycleGeneration += 1;
			resetTurnState();
			invoker.shutdown();
		});
	};
}

export default createSemctxExtension();
