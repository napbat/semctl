import { describe, expect, test } from "bun:test";
import type { ExtensionAPI, ExtensionContext } from "@oh-my-pi/pi-coding-agent";
import type { DescribeContext, NativeNode } from "@oh-my-pi/pi-tui";
import { InspectorModel, SemctlInspector, installInspector, readDisplay, snippetWindow, strengths, type Display, type Hit } from "./inspector";

function match(overrides: Partial<Hit> = {}): Hit {
	return { path: "src/main.ts", symbol: "main", qualifiedName: "app.main", language: "typescript", kind: "function", score: 2,
		startLine: 10, endLine: 11, startByte: 100, endByte: 120, snippet: "function main() {\n}",
		snippetStartLine: 10, snippetEndLine: 11, snippetStartByte: 100, snippetEndByte: 120, focusLine: 10, focusByte: 100,
		codebaseId: "cb", binding: { state: "bound", reason: null }, ...overrides };
}
function search(rows: Hit[] = [match()], query = "find main"): Display {
	return { version: 1, kind: "search", query, identity: { codebaseId: "cb", name: null, localRoot: "D:/repo", copy: "checkout" }, hits: rows };
}
const native: DescribeContext = { cols: 100, dark: true, reduceMotion: false, supports: () => true, feature: () => false };
function nodes(root: NativeNode | null): NativeNode[] {
	if (!root) return [];
	const result = [root];
	for (const c of root.c ?? []) if ("k" in c) result.push(...nodes(c));
	return result;
}
function activate(view: SemctlInspector, prefix: string): void {
	const control = nodes(view.describe(native)).find(n => n.key?.startsWith(prefix));
	if (!control?.key || typeof control.p?.actions?.click !== "string") throw new Error(`Missing native action: ${prefix}`);
	view.handleNativeEvent({ type: "action", key: control.key, act: control.p.actions.click, mods: [] });
}
function harness(hasUI = true) {
	type Handler = (event: Record<string, unknown>, ctx: ExtensionContext) => unknown;
	const handlers = new Map<string, Handler[]>();
	const widgets: Array<{ key: string; removed: boolean; placement: unknown }> = [];
	let component: SemctlInspector | undefined;
	let frames = 0;
	const toolNames = ["mcp__semctx_search_codebase", "mcp__semctx_read_source", "mcp__semctx_batch_lookup", "mcp__semctx_sync_status", "mcp__semctx_current_context", "mcp__semctx_semctx_search_codebase", "mcp__semctx_other_search_codebase"];
	const pi = {
		getAllTools: () => toolNames.map(name => ({ name })),
		on(event: string, handler: Handler) { handlers.set(event, [...(handlers.get(event) ?? []), handler]); },
	} as unknown as ExtensionAPI;
	const ctx = { hasUI, ui: { setWidget(key: string, content: unknown, options: { placement?: string } = {}) {
		widgets.push({ key, removed: content === undefined, placement: options.placement });
		if (typeof content === "function") component = content({ requestRender: () => { frames++; } });
		else if (content === undefined) component = undefined;
	} } } as unknown as ExtensionContext;
	const reset = installInspector(pi);
	return {
		widgets, get component() { return component; }, get frames() { return frames; },
		reset() { reset(ctx); },
		emit(event: string, payload: Record<string, unknown> = {}) {
			return (handlers.get(event) ?? []).map(h => h({ type: event, ...payload }, ctx));
		},
		start(id: string, target = "mcp__semctx_search_codebase", wrapped = false) {
			this.emit("tool_call", { toolCallId: id, toolName: wrapped ? "write" : target, input: wrapped ? { path: `xd://${target}`, content: "{}" } : {} });
		},
		result(id: string, display: unknown = search(), extra: Record<string, unknown> = {}) {
			const payload = { toolCallId: id, toolName: "mcp__semctx_search_codebase", input: {}, content: Object.freeze([{ type: "text", text: "Original MCP result" }]), isError: false,
				details: { serverName: "semctx", mcpToolName: "search_codebase", mcpMeta: { semctlDisplay: display } }, ...extra };
			const before = JSON.stringify(payload);
			expect(this.emit("tool_result", payload)).toEqual([undefined]);
			expect(JSON.stringify(payload)).toBe(before);
		},
	};
}

