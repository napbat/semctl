import { describe, expect, test } from "bun:test";
import * as path from "node:path";
import { GraphActivityTracker, type GraphActivityClock } from "./graph-activity";
import { readDisplay, type Display, type Hit } from "./inspector";
import { graphRoot } from "./overview";

const cwd = graphRoot(path.resolve("activity-checkout"))!;
const identity: Display["identity"] = { codebaseId: "activity-current", localRoot: cwd, copy: "checkout", name: null };
function overview(): Display {
	return { version: 1, kind: "graph", identity, title: "Indexed overview", query: null,
		nodes: [{ id: "file", label: "a.ts", category: "file", path: "src/a.ts", detail: null, hit: null }],
		edges: [], roots: ["file"], caveats: [], truncated: false };
}
function match(overrides: Partial<Hit> = {}): Hit {
	return { path: "src/a.ts", symbol: "run", qualifiedName: "app.run", language: "typescript", kind: "function", score: 2,
		startLine: 1, endLine: 1, startByte: 0, endByte: 10, snippet: "run()",
		snippetStartLine: 1, snippetEndLine: 1, snippetStartByte: 0, snippetEndByte: 10, focusLine: 1, focusByte: 0,
		codebaseId: identity.codebaseId, binding: { state: "bound", reason: null }, ...overrides };
}
function search(hits = [match()]): Display {
	return { version: 1, kind: "search", identity, query: "run", hits, indexWarning: null };
}
function source(sourcePath = "src/a.ts"): Display {
	return { version: 1, kind: "source", identity, path: sourcePath, text: "run()", language: "typescript",
		sourceOrigin: "content-plane", contentHash: "hash", encoding: "utf-8", binary: false, truncated: false,
		byteStart: 0, byteEnd: 5, fileSize: 5, lineStart: 1, lineEnd: 1 };
}
function clockHarness() {
	let now = 0;
	const callbacks: { callback(): void; at: number; active: boolean }[] = [];
	const clock: GraphActivityClock = {
		now: () => now,
		schedule(callback, delay) {
			const entry = { callback, at: now + delay, active: true };
			callbacks.push(entry);
			return () => { entry.active = false; };
		},
	};
	return { clock, callbacks, get pending() { return callbacks.filter(entry => entry.active).length; },
		advance(milliseconds: number) {
			now += milliseconds;
			for (const entry of callbacks) if (entry.active && entry.at <= now) { entry.active = false; entry.callback(); }
		} };
}
function trackerHarness() {
	const clock = clockHarness(); let renders = 0;
	const tracker = new GraphActivityTracker(graphRoot, readDisplay, () => renders++, clock.clock);
	return { tracker, clock, get renders() { return renders; } };
}
function call(tracker: GraphActivityTracker, id: string, input: Record<string, unknown> = {}, operation = "read_source", root = cwd): void {
	tracker.observeCall({ type: "tool_call", toolName: `mcp__semctx_${operation}`, toolCallId: id, input }, root);
}
function completed(id: string, display: unknown = source(), operation = "read_source", extra: Record<string, unknown> = {}): Record<string, unknown> {
	return { type: "tool_result", toolName: `mcp__semctx_${operation}`, toolCallId: id, input: {}, content: [{ type: "text", text: "Original text is not a metadata carrier" }],
		isError: false, details: { serverName: "semctx", mcpToolName: operation, mcpMeta: { semctlDisplay: display } }, ...extra };
}

