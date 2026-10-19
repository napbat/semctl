import * as path from "node:path";
import type { ExtensionAPI, ExtensionContext } from "@oh-my-pi/pi-coding-agent";
import { createSemctlHookInvoker } from "./hook-process";
import { installInspector, readDisplay, SemctlInspector } from "./inspector";
import { createGraphOverview } from "./overview";
import { installSyncStatus, readStatusUpdate } from "./status";

export { extractAdditionalContext } from "./hook-process";

const HOST = "omp";
const OMP_SEMCTX_TOOL_PREFIX = "mcp__semctx_";
const OMP_MARKETPLACE_TOOL_PREFIX = "mcp__semctx_semctx_";
const SESSION_TIMEOUT_MS = 8_000;
const PROMPT_TIMEOUT_MS = 12_000;
const TOOL_TIMEOUT_MS = 6_000;

const ORIENTATION_MESSAGE = "ca.napbat.semctx.orientation";
const PROMPT_CONTEXT_MESSAGE = "ca.napbat.semctx.prompt-context";

// OMP internal URLs (`omp://`, `local://`, `skill://`, `artifact://`, ...) and
// web URLs name harness or remote resources, not repository files. OMP also
// accepts the single-slash alias (`omp:/tools`). The scheme has two or more
// characters, so a Windows drive path (`C:/repo`) never matches.
const URL_PATH = /^[a-z][a-z0-9+.-]+:\//i;
// OMP accepts a trailing line selector on one file (`src/lib.rs:10-20`).
const LINE_SELECTOR = /:(?=[^:]*\d)[\d,+-]+$/;
// OMP `glob` treats a slash-only path as the session cwd.
const ROOT_ALIAS = /^[\\/]+$/;

type SessionSource = "startup" | "resume" | "clear" | "compact";
type HookEventName = "SessionStart" | "UserPromptSubmit" | "PreToolUse";
type HookContext = Pick<ExtensionContext, "cwd"> & {
	sessionManager: Pick<ExtensionContext["sessionManager"], "getSessionId">;
};
type OmpMcpTools = "native" | "marketplace" | "unavailable";