describe("bounded semctl display carrier", () => {
	test("requires version, operation, identity, full typed coordinates and the carrier bound", () => {
		expect(readDisplay(search(), "search_codebase")).toEqual(search());
		expect(readDisplay({ ...search(), query: undefined }, "search_codebase")).toBeUndefined();
		expect(readDisplay({ ...search(), query: 42 }, "search_codebase")).toBeUndefined();
		expect(readDisplay(search([], "x".repeat(32 * 1024)), "search_codebase")).toBeUndefined();
		for (const payload of [undefined, {}, { ...search(), version: 2 }, { ...search(), identity: null },
			search([match({ binding: { state: "wrong", reason: null } as Hit["binding"] })]),
			search([match({ snippet: "x".repeat(32 * 1024) })]), search(Array.from({ length: 33 }, () => match())),
			search([match({ score: Number.NaN })]), search([match({ startByte: -1 })]),
			search([match({ snippet: "中".repeat(12_000) })])]) expect(readDisplay(payload, "search_codebase")).toBeUndefined();
		expect(readDisplay(search(), "read_source")).toBeUndefined();
		expect(readDisplay(search(), "find_definition")).toBeUndefined();
		const cyclic: Record<string, unknown> = {}; cyclic.self = cyclic;
		expect(readDisplay(cyclic, "search_codebase")).toBeUndefined();
	});
	test("retains raw nullable and zero line coordinates but never invents numbering or focus", () => {
		expect(readDisplay(search([match({ path: null, codebaseId: null, startLine: 0 })]), "search_codebase")).toBeDefined();
		expect(snippetWindow(match())).toEqual({ start: 10, end: 11, focus: 10 });
		expect(snippetWindow(match({ snippetStartLine: 0 }))).toBeUndefined();
		expect(snippetWindow(match({ snippetEndLine: null }))).toBeUndefined();
		expect(snippetWindow(match({ snippet: "one line" }))).toBeUndefined();
		expect(snippetWindow(match({ focusLine: 999 }))?.focus).toBeUndefined();
		expect(snippetWindow(match({ focusByte: 999 }))?.focus).toBeUndefined();
		expect(snippetWindow(match({ binding: { state: "changed-local", reason: "changed" } }))?.focus).toBeUndefined();
	});
	test("normalizes only nonnegative query-relative scales, preserving ties and raw scores", () => {
		const values = (scores: (number | null)[]) => strengths(scores.map(score => match({ score })));
		expect(values([4, 2, 0])).toEqual([1, 0.5, 0]);
		expect(values([2, 2])).toEqual([1, 1]);
		expect(values([0, 0])).toEqual([null, null]);
		for (const invalid of [[4, -1], [-2, -1], [4, null], [4, Number.POSITIVE_INFINITY]]) expect(values(invalid)).toEqual([null, null]);
	});
	test("retains batch misses and applies the total-hit bound across symbols", () => {
		const payload = { version: 1, kind: "batch", identity: search().identity,
			symbols: [{ symbol: "missing", hits: [] }, { symbol: "main", hits: [match({ binding: { state: "not-probed", reason: null } })] }] };
		expect(readDisplay(payload, "batch_lookup")).toEqual(payload);
		expect(readDisplay({ ...payload, symbols: [{ symbol: "a", hits: Array.from({ length: 20 }, () => match()) }, { symbol: "b", hits: Array.from({ length: 13 }, () => match()) }] }, "batch_lookup")).toBeUndefined();
		const misses = { ...payload, symbols: Array.from({ length: 33 }, (_, i) => ({ symbol: `missing-${i}`, hits: [] })) };
		expect(readDisplay(misses, "batch_lookup")).toEqual(misses);
	});
	test("source carries exact byte provenance and numbers only a matching supplied line window", () => {
		const source = { version: 1, kind: "source", identity: search().identity, path: "src/main.ts", text: "one\ntwo",
			language: "typescript", sourceOrigin: "verified-local", contentHash: "hash", encoding: "utf-8",
			binary: false, truncated: false, byteStart: 0, byteEnd: 7, fileSize: 7, lineStart: 20, lineEnd: 21 };
		const display = readDisplay(source, "read_source");
		expect(display).toBeDefined();
		const model = new InspectorModel(); model.start("source", "mcp__semctx_read_source", "read_source"); model.finish("source", display, false);
		expect(nodes(new SemctlInspector(model, () => {}).describe(native)).find(n => n.k === "code")?.p).toMatchObject({ start: 20, numbers: true });
		const mismatch = readDisplay({ ...source, lineEnd: 30 }, "read_source");
		model.start("mismatch", "mcp__semctx_read_source", "read_source"); model.finish("mismatch", mismatch, false);
		expect(nodes(new SemctlInspector(model, () => {}).describe(native)).find(n => n.k === "code")?.p).toMatchObject({ numbers: false });
		expect(readDisplay({ ...source, byteStart: -1 }, "read_source")).toBeUndefined();
		expect(readDisplay({ ...source, byteEnd: 8 }, "read_source")).toBeUndefined();
	});
});