describe("fail-closed checkout activity attribution", () => {
	test("learns default and directory scopes from trusted immutable IDs, while explicit matching IDs map immediately", () => {
		for (const selector of [undefined, null, identity.codebaseId, cwd, path.join(cwd, ".")]) {
			const h = trackerHarness(), nested = graphRoot(path.join(cwd, "nested"))!;
			call(h.tracker, "read", { path: "src/a.ts", codebase: selector }, "read_source", nested);
			expect(h.tracker.snapshot(nested, overview()).targets).toEqual(selector === identity.codebaseId ? ["src/a.ts"] : []);
			expect(h.tracker.snapshot(nested, overview()).operations).toEqual(["read_source"]);
			h.tracker.observeResult(completed("read"), overview(), true);
			expect(h.tracker.snapshot(nested, overview()).recent?.[0].targets).toEqual(["src/a.ts"]);
			expect(h.clock.pending).toBe(1);
			call(h.tracker, "subsequent", { path: "src/a.ts", codebase: selector }, "read_source", nested);
			expect(h.tracker.snapshot(nested, overview()).targets).toEqual(["src/a.ts"]);
			h.tracker.reset();
		}
	});
	test("rejects foreign and ambiguous selectors, missing checkout binding and escaping paths without losing observations", () => {
		const inputs = [
			{ path: "src/a.ts", codebase: "foreign" }, { path: "src/a.ts", codebase: path.resolve("foreign-checkout") },
			{ path: "src/a.ts", codebase: "activity-checkout" }, { path: "src/a.ts", codebase: 4 },
			{ path: "src/a.ts", scope: "any" }, { path: "src/a.ts", codebase_ids: [identity.codebaseId, "foreign"] },
			{ path: "../escape.ts" }, { path: "..\\escape.ts" }, { path: path.resolve(cwd, "../escape.ts") }, { path: "Z:/foreign/a.ts" },
		];
		for (const input of inputs) {
			const h = trackerHarness();
			call(h.tracker, "bad", { codebase: identity.codebaseId, ...input });
			expect(h.tracker.snapshot(cwd, overview()).targets).toEqual([]);
			h.tracker.observeResult(completed("bad", undefined, "read_source", { isError: true }), overview(), true);
			expect(h.tracker.snapshot(cwd, overview()).recent).toEqual([]);
			expect(h.tracker.snapshot(cwd, overview()).requests).toEqual({ sent: 1, received: 1, failed: 1 });
			expect(h.clock.pending).toBe(0);
		}
		const h = trackerHarness();
		call(h.tracker, "local", { path: "src/a.ts", symbol: "run", codebase: identity.codebaseId });
		for (const display of [undefined, { ...overview(), identity: { ...identity, localRoot: null } },
			{ ...overview(), identity: { ...identity, localRoot: path.resolve("foreign") } },
			{ ...overview(), identity: { ...identity, localRoot: null, copy: "canonical" as const } }]) {
			expect(h.tracker.snapshot(cwd, display).targets).toEqual([]);
			expect(h.tracker.snapshot(cwd, display).operations).toEqual(["read_source"]);
		}
	});
	test("keeps all correlated counters when display mapping is unavailable and bounds exact input targets", () => {
		const h = trackerHarness();
		for (let i = 0; i < 160; i++) call(h.tracker, String(i), { path: "src/a.ts" });
		expect(h.tracker.snapshot(cwd, undefined).requests?.sent).toBe(160);
		for (let i = 0; i < 160; i++) h.tracker.observeResult(completed(String(i)), undefined, true);
		expect(h.tracker.snapshot(cwd, undefined).requests).toEqual({ sent: 160, received: 160, failed: 0 });
		expect(h.clock.pending).toBe(0);
		call(h.tracker, "batch", { symbols: Array.from({ length: 400 }, (_, i) => `exact-${i}`), query: "not a target", symbol: "run", codebase: identity.codebaseId }, "batch_lookup");
		const targets = h.tracker.snapshot(cwd, overview()).targets;
		expect(targets).toHaveLength(256);
		expect(targets).not.toContain("not a target");
		expect(targets).toContain("run");
	});
	test("learned bindings are per raw server and selector, invalidate on foreign metadata, and clear on reset", () => {
		for (const selector of [undefined, cwd]) for (const invalid of [
			completed("active", { ...source(), identity: { ...identity, codebaseId: "foreign" } }),
			completed("active", source(), "read_source", { details: { serverName: "foreign", mcpToolName: "read_source", mcpMeta: { semctlDisplay: source() } } }),
			completed("active", source(), "read_source", { details: { serverName: "semctx", mcpToolName: "search_codebase", mcpMeta: { semctlDisplay: source() } } }),
		]) {
			const h = trackerHarness();
			call(h.tracker, "learn", { codebase: selector });
			h.tracker.observeResult(completed("learn"), overview(), false);
			call(h.tracker, "active", { path: "src/a.ts", codebase: selector });
			expect(h.tracker.snapshot(cwd, overview()).targets).toEqual(["src/a.ts"]);
			h.tracker.observeCall({ toolCallId: "namespace", toolName: "mcp__semctx_semctx_read_source", input: { path: "src/namespaced.ts", codebase: selector } }, cwd);
			call(h.tracker, "other-selector", { path: "src/other.ts", codebase: selector === undefined ? cwd : undefined });
			expect(h.tracker.snapshot(cwd, overview()).targets).toEqual(["src/a.ts"]);
			h.tracker.observeResult(invalid, overview(), true);
			call(h.tracker, "after-foreign", { path: "src/a.ts", codebase: selector });
			expect(h.tracker.snapshot(cwd, overview()).targets).toEqual([]);
			expect(h.tracker.snapshot(cwd, overview()).recent).toEqual([]);
			h.tracker.reset();
			call(h.tracker, "after-reset", { path: "src/a.ts", codebase: selector });
			expect(h.tracker.snapshot(cwd, overview()).targets).toEqual([]);
		}
	});
});

