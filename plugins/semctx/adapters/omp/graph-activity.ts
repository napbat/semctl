import * as path from "node:path";
import type { TspTone } from "@oh-my-pi/pi-wire";
import type { Display } from "./inspector";

/** A bounded, short-lived observation. It does not imply an internal search or embedding stage. */
export interface GraphReceipt {
	id: number;
	operation: string;
	outcome: "returned" | "failed";
	targets: readonly string[];
}

/** Session activity backed only by observed semctx requests and producer notifications. */
export interface GraphActivity {
	operations: string[];
	targets: string[];
	recent?: readonly GraphReceipt[];
	producer: null | { text: string; tone: TspTone; progress?: number; details?: string };
	requests?: { sent: number; received: number; failed: number };
}

const object = (value: unknown): value is Record<string, unknown> => value !== null && typeof value === "object" && !Array.isArray(value);
const boundedText = (value: unknown): value is string => typeof value === "string" && value.length > 0 && value.length <= 4096 && !/[\u0000-\u001f\u007f]/.test(value);
const LIMIT = 256;
const RECEIPT_MS = 1500;
interface ObservedCall {
	root: string;
	operation: string;
	name: string;
	server: string;
	selector: string | null | false;
	targets: { value: string; source: boolean }[];
}
export interface GraphActivityClock {
	now(): number;
	/** Return cancellation for a single one-shot callback. */
	schedule(callback: () => void, delay: number): () => void;
}
const systemClock: GraphActivityClock = {
	now: () => Date.now(),
	schedule(callback, delay) {
		const timer = setTimeout(callback, delay);
		return () => clearTimeout(timer);
	},
};

/** Observes host events, never transcript text or inferred producer stages. */
export class GraphActivityTracker {
	private readonly calls = new Map<string, ObservedCall>();
	private readonly traffic = new Map<string, { sent: number; received: number; failed: number }>();
	private readonly bindings = new Map<string, string>();
	private receipts: { root: string; checkout: string; codebaseId: string | null; receipt: GraphReceipt; expires: number }[] = [];
	private cancelExpiry: (() => void) | undefined;
	private expiryGeneration = 0;
	private nextReceipt = 0;
	constructor(private readonly normalizeRoot: (value: string) => string | undefined,
		private readonly readDisplay: (value: unknown, operation: string) => Display | undefined,
		private readonly changed: () => void,
		private readonly clock: GraphActivityClock = systemClock) {}

	reset(): void {
		this.calls.clear();
		this.traffic.clear();
		this.bindings.clear();
		this.clearReceipts();
	}
	clearReceipts(): void {
		this.expiryGeneration++;
		this.cancelExpiry?.();
		this.cancelExpiry = undefined;
		this.receipts = [];
	}
	private checkout(display: Display | undefined, cwd: string): string | undefined {
		if (!display || display.identity.copy !== "checkout" || !display.identity.localRoot) return undefined;
		const root = this.normalizeRoot(display.identity.localRoot);
		if (!root) return undefined;
		const relative = path.relative(root, cwd);
		return relative === "" || (relative !== ".." && !relative.startsWith(`..${path.sep}`) && !path.isAbsolute(relative)) ? root : undefined;
	}
	private bindingKey(call: ObservedCall): string {
		return `${call.server}\0${call.root}\0${call.selector === null ? "default" : typeof call.selector === "string" ? this.normalizeRoot(call.selector) ?? call.selector : "invalid"}`;
	}
	private matches(call: ObservedCall, display: Display | undefined, requireBinding = true): string | undefined {
		const root = this.checkout(display, call.root);
		if (!root || call.selector === false) return undefined;
		if (call.selector !== null && call.selector === display?.identity.codebaseId) return root;
		if (call.selector !== null && this.normalizeRoot(call.selector) !== root) return undefined;
		// A CLI checkout does not establish the MCP server's tenant/default binding.
		if (!requireBinding || display?.identity.codebaseId !== null && this.bindings.get(this.bindingKey(call)) === display?.identity.codebaseId) return root;
		return undefined;
	}
	private source(value: string, root: string): string | undefined {
		if (!boundedText(value)) return undefined;
		// Reject foreign platform absolutes as well as checkout traversal on this host.
		const portable = value.replace(/\\/g, "/");
		if ((/^[a-z]:/i.test(portable) || portable.startsWith("//")) && !path.isAbsolute(value)) return undefined;
		const absolute = path.isAbsolute(value) ? this.normalizeRoot(value) : path.resolve(root, portable);
		if (!absolute) return undefined;
		const relative = path.relative(root, absolute);
		if (!relative || relative === ".." || relative.startsWith(`..${path.sep}`) || path.isAbsolute(relative)) return undefined;
		return relative.replace(/\\/g, "/");
	}
	private targets(call: ObservedCall, display: Display | undefined): string[] {
		const root = this.matches(call, display);
		if (!root) return [];
		return [...new Set(call.targets.flatMap(target => {
			const value = target.source ? this.source(target.value, root) : target.value;
			return value ? [value] : [];
		}))].slice(0, LIMIT);
	}