export interface SemctlHookInput {
	host: typeof HOST;
	omp_mcp_tools?: OmpMcpTools;
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

function hiddenMessage(customType: string, content: string) {
	return {
		customType,
		content,
		display: false,
		attribution: "agent" as const,
	};
}

/** A `PreToolUse` call in the Claude-shaped wire contract that `semctl hook` reads. */
interface HookToolCall {
	tool_name: string;
	tool_input: Record<string, unknown>;
	cwd: string;
}

/** The non-empty entries of an OMP semicolon-delimited `path` list. */
function pathEntries(value: unknown): string[] {
	if (typeof value !== "string") return [];
	return value
		.split(";")
		.map(entry => entry.trim())
		.filter(entry => entry.length > 0);
}

/**
 * OMP content searches and the input field that holds each search pattern.
 * `ast_grep` matches code structure and `find` is a natural-language discovery
 * query; both are repository discovery that semctl serves, so each maps to the
 * wire contract's `Grep`.
 */
const CONTENT_SEARCH_PATTERN: Record<string, string> = {
	grep: "pattern",
	ast_grep: "pat",
	find: "query",
};

const isObject = (input: unknown): input is Record<string, unknown> =>
	input !== null && typeof input === "object" && !Array.isArray(input);

/**
 * Map an OMP tool call onto the wire contract. OMP input shapes differ from
 * Claude's: `glob` carries its pattern in `path`, `grep` accepts a line
 * selector on one file, and `bash` can run in its own `cwd`. A search of
 * internal or web URLs is not repository discovery, so it is not sent.
 */
function hookToolCall(toolName: string, input: unknown, cwd: string): HookToolCall | undefined {
	if (!isObject(input)) return undefined;
	if (toolName.startsWith(OMP_SEMCTX_TOOL_PREFIX)) return { tool_name: toolName, tool_input: input, cwd };
	const patternField = CONTENT_SEARCH_PATTERN[toolName];
	if (patternField !== undefined) {
		const entries = pathEntries(input.path);
		if (entries.some(entry => URL_PATH.test(entry))) return undefined;
		const pattern = input[patternField];
		if (entries.length === 0) return { tool_name: "Grep", tool_input: { pattern }, cwd };
		const target = entries.length === 1 ? entries[0].replace(LINE_SELECTOR, "") : input.path;
		return { tool_name: "Grep", tool_input: { pattern, path: target }, cwd };
	}
	switch (toolName) {
		case "glob": {
			// OMP `glob` carries its pattern in `path`. The wire contract follows
			// Claude's `Glob`, which carries it in `pattern`.
			const entries = pathEntries(input.path);
			if (entries.some(entry => URL_PATH.test(entry))) return undefined;
			const wholeTree = entries.length === 0 || (entries.length === 1 && ROOT_ALIAS.test(entries[0]));
			return { tool_name: "Glob", tool_input: { pattern: wholeTree ? "**/*" : entries.join(";") }, cwd };
		}
		case "bash": {
			const shellCwd = typeof input.cwd === "string" ? input.cwd.trim() : "";
			if (URL_PATH.test(shellCwd)) return undefined;
			const commandCwd = shellCwd.length === 0 ? cwd : path.resolve(cwd, shellCwd);
			return { tool_name: "Bash", tool_input: input, cwd: commandCwd };
		}
		default:
			return undefined;
	}
}

/** Select the mounted server identity without rewriting Rust-owned guidance. */
function ompMcpTools(pi: ExtensionAPI): OmpMcpTools {
	const tools = pi.getAllTools();
	if (tools.some(tool => tool.name === `${OMP_SEMCTX_TOOL_PREFIX}search_codebase`)) return "native";
	if (tools.some(tool => tool.name === `${OMP_MARKETPLACE_TOOL_PREFIX}search_codebase`)) return "marketplace";
	return "unavailable";
}

export function createSemctxExtension(invoker: HookInvoker = createSemctlHookInvoker()) {
	return function semctxExtension(pi: ExtensionAPI): void {
		const overview = createGraphOverview(pi, {
			readDisplay, readStatusUpdate,
			createInspector: (display, requestRender, rawText, presentation) => new SemctlInspector(display, requestRender,
				() => [{ k: "text", p: { text: rawText.replace(/[\x00-\x08\x0b-\x1f\x7f-\x9f]/g, "") } }], rawText, "body", presentation ?? "inline"),
		});
		const instanceId = `${process.pid}-${Date.now().toString(36)}`;
		let promptGeneration = 0;
		let sessionGeneration = 0;
		let activePromptId = "";
		// The prompt of a submission that has not started a turn yet. OMP can run
		// `before_agent_start` again for the same submission before its first
		// turn. Reusing the prompt id keeps semctl's per-turn dedup intact.
		let preparingPrompt: string | undefined;

		pi.setLabel("semctx");

		const allocatePromptId = (ctx: HookContext) => {
			promptGeneration += 1;
			return `${ctx.sessionManager.getSessionId()}:${instanceId}:${promptGeneration}`;
		};
		const safeInvoke = async (input: SemctlHookInput, ctx: HookContext, timeoutMs: number) => {
			try {
				return await invoker.invoke({ ...input, omp_mcp_tools: ompMcpTools(pi) }, ctx, timeoutMs);
			} catch {
				return undefined;
			}
		};
		const resetTurnState = () => {
			sessionGeneration += 1;
			activePromptId = "";
			preparingPrompt = undefined;
		};
		const sendOrientation = async (source: SessionSource, ctx: ExtensionContext) => {
			if (source !== "compact") overview.reset();
			resetTurnState();
			const generation = sessionGeneration;
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
			if (generation === sessionGeneration && context !== undefined) {
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
			const generation = sessionGeneration;
			if (activePromptId === "" || preparingPrompt !== event.prompt) {
				activePromptId = allocatePromptId(ctx);
			}
			preparingPrompt = event.prompt;
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
			return generation !== sessionGeneration || context === undefined
				? undefined
				: { message: hiddenMessage(PROMPT_CONTEXT_MESSAGE, context) };
		});
		pi.on("turn_start", () => {
			preparingPrompt = undefined;
		});

		pi.on("tool_call", async (event, ctx) => {
			overview.observe(event, ctx);
			const generation = sessionGeneration;
			const call = hookToolCall(event.toolName, event.input, ctx.cwd);
			if (call === undefined) return undefined;
			activePromptId ||= allocatePromptId(ctx);
			const context = await safeInvoke(
				{
					host: HOST,
					hook_event_name: "PreToolUse",
					session_id: ctx.sessionManager.getSessionId(),
					prompt_id: activePromptId,
					...call,
				},
				ctx,
				TOOL_TIMEOUT_MS,
			);
			return generation !== sessionGeneration || context === undefined
				? undefined
				: { additionalContext: context };
		});
		pi.on("session_shutdown", () => {
			overview.reset();
			resetTurnState();
			invoker.shutdown();
		});
		pi.on("tool_result", (event, ctx) => overview.observe(event, ctx));
		pi.on("mcp_notification", (event, ctx) => overview.observe(event, ctx));
		installInspector(pi);
		installSyncStatus(pi, (ctx, mode) => mode === "live" ? overview.togglePopup(ctx) : overview.open(ctx));
	};
}

export default createSemctxExtension();