describe("typed completion receipts", () => {
	test("only the host's exact trusted carrier can highlight returned paths, never text or request echoes", () => {
		const variants: Record<string, unknown>[] = [
			{ details: undefined, content: [{ type: "text", text: JSON.stringify({ semctlDisplay: source() }) }] },
			{ details: { serverName: "foreign", mcpToolName: "read_source", mcpMeta: { semctlDisplay: source() } } },
			{ details: { serverName: "semctx:semctx", mcpToolName: "read_source", mcpMeta: { semctlDisplay: source() } } },
			{ details: { serverName: "semctx", mcpToolName: "search_codebase", mcpMeta: { semctlDisplay: source() } } },
			{ details: { serverName: "semctx", mcpToolName: "read_source", mcpMeta: { semctlDisplay: search() } } },
			{ details: { serverName: "semctx", mcpToolName: "read_source", mcpMeta: { semctlDisplay: { ...source(), version: 2 } } } },
			{ details: { result: { details: { serverName: "semctx", mcpToolName: "read_source", mcpMeta: { semctlDisplay: source() } } } } },
			{ toolName: "mcp__other_read_source" },
		];
		for (const extra of variants) {
			const h = trackerHarness(); call(h.tracker, "request", { path: "src/request.ts" });
			h.tracker.observeResult(completed("request", source(), "read_source", extra), overview(), true);
			expect(h.tracker.snapshot(cwd, overview()).recent).toEqual([]);
			expect(h.clock.pending).toBe(0);
		}
		const h = trackerHarness(); call(h.tracker, "request", { path: "src/request.ts" });
		h.tracker.observeResult(completed("request", source("src/returned.ts")), overview(), true);
		expect(h.tracker.snapshot(cwd, overview()).recent?.[0]).toEqual({ id: 1, operation: "read_source", outcome: "returned", targets: ["src/returned.ts"] });
		expect(h.tracker.snapshot(cwd, overview()).targets).toEqual([]);
		h.tracker.reset();
	});
	test("supports xd inner details and raw namespaced MCP identity, deduplicating nested host events", () => {
		const h = trackerHarness(), name = "mcp__semctx_semctx_search_codebase";
		const event = { type: "tool_call", toolName: "write", toolCallId: "xd", input: { path: `xd://${name}`, content: JSON.stringify({ query: "run" }) } };
		expect(h.tracker.observeCall(event, cwd)).toBe(true);
		expect(h.tracker.observeCall({ ...event, toolName: name, input: { query: "run" } }, cwd)).toBe(false);
		const details = { xdev: { tool: name, mode: "execute", args: { query: "run" }, inner: {
			serverName: "semctx:semctx", mcpToolName: "search_codebase", mcpMeta: { semctlDisplay: search() },
		} } };
		expect(h.tracker.observeResult(completed("xd", undefined, "search_codebase", { toolName: "write", details }), overview(), true)).toBe(true);
		expect(h.tracker.observeResult(completed("xd"), overview(), true)).toBe(false);
		expect(h.tracker.snapshot(cwd, overview()).requests).toEqual({ sent: 1, received: 1, failed: 0 });
		expect(h.tracker.snapshot(cwd, overview()).recent?.[0].targets).toEqual(["src/a.ts"]);
		h.tracker.reset();
		for (const wrong of [{ ...details.xdev, mode: "help" }, { ...details.xdev, tool: "mcp__other_search_codebase" }]) {
			h.tracker.observeCall(event, cwd);
			h.tracker.observeResult(completed("xd", undefined, "search_codebase", { toolName: "write", details: { xdev: wrong } }), overview(), true);
			expect(h.tracker.snapshot(cwd, overview()).recent).toEqual([]);
		}
	});
	test("extracts search, batch, source and graph paths while honoring per-hit codebase identities", () => {
		const hits = [match(), match({ path: "src/foreign.ts", codebaseId: "foreign" }), match({ path: "../escape.ts" }), match({ path: "src/inherited.ts", codebaseId: null })];
		const outputs: { operation: string; display: Display; targets: string[] }[] = [
			{ operation: "search_codebase", display: search(hits), targets: ["src/a.ts", "src/inherited.ts"] },
			{ operation: "batch_lookup", display: { version: 1, kind: "batch", identity, symbols: [{ symbol: "run", hits }] }, targets: ["src/a.ts", "src/inherited.ts"] },
			{ operation: "read_source", display: source(path.join(cwd, "src/a.ts")), targets: ["src/a.ts"] },
			{ operation: "imports", display: { ...overview(), kind: "graph", nodes: [
				{ id: "local", label: "run", detail: null, hit: match() },
				{ id: "foreign", label: "foreign", detail: null, hit: match({ path: "src/foreign.ts", codebaseId: "foreign" }) },
				{ id: "file", label: "b.ts", path: "src/b.ts", category: "file", detail: null, hit: null },
			], roots: ["local", "foreign", "file"] } as Display, targets: ["src/a.ts", "src/b.ts"] },
			{ operation: "search_codebase", display: { ...search(hits), identity: { ...identity, codebaseId: null, localRoot: null } }, targets: ["src/a.ts"] },
		];
		for (const { operation, display, targets } of outputs) {
			const h = trackerHarness(); call(h.tracker, operation, {}, operation);
			h.tracker.observeResult(completed(operation, display, operation), overview(), true);
			expect(h.tracker.snapshot(cwd, overview()).recent?.[0]?.targets).toEqual(targets);
			h.tracker.reset();
		}
		for (const foreign of [
			{ ...identity, codebaseId: "foreign" }, { ...identity, localRoot: path.resolve("foreign-checkout") },
		]) {
			const h = trackerHarness(); call(h.tracker, "foreign", {}, "search_codebase");
			h.tracker.observeResult(completed("foreign", { ...search(), identity: foreign }, "search_codebase"), overview(), true);
			expect(h.tracker.snapshot(cwd, overview()).recent).toEqual([]);
		}
	});
	test("failed requests retain only actual scoped inputs, not untrusted result paths", () => {
		const h = trackerHarness();
		call(h.tracker, "failed", { path: "src/request.ts", codebase: identity.codebaseId });
		h.tracker.observeResult(completed("failed", source("src/invented.ts"), "read_source", { isError: true }), overview(), true);
		expect(h.tracker.snapshot(cwd, overview()).recent?.[0]).toEqual({ id: 1, operation: "read_source", outcome: "failed", targets: ["src/request.ts"] });
		call(h.tracker, "untargeted", { query: "no invented target" }, "search_codebase");
		h.tracker.observeResult(completed("untargeted", search(), "search_codebase", { isError: true }), overview(), true);
		expect(h.tracker.snapshot(cwd, overview()).recent).toHaveLength(1);
		h.tracker.reset();
	});
	test("an outer successful xd write cannot hide inner MCP failure", () => {
		const h = trackerHarness(), name = "mcp__semctx_read_source";
		h.tracker.observeCall({ toolCallId: "xd-error", toolName: "write",
			input: { path: `xd://${name}`, content: JSON.stringify({ path: "src/request.ts", codebase: identity.codebaseId }) } }, cwd);
		h.tracker.observeResult(completed("xd-error", undefined, "read_source", {
			toolName: "write", isError: false, details: { xdev: { tool: name, mode: "execute", inner: {
				serverName: "semctx", mcpToolName: "read_source", isError: true, mcpMeta: { semctlDisplay: source("src/not-returned.ts") },
			} } },
		}), overview(), true);
		expect(h.tracker.snapshot(cwd, overview()).requests).toEqual({ sent: 1, received: 1, failed: 1 });
		expect(h.tracker.snapshot(cwd, overview()).recent?.[0]).toEqual({ id: 1, operation: "read_source", outcome: "failed", targets: ["src/request.ts"] });
		h.tracker.reset();
	});
});

