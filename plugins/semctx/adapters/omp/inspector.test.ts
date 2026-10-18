import { describe, expect, test } from "bun:test";
import type { DescribeContext, NativeNode } from "@oh-my-pi/pi-tui";
import type { ToolRenderer, ToolRenderResult } from "@oh-my-pi/pi-tui/tools";
import { SemctlInspector, createInspectorRenderer, readDisplay, snippetWindow, strengths, type Display, type Hit } from "./inspector";

function match(overrides: Partial<Hit> = {}): Hit {
	return { path: "src/main.ts", symbol: "main", qualifiedName: "app.main", language: "typescript", kind: "function", score: 2,
		startLine: 10, endLine: 11, startByte: 100, endByte: 120, snippet: "function main() {\n}",
		snippetStartLine: 10, snippetEndLine: 11, snippetStartByte: 100, snippetEndByte: 120, focusLine: 10, focusByte: 100,
		codebaseId: "cb", binding: { state: "bound", reason: null }, ...overrides };
}
function search(rows: Hit[] = [match()], query = "find main"): Extract<Display, { kind: "search" }> {
	return { version: 1, kind: "search", query, identity: { codebaseId: "cb", name: null, localRoot: "D:/repo", copy: "checkout" }, hits: rows, indexWarning: null };
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
const compact = { expanded: false, isPartial: false };
const nativeFallback = { body: [{ k: "text", p: { text: "complete native output" } } as NativeNode] };
const fallback: ToolRenderer = {
	renderCall: () => undefined,
	renderResult: () => undefined,
	describeResult: () => nativeFallback,
};
function result(display: Display = search(), details: Record<string, unknown> = {}): ToolRenderResult {
	return { content: [{ type: "text", text: "complete raw result" }], details: {
		serverName: "semctx", mcpToolName: "search_codebase", mcpMeta: { semctlDisplay: display }, ...details,
	} };
}
function renderer(server = "semctx") {
	return createInspectorRenderer(server, "search_codebase")({ requestRender: () => {}, fallback });
}
function component(r: ToolRenderer, value: ToolRenderResult): SemctlInspector {
	const body = r.describeResult!(value, compact)?.body?.[0];
	if (!(body instanceof SemctlInspector)) throw new Error("Missing semctl result component");
	return body;
}

describe("bounded semctl display carrier", () => {
	test("requires version, operation, identity, full typed coordinates and the carrier bound", () => {
		expect(readDisplay(search(), "search_codebase")).toEqual(search());
		expect(readDisplay({ ...search(), query: undefined }, "search_codebase")).toBeUndefined();
		expect(readDisplay({ ...search(), query: 42 }, "search_codebase")).toBeUndefined();
		expect(readDisplay(search([], "x".repeat(32 * 1024)), "search_codebase")).toBeUndefined();
		for (const payload of [undefined, {}, { ...search(), version: 2 }, { ...search(), identity: null },
			{ ...search(), hits: [{ ...match(), binding: { state: "wrong", reason: null } }] },
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
		const payload: Display = { version: 1, kind: "batch", identity: search().identity,
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
		expect(nodes(new SemctlInspector(display!, () => {}).describe(native)).find(n => n.k === "code")?.p).toMatchObject({ start: 20, numbers: true });
		const mismatch = readDisplay({ ...source, lineEnd: 30 }, "read_source");
		expect(nodes(new SemctlInspector(mismatch!, () => {}).describe(native)).find(n => n.k === "code")?.p).toMatchObject({ numbers: false });
		expect(readDisplay({ ...source, byteStart: -1 }, "read_source")).toBeUndefined();
		expect(readDisplay({ ...source, byteEnd: 8 }, "read_source")).toBeUndefined();
	});
});

describe("per-card native renderer", () => {
	test("compact results expose index warnings and never hide an unknown warning state", () => {
		const warning = "The latest indexing job failed; results may be stale.";
		const value = result({ ...search(), indexWarning: warning });
		const view = component(renderer(), value);
		expect(nodes(view.describe(native)).find(n => n.p?.title === warning)?.p?.tone).toBe("warning");
		expect(nodes(view.describe({ ...native, supports: k => k !== "icon" })).some(n =>
			n.k === "text" && n.p?.tone === "warning" && n.p?.title === warning)).toBe(true);
		for (const indexWarning of [undefined, 42, {}]) {
			const unknown = result(search(), { mcpMeta: { semctlDisplay: { ...search(), indexWarning } } });
			expect(renderer().describeResult!(unknown, compact)).toBe(nativeFallback);
		}
	});
	test("concurrent cards retain independent query and source selection across repaint and expansion", () => {
		const older = renderer(), newer = renderer();
		const query = "  exact \"quoted\" \\\\ query\nsecond line 中";
		const first = result(search([match(), match({ path: "other.ts" })], query));
		const second = result(search([match({ path: "new.ts" })], "different query"));
		const before = JSON.stringify(first), a = component(older, first), b = component(newer, second);
		a.handleNativeEvent({ type: "action", key: "hit-1", act: "hit:1", mods: [] });
		expect(component(older, first)).toBe(a);
		expect(nodes(a.describe(native)).find(n => n.k === "code")?.p?.path).toBe("other.ts");
		expect(nodes(b.describe(native)).find(n => n.k === "code")?.p?.path).toBe("new.ts");
		expect(older.describeResult!(first, compact)?.tool?.target).toBe(query);
		expect(older.describeResult!(first, { ...compact, expanded: true })).toBe(nativeFallback);
		expect(component(older, first)).toBe(a);
		expect(JSON.stringify(first)).toBe(before);
	});
	test("invalid carriers, wrong origins, failures, cancellation and partial results retain native output", () => {
		const r = renderer();
		component(r, result());
		for (const value of [
			{ ...result(), details: undefined },
			result(search(), { serverName: "semctx-other" }),
			result(search(), { mcpToolName: "read_source" }),
			result(search(), { mcpMeta: { semctlDisplay: { version: 99 } } }),
			result(search(), { isError: true }),
			{ ...result(), isError: true },
		]) expect(r.describeResult!(value, compact)).toBe(nativeFallback);
		expect(r.describeResult!(result(), { ...compact, isPartial: true })).toBe(nativeFallback);
		expect(r.describeResult!(result(), { ...compact, cancelled: true })).toBe(nativeFallback);
		expect(renderer("semctx:semctx").describeResult!(result(search(), { serverName: "semctx-semctx" }), compact)).toBe(nativeFallback);
		expect(component(renderer("semctx:semctx"), result(search(), { serverName: "semctx:semctx" })).display).toEqual(search());
	});
	test("a different result resets selection without changing another card", () => {
		const r = renderer(), first = result(search([match(), match({ path: "second.ts" })]));
		component(r, first).handleNativeEvent({ type: "action", key: "hit-1", act: "hit:1", mods: [] });
		const replacement = component(r, result(search([match({ path: "replacement.ts" })])));
		replacement.handleNativeEvent({ type: "action", key: "hit-99", act: "hit:99", mods: [] });
		expect(nodes(replacement.describe(native)).find(n => n.k === "code")?.p?.path).toBe("replacement.ts");
	});
	test("a narrow native surface retains the original result body", () => {
		expect(component(renderer(), result()).describe({ ...native, cols: 20 })?.c).toEqual(nativeFallback.body);
	});
});

describe("native provenance presentation", () => {
	test("selection does not change retained source metadata", () => {
		const payload = search([match(), match({ path: "other.ts" })]);
		const before = JSON.stringify(payload), view = new SemctlInspector(payload, () => {});
		view.handleNativeEvent({ type: "action", key: "hit-1", act: "hit:1", mods: [] });
		view.describe(native);
		expect(JSON.stringify(payload)).toBe(before);
	});
	test("more than 32 unmatched batch symbols remain available through explicit expansion", () => {
		const payload = readDisplay({ version: 1, kind: "batch", identity: search().identity,
			symbols: Array.from({ length: 33 }, (_, i) => ({ symbol: `missing-${i}`, hits: [] })) }, "batch_lookup");
		expect(payload).toBeDefined();
		const view = new SemctlInspector(payload!, () => {});
		expect(nodes(view.describe(native)).filter(n => n.key?.startsWith("batch-miss-"))).toHaveLength(8);
		activate(view, "matches-control");
		expect(nodes(view.describe(native)).filter(n => n.key?.startsWith("batch-miss-"))).toHaveLength(33);
		expect(nodes(view.describe(native)).some(n => n.k === "text" && n.p?.text === "missing-32 · no match")).toBe(true);
	});
	test("bounds search around verified focus and read_source from its first line without changing raw results", () => {
		const lines = Array.from({ length: 40 }, (_, i) => `line ${i + 1}\n`), sourceText = lines.join("");
		const payload = search([match({ snippet: sourceText, snippetStartLine: 100, snippetEndLine: 139, focusLine: 129 })]);
		const before = JSON.stringify(payload), view = new SemctlInspector(payload, () => {});
		expect(nodes(view.describe(native)).find(n => n.k === "code")?.p).toMatchObject({
			text: lines.slice(23, 35).join("").slice(0, -1), start: 123, numbers: true, marks: [{ line: 129, tone: "accent" }],
		});
		expect(JSON.stringify(payload)).toBe(before);
		const source: Display = { version: 1, kind: "source", identity: search().identity, path: "src/main.ts", text: sourceText,
			language: "typescript", sourceOrigin: "content-plane", contentHash: "hash", encoding: "utf-8", binary: false, truncated: false,
			byteStart: 0, byteEnd: sourceText.length, fileSize: sourceText.length, lineStart: 200, lineEnd: 239 };
		const sourceBefore = JSON.stringify(source);
		expect(nodes(new SemctlInspector(source, () => {}).describe(native)).find(n => n.k === "code")?.p).toMatchObject({ text: lines.slice(0, 16).join("").slice(0, -1), start: 200 });
		expect(JSON.stringify(source)).toBe(sourceBefore);
	});
	test.each(["first\nlast\n", "first\r\nlast\r\n", "first\n\n"])("a terminating line break does not invent another numbered line: %j", snippet => {
		const code = nodes(new SemctlInspector(search([match({ snippet, snippetStartLine: 10, snippetEndLine: 11, focusLine: 10 })]), () => {}).describe(native)).find(n => n.k === "code");
		expect((code?.p?.text as string).split(/\r?\n/)).toEqual(snippet.startsWith("first\n\n") ? ["first", ""] : ["first", "last"]);
	});
	test("compact batch entries are bounded and explicit expansion retains grouped hits and misses in caller order", () => {
		const payload: Display = { version: 1, kind: "batch", identity: search().identity, symbols: [
			{ symbol: "early-miss", hits: [] }, { symbol: "many", hits: Array.from({ length: 10 }, () => match()) }, { symbol: "late-miss", hits: [] },
		] };
		const view = new SemctlInspector(payload, () => {});
		const compact = nodes(view.describe(native));
		expect(compact.filter(n => /^(hit|batch-group|batch-miss)-/.test(n.key ?? ""))).toHaveLength(8);
		expect(compact.some(n => n.k === "text" && n.p?.text === "late-miss · no match")).toBe(false);
		activate(view, "matches-control");
		const all = nodes(view.describe(native));
		expect(all.filter(n => n.key?.startsWith("hit-"))).toHaveLength(10);
		expect(all.filter(n => n.key?.startsWith("batch-")).map(n => n.k === "text" ? n.p?.text : undefined)).toEqual(["early-miss · no match", "many · 10 matches", "late-miss · no match"]);
		activate(view, "matches-control");
		expect(nodes(view.describe(native)).filter(n => /^(hit|batch-group|batch-miss)-/.test(n.key ?? ""))).toHaveLength(8);
	});
	test("search expansion retains ranked source selection without unbounding a newly selected source", () => {
		const view = new SemctlInspector(search(Array.from({ length: 12 }, (_, i) => match({ path: `src/${i}.ts` }))), () => {});
		expect(nodes(view.describe(native)).filter(n => n.key?.startsWith("hit-"))).toHaveLength(8);
		activate(view, "matches-control");
		expect(nodes(view.describe(native)).filter(n => n.key?.startsWith("hit-"))).toHaveLength(12);
		view.handleNativeEvent({ type: "action", key: "hit-10", act: "hit:10", mods: [] });
		expect(nodes(view.describe(native)).find(n => n.k === "code")?.p).toMatchObject({ path: "src/10.ts" });
		activate(view, "matches-control");
		expect(nodes(view.describe(native)).filter(n => n.key?.startsWith("hit-"))).toHaveLength(8);
		expect(nodes(view.describe(native)).find(n => n.k === "code")?.p).toMatchObject({ path: "src/10.ts" });
	});
	test("verification distinguishes byte-binding failures and keeps their evidence available", () => {
		const rows = [
			match(),
			match({ binding: { state: "changed-local", reason: "Local hash changed" } }),
			match({ binding: { state: "range-mismatch", reason: "Requested range differs" } }),
			match({ binding: { state: "unverified", reason: "Probe unavailable" } }),
			match({ binding: { state: "not-probed", reason: "Batch does not probe sources" } }),
		];
		const view = new SemctlInspector(search(rows), () => {}), described = nodes(view.describe(native));
		const icons = described.filter(n => n.k === "icon");
		expect(icons.slice(0, 5).map(n => n.p?.tone)).toEqual(["success", "warning", "error", "muted", "muted"]);
		expect(icons[1].p?.title).toContain("Local hash changed");
		expect(icons[2].p?.title).toContain("Requested range differs");
		expect(icons[3].p?.title).toContain("Probe unavailable");
		expect(icons[4].p?.title).toContain("Batch does not probe sources");
		const fallback = nodes(view.describe({ ...native, supports: kind => kind !== "icon" }));
		expect(fallback.some(n => n.k === "icon")).toBe(false);
		expect(fallback.some(n => n.k === "text" && n.p?.text === "bound")).toBe(true);
	});
	test("unavailable batch scores are static neutral dashes, while finite zero remains an empty meter", () => {
		const view = new SemctlInspector({ version: 1, kind: "batch", identity: search().identity, symbols: [
			{ symbol: "main", hits: [match({ score: null })] },
		] }, () => {});
		let described = nodes(view.describe(native));
		expect(described.some(n => n.k === "meter" || n.k === "spinner" || n.k === "progress")).toBe(false);
		const unavailable = described.find(n => n.k === "text" && n.p?.text === "—");
		expect(unavailable?.p?.tone).toBe("muted");
		described = nodes(new SemctlInspector(search([match({ score: 2 }), match({ score: 0 })]), () => {}).describe(native));
		expect(described.filter(n => n.k === "meter").map(n => n.p && "value" in n.p ? n.p.value : undefined)).toEqual([1, 0]);
	});
	test("uses native meters/tooltips and validated indexed-focus source, with supported selection", () => {
		const view = new SemctlInspector(search([match(), match({ path: "src/other.ts", score: 1, binding: { state: "changed-local", reason: "changed" } })]), () => {});
		let described = nodes(view.describe(native));
		expect(described.filter(n => n.k === "meter").map(n => n.p)).toMatchObject([{ value: 1, tone: "accent" }, { value: 0.5, tone: "info" }]);
		expect(described.find(n => n.k === "code")?.p).toMatchObject({ start: 10, numbers: true, marks: [{ line: 10, tone: "accent" }] });
		view.handleNativeEvent({ type: "action", key: "hit-1", act: "hit:1", mods: [] });
		described = nodes(view.describe(native));
		expect(described.find(n => n.k === "code")?.p).toMatchObject({ marks: undefined });
		expect(described.filter(n => n.k === "text" && n.p?.href)).toHaveLength(1);
	});
	test("never opens canonical, relative-root, outside-root or mismatched-codebase paths", () => {
		for (const payload of [search([match({ path: "../secret.ts" })]), search([match({ codebaseId: "another" })]),
			{ ...search(), identity: { ...search().identity, copy: "canonical" as const, localRoot: null } },
			{ ...search(), identity: { ...search().identity, localRoot: "./repo" } }]) {
			expect(nodes(new SemctlInspector(payload, () => {}).describe(native)).some(n => n.p?.href)).toBe(false);
		}
	});
	test("status snapshots show background failure without changing successful RPC state or inventing percent", () => {
		const display = readDisplay({ version: 1, kind: "status", identity: search().identity, catalog: { files: 3, bytes: 100 }, catalogError: null,
			coordinator: { running: false, last_error: "Embedding failed", sync_progress: { done: 2 } }, jobId: "job", jobError: null,
			job: { filesEmbedded: 2, filesFailed: 1, filesToEmbed: 10, error: "Job failed", startedAt: null, completedAt: null } }, "sync_status");
		expect(display).toBeDefined();
		const view = new SemctlInspector(display!, () => {}), described = nodes(view.describe(native));
		expect(described.some(n => n.k === "progress" || n.k === "spinner" || n.k === "elapsed")).toBe(false);
		expect(JSON.stringify(described)).toContain("Embedding failed");
	});
});