describe("one native inspector lifecycle", () => {
	test("an older completion cannot replace the latest requested result", () => {
		const h = harness(); h.start("a"); h.start("b");
		h.result("b", search([match({ symbol: "new" })])); h.result("a", search([match({ symbol: "old" })]));
		expect(h.component?.model.current?.id).toBe("b");
		expect(h.widgets).toEqual([{ key: "semctl.inspector", removed: false, placement: "aboveEditor" }]);
	});
	test("has spinner/elapsed only during observed execution and never from an index plan", () => {
		const h = harness(); h.start("a");
		expect(nodes(h.component!.describe(native)).some(n => n.k === "spinner" || n.k === "elapsed")).toBe(false);
		h.emit("tool_execution_start", { toolCallId: "a", toolName: "mcp__semctx_search_codebase" });
		expect(nodes(h.component!.describe(native)).some(n => n.k === "spinner")).toBe(true);
		h.result("a");
		expect(nodes(h.component!.describe(native)).some(n => ["spinner", "elapsed", "progress"].includes(n.k))).toBe(false);
	});
	test("absent, malformed, wrong-origin and error results replace prior views without mutation", () => {
		const h = harness(); h.start("ok"); h.result("ok");
		for (const [i, extra] of [
			{ details: undefined }, { details: { serverName: "semctx-other", mcpToolName: "search_codebase", mcpMeta: { semctlDisplay: search() } } },
			{ details: { serverName: "semctx", mcpToolName: "read_source", mcpMeta: { semctlDisplay: search() } } }, { isError: true },
		].entries()) {
			h.start(`bad-${i}`); h.result(`bad-${i}`, { version: 99 }, extra);
			expect(h.component?.model.current?.display).toBeUndefined();
		}
		expect(h.component?.model.current?.state).toBe("error");
	});
	test("supports exact structured xd envelope without parsing content and deduplicates nested same-id hooks", () => {
		const h = harness(); h.start("wrapped", "mcp__semctx_search_codebase", true);
		h.start("wrapped"); // Inner ExtensionToolWrapper shares the outer write call id.
		h.result("wrapped"); // Inner event is not a second visible call or the authoritative outer result.
		expect(h.component?.model.current?.state).toBe("pending");
		h.result("wrapped", undefined, { toolName: "write", details: { xdev: { tool: "mcp__semctx_search_codebase", mode: "execute", inner: { serverName: "semctx", mcpToolName: "search_codebase", mcpMeta: { semctlDisplay: search() } } } } });
		expect(h.component?.model.current?.display?.kind).toBe("search");
		h.start("mismatch", "mcp__semctx_search_codebase", true);
		h.result("mismatch", undefined, { toolName: "write", details: { xdev: { tool: "mcp__other_search_codebase", mode: "execute", inner: { serverName: "semctx", mcpToolName: "search_codebase", mcpMeta: { semctlDisplay: search() } } } } });
		expect(h.component?.model.current?.display).toBeUndefined();
	});
	test("reset between outer and nested starts retains authority until the outer terminal event", () => {
		for (const executing of [false, true]) {
			const h = harness(); h.start("reuse", "mcp__semctx_search_codebase", true);
			if (executing) h.emit("tool_execution_start", { toolCallId: "reuse", toolName: "write" });
			h.reset(); h.start("reuse"); h.result("reuse");
			expect(h.component).toBeUndefined();
			h.result("reuse", undefined, { toolName: "write", details: { xdev: { tool: "mcp__semctx_search_codebase", mode: "execute", inner: { serverName: "semctx", mcpToolName: "search_codebase", mcpMeta: { semctlDisplay: search() } } } } });
			expect(h.component).toBeUndefined();
			if (executing) {
				h.start("reuse"); expect(h.component).toBeUndefined();
				h.emit("tool_execution_end", { toolCallId: "reuse", toolName: "write", result: {}, isError: false });
			}
			h.start("reuse"); h.result("reuse", search([match({ symbol: "new-call" })]));
			expect(h.component?.model.current?.state).toBe("done");
			expect(h.component?.model.current?.display).toEqual(search([match({ symbol: "new-call" })]));
		}
	});
	test("conflicting duplicate call targets cannot replace the first outer authority", () => {
		const h = harness(); h.start("conflict", "mcp__semctx_search_codebase", true);
		h.start("conflict", "mcp__semctx_semctx_search_codebase");
		h.result("conflict", undefined, { toolName: "write", details: { xdev: { tool: "mcp__semctx_semctx_search_codebase", mode: "execute",
			inner: { serverName: "semctx:semctx", mcpToolName: "search_codebase", mcpMeta: { semctlDisplay: search() } } } } });
		expect(h.component?.model.current?.display).toBeUndefined();
		expect(h.component?.model.current?.note).toContain("Unrecognized MCP origin");
	});
	test("completed provider call IDs can be reused as new calls rather than permanently blacklisted", () => {
		const h = harness(); h.start("same"); h.result("same");
		const firstOrder = h.component?.model.current?.order;
		h.start("same"); expect(h.component?.model.current?.state).toBe("pending");
		h.result("same", search([match({ symbol: "second" })]));
		expect(h.component?.model.current?.order).toBeGreaterThan(firstOrder!);
		expect(h.component?.model.current?.display).toEqual(search([match({ symbol: "second" })]));
	});
	test("terminal completion releases bounded authority across successive calls", () => {
		const h = harness();
		for (let i = 0; i < 80; i++) { h.start(`done-${i}`); h.result(`done-${i}`); }
		expect(h.component?.model.current?.id).toBe("done-79");
		expect(h.component?.model.current?.state).toBe("done");
	});
	test("reset protects in-flight IDs no longer displayed", () => {
		const h = harness();
		for (let i = 0; i < 12; i++) h.start(`old-${i}`, "mcp__semctx_search_codebase", true);
		h.reset(); h.start("old-0"); h.result("old-0");
		expect(h.component).toBeUndefined();
	});
	test("in-flight saturation closes the inspector rather than forgetting retired call authority", () => {
		const h = harness();
		for (let i = 0; i < 65; i++) h.start(`inflight-${i}`, "mcp__semctx_search_codebase", true);
		expect(h.component).toBeUndefined();
		h.reset(); h.start("inflight-0"); h.result("inflight-0");
		h.start("new-untracked"); h.result("new-untracked");
		expect(h.component).toBeUndefined();
	});
	test("real tool errors use failed-call messaging while preserving the original result", () => {
		const h = harness(); h.start("failure"); h.result("failure", search(), { isError: true, details: undefined });
		expect(h.component?.model.current?.state).toBe("error");
		expect(h.component?.model.current?.note).toBe("MCP call failed; see the original error in the transcript.");
		expect(h.component?.model.current?.display).toBeUndefined();
	});
	test("accepts exact marketplace raw identity, never a sanitized collision", () => {
		const h = harness(); h.start("market", "mcp__semctx_semctx_search_codebase");
		h.result("market", search(), { toolName: "mcp__semctx_semctx_search_codebase", details: { serverName: "semctx:semctx", mcpToolName: "search_codebase", mcpMeta: { semctlDisplay: search() } } });
		expect(h.component?.model.current?.display).toBeDefined();
		h.start("collision", "mcp__semctx_semctx_search_codebase");
		h.result("collision", search(), { toolName: "mcp__semctx_semctx_search_codebase", details: { serverName: "semctx-semctx", mcpToolName: "search_codebase", mcpMeta: { semctlDisplay: search() } } });
		expect(h.component?.model.current?.display).toBeUndefined();
	});
	test("a mounted semctl operation without display metadata replaces the old result with an honest fallback", () => {
		const h = harness(); h.start("old"); h.result("old");
		h.start("context", "mcp__semctx_current_context");
		h.result("context", undefined, { toolName: "mcp__semctx_current_context", details: { serverName: "semctx", mcpToolName: "current_context" } });
		expect(h.component?.model.current?.id).toBe("context");
		expect(h.component?.model.current?.display).toBeUndefined();
		expect(h.component?.model.current?.note).toContain("unavailable");
	});
	test("cancellation and reset discard pending authority, including late completion", () => {
		const h = harness(); h.start("cancel"); h.emit("agent_end"); h.result("cancel");
		expect(h.component?.model.current?.state).toBe("cancelled");
		expect(h.component?.model.current?.display).toBeUndefined();
		h.start("reset"); h.reset(); h.result("reset");
		expect(h.component).toBeUndefined();
		expect(h.widgets.at(-1)?.removed).toBe(true);
	});
	test("keeps the latest result, handles end-only errors and no-ops noninteractive hosts", () => {
		const h = harness(); for (let i = 0; i < 12; i++) h.start(String(i));
		expect(h.component?.model.current?.id).toBe("11");
		h.emit("tool_execution_end", { toolCallId: "11", toolName: "mcp__semctx_search_codebase", isError: true, result: {} });
		expect(h.component?.model.current?.state).toBe("error");
		const headless = harness(false); headless.start("a"); headless.result("a"); headless.reset();
		expect(headless.widgets).toEqual([]);
	});
	test("resets execution timing at actual start rather than counting approval wait", () => {
		let now = 10; const model = new InspectorModel(() => now);
		model.start("a", "mcp__semctx_search_codebase", "search_codebase");
		now = 1010; model.running("a"); now = 1020;
		expect(model.age(model.current!)).toBe(10);
	});
});