describe("finite receipt lifecycle", () => {
	test("expires staggered bounded receipts with one cancellable one-shot and no idle work", () => {
		const h = trackerHarness();
		call(h.tracker, "first"); h.tracker.observeResult(completed("first"), overview(), true);
		h.clock.advance(1000);
		call(h.tracker, "second"); h.tracker.observeResult(completed("second"), overview(), true);
		expect(h.clock.pending).toBe(1);
		h.clock.advance(499);
		expect(h.tracker.snapshot(cwd, overview()).recent).toHaveLength(2);
		expect(h.renders).toBe(0);
		h.clock.advance(1);
		expect(h.tracker.snapshot(cwd, overview()).recent?.map(receipt => receipt.id)).toEqual([2]);
		expect(h.clock.pending).toBe(1);
		h.clock.advance(1000);
		expect(h.tracker.snapshot(cwd, overview()).recent).toEqual([]);
		expect(h.renders).toBe(2);
		expect(h.clock.pending).toBe(0);
		for (let i = 0; i < 40; i++) { call(h.tracker, String(i)); h.tracker.observeResult(completed(String(i)), overview(), true); }
		expect(h.tracker.snapshot(cwd, overview()).recent).toHaveLength(32);
		expect(h.clock.pending).toBe(1);
		h.clock.advance(1500);
		expect(h.clock.pending).toBe(0);
	});
	test("reset, close and replaced checkout identities cannot leak stale feedback or timer callbacks", () => {
		const h = trackerHarness();
		call(h.tracker, "first"); h.tracker.observeResult(completed("first"), overview(), true);
		const stale = h.clock.callbacks.at(-1)!;
		expect(h.tracker.snapshot(cwd, { ...overview(), identity: { ...identity, codebaseId: "replacement" } }).recent).toEqual([]);
		h.tracker.reset();
		expect(h.clock.pending).toBe(0);
		call(h.tracker, "second"); h.tracker.observeResult(completed("second"), overview(), true);
		stale.callback();
		expect(h.tracker.snapshot(cwd, overview()).recent?.[0].id).toBe(2);
		expect(h.clock.pending).toBe(1);
		expect(h.renders).toBe(0);
		h.tracker.clearReceipts();
		expect(h.clock.pending).toBe(0);
		call(h.tracker, "closed"); h.tracker.observeResult(completed("closed"), overview(), false);
		expect(h.tracker.snapshot(cwd, overview()).recent).toEqual([]);
		expect(h.clock.pending).toBe(0);
		h.tracker.reset();
		expect(h.tracker.observeResult(completed("second"), overview(), true)).toBe(false);
		expect(h.tracker.snapshot(cwd, overview()).requests).toEqual({ sent: 0, received: 0, failed: 0 });
	});
});