	observeCall(event: Record<string, unknown>, root: string): boolean {
		if (typeof event.toolCallId !== "string" || typeof event.toolName !== "string") return false;
		let name = event.toolName;
		let input = object(event.input) ? event.input : undefined;
		if (name === "write") {
			if (!input || typeof input.path !== "string" || !input.path.startsWith("xd://")) return false;
			name = input.path.slice(5);
			const content = input.content;
			input = undefined;
			if (typeof content === "string" && content.length <= 64 * 1024) {
				try { const parsed: unknown = JSON.parse(content); if (object(parsed)) input = parsed; } catch { /* Count malformed dispatch, without mapping its targets. */ }
			}
		}
		const match = /^(mcp__semctx_semctx_|mcp__semctx_)([a-z_]{1,64})$/.exec(name);
		if (!match) return false;
		// xd dispatch may also emit the wrapped call with the same host call ID.
		if (this.calls.has(event.toolCallId)) return false;
		const counts = this.traffic.get(root) ?? { sent: 0, received: 0, failed: 0 };
		counts.sent++;
		this.traffic.set(root, counts);
		let selector: ObservedCall["selector"] = input ? null : false;
		if (input && input.codebase !== undefined && input.codebase !== null) selector = boundedText(input.codebase) ? input.codebase : false;
		// Multi-codebase/query scope syntax is not an exact local checkout selector.
		if (input && (input.scope !== undefined && input.scope !== null || input.codebase_ids !== undefined && input.codebase_ids !== null)) selector = false;
		const targets: ObservedCall["targets"] = [];
		if (input) {
			if (boundedText(input.path)) targets.push({ value: input.path, source: true });
			for (const value of [input.symbol, input.from, input.to]) if (boundedText(value)) targets.push({ value, source: false });
			if (Array.isArray(input.symbols)) for (const value of input.symbols.slice(0, LIMIT)) if (boundedText(value)) targets.push({ value, source: false });
		}
		this.calls.set(event.toolCallId, { root, name, operation: match[2], server: match[1] === "mcp__semctx_" ? "semctx" : "semctx:semctx", selector, targets });
		return true;
	}