describe("native provenance presentation", () => {
	test("exact query remains bound to the current result across hit selection", () => {
		const h = harness(), older = "  exact \"quoted\" \\\\ query\nsecond line \u4e2d", newer = "different query";
		h.start("older"); h.result("older", search([match()], older));
		expect(nodes(h.component!.describe(native)).find(n => n.key === "query")?.p?.title).toBe(older);
		h.start("newer"); h.result("newer", search([match(), match({ path: "other.ts" })], newer));
		const described = nodes(h.component!.describe(native));
		expect(described.find(n => n.key === "query")?.p?.title).toBe(newer);
		h.component!.handleNativeEvent({ type: "action", key: "hit-1", act: "hit:1", mods: [] });
		expect(nodes(h.component!.describe(native)).find(n => n.key === "query")?.p?.title).toBe(newer);
	});
	test("more than 32 unmatched batch symbols remain available through explicit expansion", () => {
		const payload = readDisplay({ version: 1, kind: "batch", identity: search().identity,
			symbols: Array.from({ length: 33 }, (_, i) => ({ symbol: `missing-${i}`, hits: [] })) }, "batch_lookup");
		expect(payload).toBeDefined();
		const model = new InspectorModel(); model.start("misses", "mcp__semctx_batch_lookup", "batch_lookup"); model.finish("misses", payload, false);
		const view = new SemctlInspector(model, () => {});
		expect(nodes(view.describe(native)).filter(n => n.key?.startsWith("batch-miss-"))).toHaveLength(8);
		activate(view, "matches-control-");
		expect(nodes(view.describe(native)).filter(n => n.key?.startsWith("batch-miss-"))).toHaveLength(33);
		expect(nodes(view.describe(native)).some(n => n.k === "text" && n.p?.text === "missing-32 · no match")).toBe(true);
	});
	test("uses only the supplied checkout basename for display while retaining full identity on hover", () => {
		const payload = search();
		payload.identity = { codebaseId: "full-codebase-identifier", name: null, localRoot: "/workspace/project", copy: "checkout" };
		const model = new InspectorModel(); model.start("identity", "mcp__semctx_search_codebase", "search_codebase"); model.finish("identity", payload, false);
		const described = nodes(new SemctlInspector(model, () => {}).describe(native));
		const header = described.find(n => n.k === "text" && n.p?.text === "checkout · project");
		expect(header?.p?.title).toBe("Codebase ID: full-codebase-identifier\nCopy: checkout\nLocal root: /workspace/project");
		const visible = described.flatMap(n => n.k === "text" && n.p?.text ? [n.p.text] : []).join("\n");
		expect(visible).not.toContain("full-codebase-identifier");
		expect(visible).not.toContain("/workspace/project");
	});
	test("bounds search around verified focus and read_source from its first line without changing raw results", () => {
		const lines = Array.from({ length: 40 }, (_, i) => `line ${i + 1}\n`), sourceText = lines.join("");
		const payload = search([match({ snippet: sourceText, snippetStartLine: 100, snippetEndLine: 139, focusLine: 129 })]);
		const model = new InspectorModel(); model.start("search", "mcp__semctx_search_codebase", "search_codebase"); model.finish("search", payload, false);
		const view = new SemctlInspector(model, () => {});
		expect(nodes(view.describe(native)).find(n => n.k === "code")?.p).toMatchObject({
			text: lines.slice(23, 35).join("").slice(0, -1), start: 123, numbers: true, marks: [{ line: 129, tone: "accent" }],
		});
		expect(model.current?.display).toEqual(payload);
		const source: Display = { version: 1, kind: "source", identity: search().identity, path: "src/main.ts", text: sourceText,
			language: "typescript", sourceOrigin: "content-plane", contentHash: "hash", encoding: "utf-8", binary: false, truncated: false,
			byteStart: 0, byteEnd: sourceText.length, fileSize: sourceText.length, lineStart: 200, lineEnd: 239 };
		model.start("source", "mcp__semctx_read_source", "read_source"); model.finish("source", source, false);
		expect(nodes(view.describe(native)).find(n => n.k === "code")?.p).toMatchObject({ text: lines.slice(0, 16).join("").slice(0, -1), start: 200 });
		expect(model.current?.display).toEqual(source);
	});
	test.each(["first\nlast\n", "first\r\nlast\r\n", "first\n\n"])("a terminating line break does not invent another numbered line: %j", snippet => {
		const model = new InspectorModel(); model.start("lines", "mcp__semctx_search_codebase", "search_codebase");
		model.finish("lines", search([match({ snippet, snippetStartLine: 10, snippetEndLine: 11, focusLine: 10 })]), false);
		const code = nodes(new SemctlInspector(model, () => {}).describe(native)).find(n => n.k === "code");
		expect((code?.p?.text as string).split(/\r?\n/)).toEqual(snippet.startsWith("first\n\n") ? ["first", ""] : ["first", "last"]);
	});
	test("compact batch entries are bounded and explicit expansion retains grouped hits and misses in caller order", () => {
		const model = new InspectorModel(); model.start("batch", "mcp__semctx_batch_lookup", "batch_lookup");
		const payload: Display = { version: 1, kind: "batch", identity: search().identity, symbols: [
			{ symbol: "early-miss", hits: [] }, { symbol: "many", hits: Array.from({ length: 10 }, () => match()) }, { symbol: "late-miss", hits: [] },
		] };
		model.finish("batch", payload, false);
		const view = new SemctlInspector(model, () => {});
		const compact = nodes(view.describe(native));
		expect(compact.filter(n => /^(hit|batch-group|batch-miss)-/.test(n.key ?? ""))).toHaveLength(8);
		expect(compact.some(n => n.k === "text" && n.p?.text === "late-miss · no match")).toBe(false);
		activate(view, "matches-control-");
		const all = nodes(view.describe(native));
		expect(all.filter(n => n.key?.startsWith("hit-"))).toHaveLength(10);
		expect(all.filter(n => n.key?.startsWith("batch-")).map(n => n.k === "text" ? n.p?.text : undefined)).toEqual(["early-miss · no match", "many · 10 matches", "late-miss · no match"]);
		activate(view, "matches-control-");
		expect(nodes(view.describe(native)).filter(n => /^(hit|batch-group|batch-miss)-/.test(n.key ?? ""))).toHaveLength(8);
		expect(model.current?.display).toEqual(payload);
	});
	test("search expansion retains ranked source selection without unbounding a newly selected source", () => {
		const model = new InspectorModel(); model.start("search", "mcp__semctx_search_codebase", "search_codebase");
		model.finish("search", search(Array.from({ length: 12 }, (_, i) => match({ path: `src/${i}.ts` }))), false);
		const view = new SemctlInspector(model, () => {});
		expect(nodes(view.describe(native)).filter(n => n.key?.startsWith("hit-"))).toHaveLength(8);
		activate(view, "matches-control-");
		expect(nodes(view.describe(native)).filter(n => n.key?.startsWith("hit-"))).toHaveLength(12);
		view.handleNativeEvent({ type: "action", key: "hit-10", act: "hit:10", mods: [] });
		expect(nodes(view.describe(native)).find(n => n.k === "code")?.p).toMatchObject({ path: "src/10.ts" });
		activate(view, "matches-control-");
		expect(nodes(view.describe(native)).filter(n => n.key?.startsWith("hit-"))).toHaveLength(8);
		expect(nodes(view.describe(native)).find(n => n.k === "code")?.p).toMatchObject({ path: "src/10.ts" });
	});
	test("plain fallback respects ASCII output and narrow-width bounds", () => {
		const model = new InspectorModel(); model.start("id-with-unicode-\u4e2d", "mcp__semctx_search_codebase", "search_codebase");
		model.finish("id-with-unicode-\u4e2d", search([match({ path: "\u4e2d.ts", snippet: "\u4e2d\n\u6587" })]), false);
		const view = new SemctlInspector(model, () => {});
		expect(view.render(9).every(line => /^[\x20-\x7e]*$/.test(line) && line.length <= 9)).toBe(true);
		expect(view.render(0).every(line => line === "")).toBe(true);
	});
	test("uses compact themed verification icons with precise scope and retained failure reasons only on hover", () => {
		const rows = [
			match(),
			match({ binding: { state: "changed-local", reason: "Local hash changed" } }),
			match({ binding: { state: "range-mismatch", reason: "Requested range differs" } }),
			match({ binding: { state: "unverified", reason: "Probe unavailable" } }),
			match({ binding: { state: "not-probed", reason: "Batch does not probe sources" } }),
		];
		const model = new InspectorModel(); model.start("icons", "mcp__semctx_search_codebase", "search_codebase");
		model.finish("icons", search(rows), false);
		const view = new SemctlInspector(model, () => {}), described = nodes(view.describe(native));
		const icons = described.filter(n => n.k === "icon");
		expect(icons.map(n => n.p && "name" in n.p ? n.p.name : undefined)).toEqual(["check", "warn", "x-circle", "info", "info", "check"]);
		expect(icons[0].p).toMatchObject({ tone: "success", title: "bound · Source bytes and coordinates verified; semantic symbol/focus identity is not verified." });
		expect(icons[1].p?.title).toContain("Local hash changed");
		expect(icons[2].p?.title).toContain("Requested range differs");
		expect(icons[3].p?.title).toContain("Probe unavailable");
		expect(icons[4].p?.title).toContain("Batch does not probe sources");
		const visibleText = described.flatMap(n => n.k === "text" && n.p?.text ? [n.p.text] : []).join("\n");
		expect(visibleText).not.toContain("semantic symbol/focus identity");
		expect(visibleText).not.toContain("Local hash changed");
		const fallback = nodes(view.describe({ ...native, supports: kind => kind !== "icon" }));
		expect(fallback.some(n => n.k === "icon")).toBe(false);
		expect(fallback.some(n => n.k === "text" && n.p?.text === "bound")).toBe(true);
	});
	test("unavailable batch scores are static neutral dashes, while finite zero remains an empty meter", () => {
		const model = new InspectorModel(); model.start("batch", "mcp__semctx_batch_lookup", "batch_lookup");
		model.finish("batch", { version: 1, kind: "batch", identity: search().identity, symbols: [
			{ symbol: "main", hits: [match({ score: null })] },
		] }, false);
		const view = new SemctlInspector(model, () => {});
		let described = nodes(view.describe(native));
		expect(described.some(n => n.k === "meter" || n.k === "spinner" || n.k === "progress")).toBe(false);
		const unavailable = described.find(n => n.k === "text" && n.p?.text === "—");
		expect(unavailable?.p?.tone).toBe("muted");
		expect(unavailable?.p?.title).toContain("Relative match score");
		expect(unavailable?.p?.title).toContain("raw hybrid score unavailable");
		expect(unavailable?.p?.title).toContain("query-relative, not confidence");
		expect(unavailable?.p?.aria).toBe(unavailable?.p?.title);
		model.start("scores", "mcp__semctx_search_codebase", "search_codebase"); model.finish("scores", search([match({ score: 2 }), match({ score: 0 })]), false);
		described = nodes(view.describe(native));
		expect(described.filter(n => n.k === "meter").map(n => n.p && "value" in n.p ? n.p.value : undefined)).toEqual([1, 0]);
	});
	test("uses native meters/tooltips and validated indexed-focus source, with supported selection", () => {
		const model = new InspectorModel(); model.start("a", "mcp__semctx_search_codebase", "search_codebase");
		model.finish("a", search([match(), match({ path: "src/other.ts", score: 1, binding: { state: "changed-local", reason: "changed" } })]), false);
		const view = new SemctlInspector(model, () => {});
		let described = nodes(view.describe(native));
		expect(described.filter(n => n.k === "meter").map(n => n.p)).toMatchObject([{ value: 1, tone: "accent" }, { value: 0.5, tone: "info" }]);
		expect(described.find(n => n.k === "meter")?.p?.title).toContain("raw hybrid score 2");
		const score = described.find(n => n.k === "meter");
		expect(score?.p?.title).toContain("Relative match score");
		expect(score?.p?.aria).toBe(score?.p?.title);
		const visibleText = described.flatMap(n => n.k === "text" && n.p?.text ? [n.p.text] : []).join("\n");
		expect(visibleText).not.toContain("Strength");
		expect(visibleText).not.toContain("query-relative");
		expect(visibleText).toContain("2 matches");
		expect(described.find(n => n.k === "code")?.p).toMatchObject({ start: 10, numbers: true, marks: [{ line: 10, tone: "accent" }] });
		view.handleNativeEvent({ type: "action", key: "hit-1", act: "hit:1", mods: [] });
		described = nodes(view.describe(native));
		expect(described.find(n => n.k === "code")?.p).toMatchObject({ marks: undefined });
		expect(described.filter(n => n.k === "text" && n.p?.href)).toHaveLength(1);
		view.handleNativeEvent({ type: "toggle", key: "", collapsed: true });
		expect(view.describe(native)?.p).toMatchObject({ collapsed: true });
		expect(view.render(20)).toHaveLength(2);
	});
	test("never opens canonical, relative-root, outside-root or mismatched-codebase paths", () => {
		for (const payload of [search([match({ path: "../secret.ts" })]), search([match({ codebaseId: "another" })]),
			{ ...search(), identity: { ...search().identity, copy: "canonical" as const, localRoot: null } },
			{ ...search(), identity: { ...search().identity, localRoot: "./repo" } }]) {
			const model = new InspectorModel(); model.start("a", "mcp__semctx_search_codebase", "search_codebase"); model.finish("a", payload, false);
			expect(nodes(new SemctlInspector(model, () => {}).describe(native)).some(n => n.p?.href)).toBe(false);
		}
	});
	test("status snapshots show background failure without changing successful RPC state or inventing percent", () => {
		const display = readDisplay({ version: 1, kind: "status", identity: search().identity, catalog: { files: 3, bytes: 100 }, catalogError: null,
			coordinator: { running: false, last_error: "Embedding failed", sync_progress: { done: 2 } }, jobId: "job", jobError: null,
			job: { filesEmbedded: 2, filesFailed: 1, filesToEmbed: 10, error: "Job failed", startedAt: null, completedAt: null } }, "sync_status");
		expect(display).toBeDefined();
		const model = new InspectorModel(); model.start("s", "mcp__semctx_sync_status", "sync_status"); model.finish("s", display, false);
		const view = new SemctlInspector(model, () => {}), described = nodes(view.describe(native));
		expect(described[0].p).toMatchObject({ status: "done" });
		expect(described.some(n => n.k === "progress" || n.k === "spinner" || n.k === "elapsed")).toBe(false);
		expect(JSON.stringify(described)).toContain("Embedding failed");
		expect(view.describe({ ...native, cols: 20 })).toBeNull();
		expect(view.render(20).every(row => row.length <= 20)).toBe(true);
	});
});
