import { describe, expect, test } from "bun:test";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
import type { DescribeContext, NativeNode } from "@oh-my-pi/pi-tui";
import type { ToolRenderer, ToolRenderResult } from "@oh-my-pi/pi-tui/tools";
import { SemctlInspector, createInspectorRenderer, identityBadge, readDisplay, snippetWindow, type Display, type Hit } from "./inspector";
import { strengths } from "./graph-data/matches.js";
import { GraphInspector } from "./graph";
import { resultGraph } from "./result-graph";
import { matchScoreIndicator } from "./match-indicators";
import { asNativeElement } from "./native-elements";

function match(overrides: Partial<Hit> = {}): Hit {
	return { path: "src/main.ts", symbol: "main", qualifiedName: "app.main", language: "typescript", kind: "function", score: 2,
		startLine: 10, endLine: 11, startByte: 100, endByte: 120, snippet: "function main() {\n}",
		snippetStartLine: 10, snippetEndLine: 11, snippetStartByte: 100, snippetEndByte: 120, focusLine: 10, focusByte: 100,
		codebaseId: "cb", binding: { state: "bound", reason: null }, ...overrides };
}
function search(rows: Hit[] = [match()], query = "find main"): Extract<Display, { kind: "search" }> {
	return { version: 1, kind: "search", query, identity: { codebaseId: "cb", name: null, localRoot: resolve("/repo"), copy: "checkout" }, hits: rows, indexWarning: null };
}
const native: DescribeContext = { cols: 100, dark: true, reduceMotion: false, supports: () => true, feature: () => false };
function nodes(root: NativeNode | null): NativeNode[] {
	if (!root) return [];
	const result = [root];
	if (root.k === "section" && root.p?.collapsed) return result;
	for (const c of root.c ?? []) if ("k" in c) result.push(...nodes(c));
	return result;
}
function textContent(node: NativeNode): string {
	if (node.k === "kv") return node.p?.items.map(item => [item.k, item.v].map(value =>
		typeof value === "string" ? value : value.map(span => span.t).join("")).join(": ")).join("\n") ?? "";
	return node.k === "text" ? node.p?.text ?? node.p?.spans?.map(span => span.t).join("") ?? "" : "";
}
function activate(view: SemctlInspector, prefix: string): void {
	const control = nodes(view.describe(native)).find(n => n.key?.startsWith(prefix));
	if (!control?.key) throw new Error(`Missing native control: ${prefix}`);
	if (control.k === "section") {
		view.handleNativeEvent({ type: "toggle", key: `tool/body/${control.key}`, collapsed: !control.p?.collapsed });
	} else {
		if (typeof control.p?.actions?.click !== "string") throw new Error(`Missing native action: ${prefix}`);
		view.handleNativeEvent({ type: "action", key: control.key, act: control.p.actions.click, mods: [] });
	}
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
function component(r: ToolRenderer, value: ToolRenderResult, options = compact): SemctlInspector {
	const body = r.describeResult!(value, options)?.body?.[0];
	if (!(body instanceof SemctlInspector)) throw new Error("Missing semctl result component");
	return body;
}
function hrefs(root: NativeNode | null): string[] {
	return nodes(root).flatMap(node => [
		...(node.p?.href ? [node.p.href] : []),
		...(node.k === "text" ? node.p?.spans?.flatMap(span => span.href ? [span.href] : []) ?? [] : []),
	]);
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
		expect(snippetWindow(match())).toMatchObject({ start: 10, end: 11, focus: 10 });
		expect(snippetWindow(match({ snippetStartLine: 0 }))).toBeUndefined();
		expect(snippetWindow(match({ snippetEndLine: null }))).toBeUndefined();
		expect(snippetWindow(match({ snippet: "one line" }))).toBeUndefined();
		expect(snippetWindow(match({ focusLine: 999 }))?.focus).toBeUndefined();
		expect(snippetWindow(match({ focusByte: 999 }))?.focus).toBeUndefined();
	});
	test("normalizes nonnegative sets by ratio and signed sets by range, preserving ties and raw scores", () => {
		const values = (scores: (number | null)[]) => strengths(scores.map(score => match({ score })));
		expect(values([4, 2, 0])).toEqual([1, 0.5, 0]);
		expect(values([2, 2])).toEqual([1, 1]);
		expect(values([0, 0])).toEqual([null, null]);
		expect(values([4, -1])).toEqual([1, 0.1]);
		expect(values([-2, -1])).toEqual([0.1, 1]);
		expect(values([-1, -1])).toEqual([null, null]);
		for (const invalid of [[4, null], [-4, null], [4, Number.POSITIVE_INFINITY], [-4, Number.NEGATIVE_INFINITY]]) expect(values(invalid)).toEqual([null, null]);
	});
	test("retains batch misses and applies the total-hit bound across symbols", () => {
		const payload: Display = { version: 1, kind: "batch", identity: search().identity,
			symbols: [{ symbol: "missing", hits: [] }, { symbol: "main", hits: [match({ binding: { state: "not-probed", reason: null } })] }] };
		expect(readDisplay(payload, "batch_lookup")).toEqual(payload);
		expect(readDisplay({ ...payload, symbols: [{ symbol: "a", hits: Array.from({ length: 20 }, () => match()) }, { symbol: "b", hits: Array.from({ length: 13 }, () => match()) }] }, "batch_lookup")).toBeUndefined();
		const misses = { ...payload, symbols: Array.from({ length: 33 }, (_, i) => ({ symbol: `missing-${i}`, hits: [] })) };
		expect(readDisplay(misses, "batch_lookup")).toEqual(misses);
	});
	test("lookup provenance is optional for legacy carriers and rejects malformed access metadata", () => {
		const payload: Display = { version: 1, kind: "batch", identity: search().identity,
			symbols: [{ symbol: "main", hits: [match({ score: null })] }] };
		for (const operation of ["find_definition", "find_references", "who_calls", "implementations_of", "search_symbols", "expand_chunk"]) {
			expect(readDisplay(payload, operation)).toEqual(payload);
		}
		for (const invalid of [{ isWrite: "false" }, { enclosingSymbol: 7 }, { project: {} }]) {
			expect(readDisplay({ ...payload, symbols: [{ symbol: "main", hits: [{ ...match(), ...invalid }] }] }, "find_references")).toBeUndefined();
		}
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

describe("returned match graph", () => {
	test("requires distinct supplied locations and does not manufacture a graph for repeated hits", () => {
		for (const hits of [[], [match()], [match(), match()], [match({ path: null }), match({ path: null, symbol: "other" })]]) {
			expect(resultGraph(search(hits))).toBeUndefined();
		}
		expect(resultGraph(search([match(), match({ startLine: 20, startByte: 200 })]))?.edges).toHaveLength(2);
	});
	test("keeps identical paths from different codebases separate and connects only file membership", () => {
		const hits = [match(), match({ startLine: 20, startByte: 200 }), match({ codebaseId: "other" }), match({ path: null })];
		const graph = resultGraph({ version: 1, identity: search().identity, kind: "batch", symbols: [{ symbol: "main", hits }] })!;
		expect(graph.roots).toHaveLength(3);
		expect(graph.edges).toHaveLength(3);
		expect(graph.edges[0].from).toBe(graph.edges[1].from);
		expect(graph.edges[2].from).not.toBe(graph.edges[0].from);
		for (const edge of graph.edges) {
			expect(edge.kind).toBe("containment");
			expect(graph.nodes.find(node => node.id === edge.from)?.hit).toBeNull();
			expect(graph.nodes.find(node => node.id === edge.to)?.hit?.path).toBe("src/main.ts");
		}
		const unlocated = graph.nodes.find(node => node.hit?.path === null)!;
		expect(graph.roots).toContain(unlocated.id);
		expect(graph.edges.some(edge => edge.from === unlocated.id || edge.to === unlocated.id)).toBe(false);
	});
	test("the full match bound fits the graph bound without omitting locations or endpoints", () => {
		const graph = resultGraph(search(Array.from({ length: 32 }, (_, i) => match({ path: `src/${i}.ts` }))))!;
		expect(graph.nodes).toHaveLength(64);
		expect(graph.edges).toHaveLength(32);
		const ids = new Set(graph.nodes.map(node => node.id));
		expect(ids.size).toBe(64);
		for (const edge of graph.edges) {
			expect(ids.has(edge.from)).toBe(true);
			expect(ids.has(edge.to)).toBe(true);
		}
	});
});

describe("per-card native renderer", () => {
	test("matches start folded and expand independently without fetching or replacing another preview", () => {
		const payload = search([match(), match({ path: "other.ts" })]);
		const before = JSON.stringify(payload), view = new SemctlInspector(payload, () => {});
		expect(nodes(view.describe(native)).some(n => n.k === "code")).toBe(false);
		activate(view, "hit-1");
		activate(view, "hit-0");
		expect(nodes(view.describe(native)).filter(n => n.k === "code").map(n => n.p?.path)).toEqual(["src/main.ts", "other.ts"]);
		activate(view, "hit-0");
		expect(nodes(view.describe(native)).filter(n => n.k === "code").map(n => n.p?.path)).toEqual(["other.ts"]);
		view.handleNativeEvent({ type: "toggle", key: "match-1/preview/match-details-1", collapsed: false });
		const collapsed = () => {
			const section = nodes(view.describe(native)).find(n => n.key === "match-details-1");
			return section?.k === "section" ? section.p?.collapsed : undefined;
		};
		expect(collapsed()).toBe(false);
		activate(view, "hit-1");
		activate(view, "hit-1");
		expect(collapsed()).toBe(false);
		expect(JSON.stringify(payload)).toBe(before);
	});
	test("score displays distinguish absent values from unknown relative strength", () => {
		const rows = [match({ score: null }), match({ score: -1 }), match({ score: 0 })];
		for (const payload of [
			search(rows),
			{ version: 1, kind: "batch", identity: search().identity, symbols: [{ symbol: "main", hits: rows }] } as Display,
		]) {
			const view = new SemctlInspector(payload, () => {});
			const cards = nodes(view.describe(native)).filter(n => n.key?.startsWith("hit-"));
			expect(nodes(cards[0]).some(n => n.k === "meter")).toBe(false);
			for (const index of [1, 2]) {
				const meter = nodes(cards[index]).find(n => n.k === "meter");
				expect(meter?.p?.value).toBeNull();
				expect(meter?.p?.title).toContain(String(rows[index].score));
			}
		}
	});
	test("signed score sets render filled rank-ordered rails with range tooltips in native and text rows", () => {
		const rows = [match({ score: -1.59 }), match({ score: -6.14 }), match({ score: -3.865 })];
		for (const supportsMeter of [true, false]) {
			const cx = { ...native, supports: (kind: string) => kind !== "meter" || supportsMeter };
			const view = new SemctlInspector(search(rows), () => {});
			const indicators = nodes(view.describe(cx)).filter(node => node.key?.startsWith("hit-"))
				.map(card => nodes(card).find(node => node.key === "relevance")!);
			if (supportsMeter) {
				expect(indicators.slice(0, 2).map(node => node.p?.value)).toEqual([1, 0.1]);
				expect(indicators[2].p?.value).toBeCloseTo(0.55, 12);
				for (const node of indicators) expect(node.p?.tone).toBe("accent");
			} else expect(indicators.map(node => node.p?.text)).toEqual(["▰▰▰▰▰▰", "▰▱▱▱▱▱", "▰▰▰▱▱▱"]);
			indicators.forEach((node, index) => {
				expect(node.p?.title).toContain(`Score ${rows[index].score}`);
				expect(node.p?.title).toContain("Bar: position between lowest and highest result scores");
				expect(node.p?.title).toContain("not confidence");
			});
		}
		const ratio = [match({ score: 4 }), match({ score: 1 })];
		const titles = nodes(new SemctlInspector(search(ratio), () => {}).describe(native))
			.filter(node => node.key === "relevance").map(node => node.p?.title);
		expect(titles).toHaveLength(2);
		for (const title of titles) expect(title).toContain("Bar: relative to highest result score");
	});
	test("unscored matches allocate no score child in native or text-only rows, while zero keeps its indicator", () => {
		for (const supportsMeter of [true, false]) {
			const cx = { ...native, supports: (kind: string) => kind !== "meter" || supportsMeter };
			const absent = match({ score: undefined } as unknown as Partial<Hit>);
			const view = new SemctlInspector(search([match({ score: null }), absent, match({ score: 0 })]), () => {});
			const cards = nodes(view.describe(cx)).filter(node => node.key?.startsWith("hit-"));
			expect(cards).toHaveLength(3);
			for (const card of cards.slice(0, 2)) {
				expect(nodes(card).some(node => node.key === "relevance")).toBe(false);
				expect(card.c).toHaveLength((cards[2].c?.length ?? 0) - 1);
			}
			expect(nodes(cards[2]).find(node => node.key === "relevance")?.p?.title).toContain("0");
			expect(matchScoreIndicator(absent, 0, null, cx, "hit:0")).toBeUndefined();
		}
	});
	test("verification failures remain available only on accessible diagnostics, including fallback icons", () => {
		const reason = "The local file hash differs from the indexed source.";
		for (const unsupported of [[], ["icon"], ["icon", "image"]]) {
			const cx: DescribeContext = { ...native, supports: kind => !unsupported.includes(kind) };
			const view = new SemctlInspector(search([match({ binding: { state: "changed-local", reason } })]), () => {});
			activate(view, "hit-0");
			activate(view, "match-details-0");
			const described = nodes(view.describe(cx)), diagnostic = described.find(node => node.key === "verification");
			expect(diagnostic?.p?.title).toContain(reason);
			expect(diagnostic?.p?.aria).toContain(reason);
			expect(described.map(textContent).join("\n")).not.toContain(reason);
			expect(described.map(textContent).join("\n")).not.toContain("Local changes");
			expect(described.filter(node => node.key !== "verification").every(node => !node.p?.title?.includes(reason))).toBe(true);
		}
	});
	test("freshness warnings move to a header badge without repeating warning prose in the body", () => {
		const warning = "The latest indexing job failed; results may be stale.";
		const value = result({ ...search(), indexWarning: warning }), r = renderer();
		const view = component(r, value);
		expect(r.describeResult!(value, compact)?.tool?.badges).toEqual([identityBadge(search().identity), { text: "index warning", tone: "warning", title: warning }]);
		expect(renderer().describeResult!(result(), compact)?.tool?.badges).toEqual([identityBadge(search().identity)]);
		for (const cx of [native, { ...native, supports: (kind: string) => kind !== "icon" }]) {
			const described = nodes(view.describe(cx));
			expect(described.some(node => node.key === "index-warning" || node.p?.role === "semctl.index-warning")).toBe(false);
			expect(described.map(textContent).join("\n")).not.toContain(warning);
		}
		for (const indexWarning of [undefined, 42, {}]) {
			const unknown = result(search(), { mcpMeta: { semctlDisplay: { ...search(), indexWarning } } });
			expect(renderer().describeResult!(unknown, compact)).toBe(nativeFallback);
		}
	});
	test("the tool header carries codebase identity and bodies start with their content", () => {
		const styled: DescribeContext = { ...native, feature: name => name === "styles" };
		const r = renderer(), value = result(search([match(), match({ path: "other.ts" })]));
		expect(r.describeResult!(value, compact)?.tool?.badges).toEqual([
			{ text: "repo", tone: "muted", title: `Codebase: cb\nCheckout: ${resolve("/repo")}\nCopy: checkout` },
		]);
		expect(identityBadge({ codebaseId: null, name: "semctl", localRoot: null, copy: "canonical" }))
			.toEqual({ text: "semctl · canonical", tone: "muted", title: "Codebase: unbound\nCopy: canonical" });
		expect(identityBadge({ codebaseId: "cb", name: null, localRoot: null, copy: "canonical" }).text).toBe("Codebase · canonical");
		const source: Display = { version: 1, kind: "source", identity: search().identity, path: "src/main.ts", text: "one",
			language: "typescript", sourceOrigin: "content-plane", contentHash: "hash", encoding: "utf-8", binary: false, truncated: false,
			byteStart: 0, byteEnd: 3, fileSize: 3, lineStart: 1, lineEnd: 1 };
		const view = component(r, value);
		for (const cx of [native, styled]) {
			const root = view.describe(cx), first = root?.c?.[0];
			expect(nodes(root).some(node => node.p?.role === "semctl.identity")).toBe(false);
			expect(first && "k" in first ? first.p?.role : undefined).toBe("semctl.matches");
			const sourceRoot = new SemctlInspector(source, () => {}).describe(cx), sourceFirst = sourceRoot?.c?.[0];
			expect(nodes(sourceRoot).some(node => node.p?.role === "semctl.identity")).toBe(false);
			expect(sourceFirst && "k" in sourceFirst ? textContent(sourceFirst) : undefined).toStartWith("src/main.ts");
		}
	});
	test("header badges and match hovers stay within short tooltip bounds", () => {
		const longRoot = resolve(`/${"checkout-segment/".repeat(8)}repo`);
		const badge = identityBadge({ codebaseId: "cb".repeat(60), name: null, localRoot: longRoot, copy: "checkout" });
		expect(badge.title.split("\n")).toHaveLength(3);
		for (const line of badge.title.split("\n")) expect(line.length).toBeLessThanOrEqual(88);
		const warning = Array.from({ length: 6 }, (_, index) => `Warning line ${index}: ${"stale ".repeat(30)}`).join("\n");
		const warned = renderer().describeResult!(result({ ...search(), indexWarning: warning }), compact)?.tool?.badges?.[1];
		expect(warned?.title?.split("\n").length).toBe(4);
		for (const line of warned?.title?.split("\n") ?? []) expect(line.length).toBeLessThanOrEqual(88);
		const long = "segment::".repeat(20);
		const view = new SemctlInspector(search([match({ qualifiedName: long, enclosingSymbol: long, binding: { state: "changed-local", reason: "reason ".repeat(40) } })]), () => {});
		for (const node of nodes(view.describe(native))) if (node.p?.title) {
			expect(node.p.title.split("\n").length).toBeLessThanOrEqual(3);
			for (const line of node.p.title.split("\n")) expect(line.length).toBeLessThanOrEqual(88);
		}
	});
	test("the result graph toggle is the last item of the matches flow and opens the graph below it", () => {
		const styled: DescribeContext = { ...native, feature: name => name === "styles" };
		const view = new SemctlInspector(search([match(), match({ path: "other.ts" })]), () => {});
		const control = (cx: DescribeContext) => {
			const last = nodes(view.describe(cx)).find(node => node.p?.role === "semctl.matches")?.c?.at(-1);
			return last && "k" in last ? last : undefined;
		};
		for (const cx of [native, styled, { ...native, supports: (kind: string) => kind !== "icon" }]) {
			expect(control(cx)?.key).toBe("graph-control");
			expect(control(cx)?.p).toMatchObject({ role: "semctl.match.graph", actions: { click: "graph" }, aria: "Show result graph" });
		}
		expect(asNativeElement(control(styled)!)?.p.attrs).toMatchObject({ "aria-pressed": false, "aria-expanded": false });
		activate(view, "graph-control");
		expect(asNativeElement(control(styled)!)?.p.attrs).toMatchObject({ "aria-pressed": true, "aria-expanded": true });
		expect(view.describe(styled)?.c?.map(child => "k" in child ? child.key : undefined)).toEqual(["matches", "result-graph"]);
		expect(new SemctlInspector(search(), () => {}).describe(native)?.c?.some(child => "k" in child && nodes(child).some(node => node.key === "graph-control"))).toBe(false);
	});
	test("concurrent cards retain independent query and open matches across repaint and full-result expansion", () => {
		const older = renderer(), newer = older;
		const query = "  exact \"quoted\" \\\\ query\nsecond line 中";
		const first = result(search([match(), match({ path: "other.ts" })], query));
		const second = result(search([match({ path: "new.ts" })], "different query"));
		const before = JSON.stringify(first), a = component(older, first), b = component(newer, second);
		a.handleNativeEvent({ type: "action", key: "hit-1", act: "hit:1", mods: [] });
		activate(b, "hit-0");
		expect(component(older, first)).toBe(a);
		expect(nodes(a.describe(native)).find(n => n.k === "code")?.p?.path).toBe("other.ts");
		expect(nodes(b.describe(native)).find(n => n.k === "code")?.p?.path).toBe("new.ts");
		expect(older.describeResult!(first, compact)?.tool?.target).toBe(query);
		expect(component(older, first, { ...compact, expanded: true })).toBe(a);
		expect(nodes(a.describe(native)).find(n => n.k === "code")?.p?.path).toBe("other.ts");
		expect(component(older, first)).toBe(a);
		expect(JSON.stringify(first)).toBe(before);
	});
	test("compact previews retain rank and restore late source selections when expanded", () => {
		const r = renderer();
		const value = result(search(Array.from({ length: 32 }, (_, i) => match({ path: `src/${i}.ts`, score: 32 - i }))));
		const view = component(r, value);
		const visible = () => nodes(view.describe(native)).filter(node => /^match-\d+$/.test(node.key ?? "") && !node.p?.hidden);
		const initial = visible().map(node => node.key);
		expect(initial.length).toBeLessThan(32);
		expect(initial).toEqual(Array.from({ length: initial.length }, (_, i) => `match-${i}`));
		expect(visible().flatMap(nodes).filter(node => node.k === "meter").map(node => node.p?.value)).toEqual(initial.map((_, i) => (32 - i) / 32));
		component(r, value, { ...compact, expanded: true });
		activate(view, "hit-31");
		expect(visible().map(node => node.key)).toEqual(Array.from({ length: 32 }, (_, i) => `match-${i}`));
		expect(visible().flatMap(nodes).find(node => node.k === "code")?.p?.path).toBe("src/31.ts");
		component(r, value);
		expect(visible().map(node => node.key)).toEqual(initial);
		expect(visible().flatMap(nodes).some(node => node.k === "code")).toBe(false);
		component(r, value, { ...compact, expanded: true });
		expect(visible().flatMap(nodes).find(node => node.k === "code")?.p?.path).toBe("src/31.ts");
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
	test("a different result resets open matches without changing another card", () => {
		const r = renderer(), first = result(search([match(), match({ path: "second.ts" })]));
		component(r, first).handleNativeEvent({ type: "action", key: "hit-1", act: "hit:1", mods: [] });
		const replacement = component(r, result(search([match({ path: "replacement.ts" })])));
		replacement.handleNativeEvent({ type: "action", key: "hit-99", act: "hit:99", mods: [] });
		expect(nodes(replacement.describe(native)).some(n => n.k === "code")).toBe(false);
		activate(replacement, "hit-0");
		expect(nodes(replacement.describe(native)).find(n => n.k === "code")?.p?.path).toBe("replacement.ts");
	});
	test("graph-first cards show the graph directly and preserve selection across full-result expansion", () => {
		const display: Display = { version: 1, identity: search().identity, kind: "graph", title: "Call graph", query: "main",
			nodes: [{ id: "main", label: "main", detail: null, hit: match() }, { id: "worker", label: "worker", detail: null, hit: null }],
			edges: [{ from: "main", to: "worker", label: "calls" }, { from: "worker", to: "main", label: "calls" }],
			roots: ["main"], caveats: [], truncated: false };
		const value = result(display, { mcpToolName: "call_graph" });
		const first = createInspectorRenderer("semctx", "call_graph")({ requestRender: () => {}, fallback });
		const a = component(first, value), b = component(first, result(display, { mcpToolName: "call_graph" }));
		const body = (view: SemctlInspector) => nodes(view.describe(native)).flatMap(node => node.c ?? []).find(child => child instanceof GraphInspector);
		const graph = body(a);
		if (!(graph instanceof GraphInspector)) throw new Error("Missing graph-first preview");
		graph.handleNativeEvent({ type: "action", key: "", act: "node:0", mods: [] });
		graph.handleNativeEvent({ type: "action", key: "", act: "edge:0", mods: [] });
		expect(nodes(graph.describe(native)).map(textContent).join("\n")).toContain("main → worker");
		const other = body(b);
		if (!(other instanceof GraphInspector)) throw new Error("Missing independent graph");
		expect(nodes(other.describe(native)).some(node => node.key === "selection")).toBe(false);
		expect(component(first, value, { ...compact, expanded: true })).toBe(a);
		expect(component(first, value)).toBe(a);
		expect(body(a)).toBe(graph);
		expect(nodes(graph.describe(native)).map(textContent).join("\n")).toContain("main → worker");
	});
	test("the result Graph control retains graph selection and independent match expansion", () => {
		const view = new SemctlInspector(search([match(), match({ path: "other.ts" })]), () => {});
		const host = { ...native, cols: 40, supports: (kind: string) => kind !== "section" };
		const body = () => nodes(view.describe(host)).flatMap(node => node.c ?? []).find(child => child instanceof GraphInspector);
		activate(view, "hit-0");
		expect(body()).toBeUndefined();
		activate(view, "graph-control");
		const graph = body();
		if (!(graph instanceof GraphInspector)) throw new Error("Missing result graph");
		graph.handleNativeEvent({ type: "action", key: "", act: "node:0", mods: [] });
		graph.handleNativeEvent({ type: "action", key: "", act: "edge:0", mods: [] });
		expect(nodes(graph.describe(native)).some(node => node.key === "selection")).toBe(true);
		activate(view, "graph-control");
		expect(body()).toBeUndefined();
		expect(nodes(view.describe(host)).find(node => node.k === "code")?.p?.path).toBe("src/main.ts");
		activate(view, "graph-control");
		expect(body()).toBe(graph);
		expect(nodes(graph.describe(native)).some(node => node.key === "selection")).toBe(true);
	});
	test("a graph source preview keeps the match's original rank and relative relevance", () => {
		const view = new SemctlInspector(search([match({ score: 9 }), match({ path: "other.ts", score: 3 })]), () => {});
		activate(view, "graph-control");
		const graph = nodes(view.describe(native)).flatMap(node => node.c ?? []).find(child => child instanceof GraphInspector);
		if (!(graph instanceof GraphInspector)) throw new Error("Missing result graph");
		graph.handleNativeEvent({ type: "action", key: "", act: "node:1", mods: [] });
		graph.handleNativeEvent({ type: "action", key: "", act: "edge:1", mods: [] });
		const preview = nodes(graph.describe(native)).flatMap(node => node.c ?? []).find(child => child instanceof SemctlInspector);
		if (!(preview instanceof SemctlInspector)) throw new Error("Missing selected match preview");
		const meter = nodes(preview.describe(native)).find(node => node.k === "meter");
		expect(meter?.p?.value).toBeCloseTo(1 / 3);
		expect(meter?.p?.title).toContain("Rank 2");
	});
	test("graph hits retain scores across context-only nodes, root ordering and selected source previews", () => {
		const data: Display = { version: 1, identity: search().identity, kind: "graph", title: "Graph", query: null,
			nodes: [{ id: "context", label: "Context", detail: null, hit: null },
				{ id: "high", label: "High", detail: null, hit: match({ score: 8 }) },
				{ id: "low", label: "Low", detail: null, hit: match({ path: "other.ts", score: 2 }) }],
			edges: [{ from: "context", to: "low", label: "calls" }], roots: ["low"], caveats: [], truncated: false };
		const view = new SemctlInspector(data, () => {});
		const graph = nodes(view.describe(native)).flatMap(node => node.c ?? []).find(child => child instanceof GraphInspector);
		if (!(graph instanceof GraphInspector)) throw new Error("No graph component");
		const quick = nodes(graph.describe(native)).find(node => node.key === "quick-0");
		const meter = nodes(quick ?? null).find(node => node.k === "meter");
		expect(meter?.p?.value).toBe(0.25);
		expect(meter?.p?.title).toContain("Rank 2");
		graph.handleNativeEvent({ type: "action", key: "", act: "node:0", mods: [] });
		const preview = nodes(graph.describe(native)).flatMap(node => node.c ?? []).find(child => child instanceof SemctlInspector);
		if (!(preview instanceof SemctlInspector)) throw new Error("No selected source preview");
		const selected = nodes(preview.describe(native)).find(node => node.k === "meter");
		expect(selected?.p?.value).toBe(0.25);
		expect(selected?.p?.title).toContain("Rank 2");
	});
	test("narrow graphical hosts retain native matches and score bars", () => {
		const described = nodes(component(renderer(), result()).describe({ ...native, cols: 20 }));
		expect(described.some(node => node.key === "hit-0")).toBe(true);
		expect(described.some(node => node.k === "meter")).toBe(true);
		expect(described.some(node => textContent(node) === "complete native output")).toBe(false);
	});
	test("styled matches expose data-motion from reduced motion and keep match keys across expansion", () => {
		const styled: DescribeContext = { ...native, feature: name => name === "styles" };
		const view = new SemctlInspector(search([match(), match({ path: "other.ts" })]), () => {});
		const container = (cx: DescribeContext) => nodes(view.describe(cx)).find(node => node.p?.role === "semctl.matches");
		const before = container(styled)?.c?.map(child => "k" in child ? child.key : undefined);
		expect(asNativeElement(container(styled)!)?.p.attrs?.["data-motion"]).toBe("full");
		expect(asNativeElement(container({ ...styled, reduceMotion: true })!)?.p.attrs?.["data-motion"]).toBe("reduced");
		expect(container(native)?.k).toBe("row");
		view.handleNativeEvent({ type: "action", key: "hit-0", act: "hit:0", mods: [] });
		expect(container(styled)?.c?.map(child => "k" in child ? child.key : undefined)).toEqual(before);
		expect(nodes(view.describe(styled)).some(node => node.p?.role === "semctl.identity")).toBe(false);
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
	test("whole-result expansion retains every batch miss beyond the hit bound", () => {
		const payload = readDisplay({ version: 1, kind: "batch", identity: search().identity,
			symbols: Array.from({ length: 33 }, (_, i) => ({ symbol: `missing-${i}`, hits: [] })) }, "batch_lookup");
		expect(payload).toBeDefined();
		const r = createInspectorRenderer("semctx", "batch_lookup")({ requestRender: () => {}, fallback });
		const value = result(payload!, { mcpToolName: "batch_lookup" }), view = component(r, value);
		const expected = payload!.kind === "batch" ? payload!.symbols.map(group => group.symbol) : [];
		for (const expanded of [true, false, true]) {
			expect(component(r, value, { ...compact, expanded })).toBe(view);
			expect(nodes(view.describe(native)).filter(node => node.key?.startsWith("batch-miss-")).map(textContent)).toEqual(
				expected.map(symbol => expect.stringContaining(symbol)));
		}
	});
	test("bounds search around verified focus and read_source from its first line without changing raw results", () => {
		const lines = Array.from({ length: 40 }, (_, i) => `line ${i + 1}\n`), sourceText = lines.join("");
		const payload = search([match({ snippet: sourceText, snippetStartLine: 100, snippetEndLine: 139, focusLine: 129 })]);
		const before = JSON.stringify(payload), view = new SemctlInspector(payload, () => {});
		activate(view, "hit-0");
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
		const view = new SemctlInspector(search([match({ snippet, snippetStartLine: 10, snippetEndLine: 11, focusLine: 10 })]), () => {});
		activate(view, "hit-0");
		const code = nodes(view.describe(native)).find(n => n.k === "code");
		expect((code?.p?.text as string).split(/\r?\n/)).toEqual(snippet.startsWith("first\n\n") ? ["first", ""] : ["first", "last"]);
	});
	test("native whole-result expansion retains batch grouping, caller order and each match preview", () => {
		const payload: Display = { version: 1, kind: "batch", identity: search().identity, symbols: [
			{ symbol: "early-miss", hits: [] }, { symbol: "many", hits: Array.from({ length: 10 }, (_, index) => match({ path: `src/${index}.ts` })) },
			{ symbol: "late-miss", hits: [] },
		] };
		const r = createInspectorRenderer("semctx", "batch_lookup")({ requestRender: () => {}, fallback });
		const value = result(payload, { mcpToolName: "batch_lookup" }), view = component(r, value);
		activate(view, "hit-9");
		for (const expanded of [true, false, true]) {
			expect(component(r, value, { ...compact, expanded })).toBe(view);
			const all = nodes(view.describe(native));
			expect(all.filter(node => node.key?.startsWith("hit-"))).toHaveLength(10);
			expect(all.filter(node => node.key?.startsWith("batch-")).map(textContent)).toEqual(
				payload.symbols.map(group => expect.stringContaining(group.symbol)));
			expect(all.find(node => node.k === "code")?.p?.path).toBe("src/9.ts");
		}
	});
	test("each batch heading stays with its first match across native result expansion", () => {
		const payload: Display = { version: 1, kind: "batch", identity: search().identity, symbols: [
			{ symbol: "first", hits: [match()] }, { symbol: "second", hits: [match({ path: "second.ts" })] },
		] };
		const r = createInspectorRenderer("semctx", "batch_lookup")({ requestRender: () => {}, fallback });
		const value = result(payload, { mcpToolName: "batch_lookup" });
		for (const expanded of [false, true, false]) {
			const view = component(r, value, { ...compact, expanded });
			expect(nodes(view.describe(native)).filter(node => /^(batch-group|hit)-/.test(node.key ?? "")).map(node => node.key))
				.toEqual(["batch-group-0", "hit-0", "batch-group-1", "hit-1"]);
		}
	});
	test("reference chips retain enclosing context and exact semantic provenance without invented scores", () => {
		const reference = match({ score: null, enclosingSymbol: "app.update", isWrite: true,
			referenceNamespace: "value", referenceKind: "assignment", externalTarget: "external.main",
			binding: { state: "not-probed", reason: null } });
		const payload: Display = { version: 1, kind: "batch", identity: search().identity,
			symbols: [{ symbol: "main", hits: [reference] }] };
		const r = createInspectorRenderer("semctx", "find_references")({ requestRender: () => {}, fallback });
		const response = result(payload, { mcpToolName: "find_references" });
		const view = component(r, response);
		expect(r.describeResult!(response, compact)?.tool?.target).toBe("main");
		expect(nodes(view.describe(native)).some(n => n.k === "meter")).toBe(false);
		activate(view, "hit-0");
		activate(view, "match-details-0");
		const details = nodes(view.describe(native)).map(textContent).join("\n");
		for (const fact of ["write", "value", "assignment", "external.main", "app.main", "app.update"]) expect(details).toContain(fact);
		expect(new Set(hrefs(view.describe(native))).size).toBe(1);
		expect(component(r, response, { ...compact, expanded: true })).toBe(view);
	});
	test("top-level tool expansion preserves independently opened late matches", () => {
		const r = renderer(), value = result(search(Array.from({ length: 12 }, (_, i) => match({ path: `src/${i}.ts` }))));
		const view = component(r, value, { ...compact, expanded: true });
		activate(view, "hit-10");
		activate(view, "hit-1");
		const expected = Array.from({ length: 12 }, (_, i) => `hit-${i}`);
		for (const expanded of [false, true, false]) {
			expect(component(r, value, { ...compact, expanded })).toBe(view);
			expect(nodes(view.describe(native)).filter(node => node.key?.startsWith("hit-")).map(node => node.key)).toEqual(expected);
			expect(nodes(view.describe(native)).filter(node => node.k === "code").map(node => node.p?.path)).toEqual(["src/1.ts", "src/10.ts"]);
		}
		activate(view, "hit-1");
		expect(nodes(view.describe(native)).filter(node => node.k === "code").map(node => node.p?.path)).toEqual(["src/10.ts"]);
	});
	test("verification distinguishes byte-binding failures and keeps their evidence available", () => {
		const rows = [
			match(),
			match({ binding: { state: "changed-local", reason: "Local hash changed" } }),
			match({ binding: { state: "range-mismatch", reason: "Requested range differs" } }),
			match({ binding: { state: "unverified", reason: "Probe unavailable" } }),
			match({ binding: { state: "not-probed", reason: "Batch does not probe sources" } }),
		];
		const view = new SemctlInspector(search(rows), () => {});
		const described = nodes(view.describe(native));
		const icons = described.filter(node => node.key === "verification");
		expect(icons.map(node => node.p?.tone)).toEqual(["success", "warning", "error", "muted", "muted"]);
		expect(icons[1].p?.title).toContain("Local hash changed");
		expect(icons[2].p?.title).toContain("Requested range differs");
		expect(icons[3].p?.title).toContain("Probe unavailable");
		expect(icons[4].p?.title).toContain("Batch does not probe sources");
	});
	test("indexed preview focus does not turn changed checkout navigation into a trusted source jump", () => {
		const view = new SemctlInspector(search([match(), match({ path: "src/other.ts", score: 1, binding: { state: "changed-local", reason: "changed" } })]), () => {});
		activate(view, "hit-0");
		activate(view, "hit-1");
		const described = nodes(view.describe(native));
		for (const path of ["src/main.ts", "src/other.ts"]) {
			expect(described.find(n => n.k === "code" && n.p?.path === path)?.p).toMatchObject({
				start: 10, numbers: true, marks: [{ line: 10, tone: "accent" }],
			});
		}
		const links = hrefs(view.describe(native));
		expect(links).toHaveLength(2);
		for (const href of links) expect(new URL(href).hash).toBe("");
		expect(described.find(n => n.key === "verification" && n.p?.tone === "warning")?.p?.title).toContain("changed");
	});
	test("never opens canonical, relative-root, outside-root or mismatched-codebase paths", () => {
		for (const payload of [search([match({ path: "../secret.ts" })]), search([match({ codebaseId: "another" })]),
			search([match({ codebaseId: null })]),
			{ ...search([match({ codebaseId: null })]), identity: { ...search().identity, codebaseId: null } },
			{ ...search(), identity: { ...search().identity, codebaseId: null } },
			{ ...search(), identity: { ...search().identity, copy: "canonical" as const, localRoot: null } },
			{ ...search(), identity: { ...search().identity, localRoot: null } },
			{ ...search(), identity: { ...search().identity, localRoot: "./repo" } }]) {
			const view = new SemctlInspector(payload, () => {});
			activate(view, "hit-0");
			expect(hrefs(view.describe(native))).toEqual([]);
		}
	});
	test("collapsed match paths are safe file links without coupling navigation to source expansion", () => {
		const view = new SemctlInspector(search([match({ path: "src/a #?.ts", score: null }), match({
			path: "src/changed.ts", binding: { state: "changed-local", reason: "Local bytes changed" },
		})]), () => {});
		const collapsed = view.describe(native), links = hrefs(collapsed);
		expect(links).toHaveLength(2);
		const target = new URL(links[0]);
		expect(target.protocol).toBe("file:");
		expect(decodeURIComponent(target.pathname)).toEndWith("/repo/src/a #?.ts");
		expect(target.hash).toBe("");
		expect(target.search).toBe("");
		expect(nodes(collapsed).some(node => node.k === "code")).toBe(false);
		activate(view, "hit-0");
		expect(nodes(view.describe(native)).find(node => node.k === "code")?.p?.path).toBe("src/a #?.ts");
		expect(new Set(hrefs(view.describe(native)))).toEqual(new Set(links));
		activate(view, "hit-0");
		expect(hrefs(view.describe(native))).toEqual(links);
	});
	test("match chips retain complete returned paths and line ranges without changing file targets", () => {
		for (const paths of [
			["src/left/shared/main.ts", "src/right/shared/main.ts", "src/unique.ts", "src/left/shared/main.ts"],
			["src\\left\\shared\\main.ts", "src\\right\\shared\\main.ts", "src\\unique.ts", "src\\left\\shared\\main.ts"],
		]) {
			const payload = search(paths.map(path => match({ path })));
			const view = new SemctlInspector(payload, () => {});
			const described = view.describe(native);
			const filenames = nodes(described).filter(node => node.p?.role === "semctl.match.path");
			expect(filenames.map(textContent)).toEqual(paths.map(path => `${path}:10–11`));
			expect(hrefs(described)).toHaveLength(paths.length);
			for (const [index, href] of hrefs(described).entries()) {
				expect(fileURLToPath(href)).toBe(resolve(payload.identity.localRoot!, paths[index]));
			}
		}
	});
	test("no chip or expanded preview links unsafe paths or foreign codebases", () => {
		for (const hit of [
			match({ path: "../private.ts" }), match({ path: "/private.ts" }), match({ path: "https://example.invalid/a.ts" }),
			match({ path: "file:///private.ts" }), match({ path: "C:\\private.ts" }), match({ codebaseId: "foreign" }),
			match({ path: "" }), match({ path: "." }),
		]) {
			const view = new SemctlInspector(search([hit]), () => {});
			expect(hrefs(view.describe(native))).toEqual([]);
			activate(view, "hit-0");
			expect(hrefs(view.describe(native))).toEqual([]);
		}
	});
	test("unprobed checkout links stay file-only while returned snippets retain reported match focus", () => {
		for (const state of ["not-probed", "changed-local", "range-mismatch", "unverified"] as const) {
			const hit = match({ score: null, binding: { state, reason: `Diagnostic for ${state}` } });
			const view = new SemctlInspector(search([hit]), () => {});
			const collapsed = view.describe(native), links = hrefs(collapsed);
			expect(links).toHaveLength(1);
			const url = new URL(links[0]);
			expect(url.protocol).toBe("file:");
			expect(url.hash).toBe("");
			expect(url.search).toBe("");
			const filename = nodes(collapsed).find(node => node.k === "text" && node.p?.spans?.some(span => span.href));
			expect(filename?.p?.title).toContain("current checkout file");
			expect(filename?.p?.title).toContain("unverified");
			expect(nodes(collapsed).find(node => node.key === "verification")?.p?.title).toContain(`Diagnostic for ${state}`);
			activate(view, "hit-0");
			const expanded = view.describe(native);
			expect(new Set(hrefs(expanded))).toEqual(new Set(links));
			expect(nodes(expanded).find(node => node.k === "code")?.p?.marks).toEqual([{ line: 10, tone: "accent" }]);
		}
	});
	test("source range highlighting clips to the returned snippet and preserves context", () => {
		const snippet = "before\nfirst match\nsecond match\nafter\nlast";
		const view = new SemctlInspector(search([match({ snippet, snippetStartLine: 10, snippetEndLine: 14,
			startLine: 11, endLine: 12, focusLine: null, focusByte: null,
			binding: { state: "not-probed", reason: null } })]), () => {});
		activate(view, "hit-0");
		expect(nodes(view.describe(native)).find(node => node.k === "code")?.p).toMatchObject({
			text: snippet, start: 10, marks: [{ line: 11, tone: "accent" }, { line: 12, tone: "accent" }],
		});
		for (const range of [[5, 11], [13, 20]] as const) {
			const hit = match({ snippet, snippetStartLine: 10, snippetEndLine: 14,
				startLine: range[0], endLine: range[1], focusLine: null, focusByte: null });
			expect(snippetWindow(hit)?.mark).toEqual([Math.max(10, range[0]), Math.min(14, range[1])]);
		}
	});
	test("invalid or absent match coordinates never create source highlights", () => {
		for (const overrides of [
			{ focusLine: 99 }, { focusByte: 999 }, { snippetStartLine: 0 },
			{ snippetEndLine: 99 }, { focusLine: null, startLine: null },
			{ focusLine: null, startLine: 99, endLine: 100 },
		]) {
			const view = new SemctlInspector(search([match(overrides)]), () => {});
			activate(view, "hit-0");
			expect(nodes(view.describe(native)).find(node => node.k === "code")?.p?.marks).toBeUndefined();
		}
	});
	test("native tool expansion exposes the full typed read_source while folding restores the bounded preview", () => {
		const source = Array.from({ length: 40 }, (_, index) => `source line ${index + 1}`).join("\n");
		const payload: Display = { version: 1, identity: search().identity, kind: "source", path: "src/main.ts", text: source,
			language: "typescript", sourceOrigin: "content-plane", contentHash: "hash", encoding: "utf-8", binary: false, truncated: false,
			byteStart: 0, byteEnd: source.length, fileSize: source.length, lineStart: 10, lineEnd: 49 };
		const r = createInspectorRenderer("semctx", "read_source")({ requestRender: () => {}, fallback });
		const value = result(payload, { mcpToolName: "read_source" }), view = component(r, value);
		expect(nodes(view.describe(native)).find(node => node.k === "code")?.p?.text).not.toContain("source line 40");
		expect(component(r, value, { ...compact, expanded: true })).toBe(view);
		expect(nodes(view.describe(native)).find(node => node.k === "code")?.p).toMatchObject({ text: source, start: 10, numbers: true });
		expect(component(r, value)).toBe(view);
		expect(nodes(view.describe(native)).find(node => node.k === "code")?.p?.text).not.toContain("source line 40");
	});
	test("whole-tool expansion reveals complete open-match snippets without opening other matches", () => {
		const snippet = Array.from({ length: 40 }, (_, index) => `snippet line ${index + 1}`).join("\n");
		const value = result(search([match({ snippet, snippetStartLine: 100, snippetEndLine: 139, focusLine: 129 }),
			match({ path: "src/other.ts" })]));
		const r = renderer(), view = component(r, value);
		activate(view, "hit-0");
		const bounded = nodes(view.describe(native)).find(node => node.k === "code")?.p?.text;
		expect(bounded).not.toContain("snippet line 40");
		component(r, value, { ...compact, expanded: true });
		const full = nodes(view.describe(native)).filter(node => node.k === "code");
		expect(full).toHaveLength(1);
		expect(full[0].p).toMatchObject({ text: snippet, start: 100, marks: [{ line: 129, tone: "accent" }] });
		component(r, value);
		expect(nodes(view.describe(native)).find(node => node.k === "code")?.p?.text).toBe(bounded);
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