	observeResult(event: Record<string, unknown>, display: Display | undefined, retainFeedback: boolean): boolean {
		if (typeof event.toolCallId !== "string") return false;
		const call = this.calls.get(event.toolCallId);
		if (!call) return false;
		this.calls.delete(event.toolCallId);
		// Host ToolResultEvent carries details directly; xd write results carry xdev.inner.
		let details = event.details;
		let trustedRoute = event.toolName === call.name;
		if (event.toolName === "write" && object(details) && object(details.xdev)) {
			trustedRoute = details.xdev.mode === "execute" && details.xdev.tool === call.name;
			details = details.xdev.inner;
		}
		const failed = event.isError === true || (trustedRoute && object(details) && details.isError === true);
		const counts = this.traffic.get(call.root);
		if (counts) { counts.received++; if (failed) counts.failed++; }
		let targets: string[] = failed && trustedRoute && retainFeedback ? this.targets(call, display) : [];
		const root = this.matches(call, display, false);
		const hasCarrier = object(details) && object(details.mcpMeta) && details.mcpMeta.semctlDisplay !== undefined;
		const returned = hasCarrier && trustedRoute && object(details) && details.serverName === call.server && details.mcpToolName === call.operation && object(details.mcpMeta)
			? this.readDisplay(details.mcpMeta.semctlDisplay, call.operation) : undefined;
		const scoped = returned && root && display && this.returnedScope(returned, display, root);
		if (hasCarrier && !scoped) { this.bindings.delete(this.bindingKey(call)); targets = []; }
		if (!failed && scoped && returned && root && display) {
			const parentScoped = returned.identity.codebaseId !== null && returned.identity.codebaseId === display.identity.codebaseId;
			if (parentScoped) this.bindings.set(this.bindingKey(call), returned.identity.codebaseId!);
			if (retainFeedback) {
				const add = (value: string | null, codebaseId: string | null = null) => {
					if (targets.length >= LIMIT || !value || (codebaseId === null ? !parentScoped : codebaseId !== display.identity.codebaseId)) return;
					const source = this.source(value, root);
					if (source && !targets.includes(source)) targets.push(source);
				};
				if (returned.kind === "search") returned.hits.forEach(hit => add(hit.path, hit.codebaseId));
				else if (returned.kind === "batch") returned.symbols.forEach(symbol => symbol.hits.forEach(hit => add(hit.path, hit.codebaseId)));
				else if (returned.kind === "source") add(returned.path);
				else if (returned.kind === "graph") for (const node of returned.nodes) {
					if (node.hit) add(node.hit.path, node.hit.codebaseId);
					else if (node.path) add(node.path);
				}
			}
		}
		if (!retainFeedback) return true;
		const checkout = this.matches(call, display, false);
		if (targets.length && checkout && display) {
			this.receipts.push({ root: call.root, checkout, codebaseId: display.identity.codebaseId,
				receipt: { id: ++this.nextReceipt, operation: call.operation, outcome: failed ? "failed" : "returned", targets }, expires: this.clock.now() + RECEIPT_MS });
			if (this.receipts.length > 32) this.receipts.shift();
			this.scheduleExpiry();
		}
		return true;
	}
	private returnedScope(returned: Display, overview: Display, root: string): boolean {
		const identity = returned.identity;
		if (identity.codebaseId !== null && identity.codebaseId !== overview.identity.codebaseId) return false;
		if (identity.localRoot !== null && this.normalizeRoot(identity.localRoot) !== root) return false;
		return true;
	}
	private scheduleExpiry(): void {
		this.cancelExpiry?.();
		this.cancelExpiry = undefined;
		const generation = ++this.expiryGeneration;
		if (!this.receipts.length) return;
		const expires = Math.min(...this.receipts.map(entry => entry.expires));
		this.cancelExpiry = this.clock.schedule(() => {
			if (generation !== this.expiryGeneration) return;
			this.cancelExpiry = undefined;
			const now = this.clock.now(), prior = this.receipts.length;
			this.receipts = this.receipts.filter(entry => entry.expires > now);
			this.scheduleExpiry();
			if (this.receipts.length !== prior) this.changed();
		}, Math.max(0, expires - this.clock.now()));
	}
	snapshot(root: string, display: Display | undefined): Pick<GraphActivity, "operations" | "targets" | "recent" | "requests"> {
		const calls = [...this.calls.values()].filter(call => call.root === root);
		const checkout = this.checkout(display, root);
		return {
			operations: calls.map(call => call.operation),
			targets: [...new Set(calls.flatMap(call => this.targets(call, display)))].slice(0, LIMIT),
			recent: this.receipts.filter(entry => entry.root === root && entry.checkout === checkout && entry.codebaseId === display?.identity.codebaseId).map(entry => entry.receipt),
			requests: this.traffic.get(root) ?? { sent: 0, received: 0, failed: 0 },
		};
	}
}
