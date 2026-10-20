import { afterEach, describe, expect, spyOn, test } from "bun:test";
import type { Component, DescribeContext, NativeNode } from "@oh-my-pi/pi-tui";
import { readDisplay, SemctlInspector, type Hit } from "./inspector";
import { GraphInspector, graphRelations, readGraph, type GraphData } from "./graph";
import { asNativeElement } from "./native-elements";
import { conceptRevision, resetConcepts } from "./concept-colors";
import { lookupGraphColor, setGraphColorMode } from "./graph-colors";
import { nativeGraphLayout } from "./graph-native-layout";
import { conceptClass } from "./native-concepts";
import { DEFAULT_GRAPH_APPEARANCE, GraphAppearanceStore, type GraphAppearance } from "./graph-appearance";
import { indexingParts, type GraphIndexingCounts } from "./graph-indexing";
import { GraphPage } from "./graph-page";

afterEach(() => { resetConcepts(); setGraphColorMode("concepts"); });

const native: DescribeContext = { cols: 80, dark: true, reduceMotion: false, supports: () => true, feature: () => false };
function graph(): GraphData {
	return { kind: "graph", title: "Relationships", query: "A", nodes: ["A", "B", "C", "D", "isolated"].map(id => ({ id, label: id, detail: `Details ${id}`, hit: null })),
		edges: [{ from: "A", to: "B", label: "calls" }, { from: "A", to: "C", label: "calls" },
			{ from: "B", to: "D", label: "calls" }, { from: "C", to: "D", label: "structural relation" },
			{ from: "D", to: "A", label: "calls" }, { from: "D", to: "D", label: "recurses" },
			{ from: "A", to: "B", label: "also references" }], roots: ["A"], caveats: ["Bounded graph; unresolved calls are absent."], truncated: true };
}
function hit(overrides: Partial<Hit>): Hit {
	return { path: null, symbol: null, qualifiedName: null, language: null, kind: null, score: null,
		startLine: null, endLine: null, startByte: null, endByte: null, snippet: null,
		snippetStartLine: null, snippetEndLine: null, snippetStartByte: null, snippetEndByte: null, focusLine: null, focusByte: null,
		codebaseId: null, binding: { state: "not-probed", reason: null }, ...overrides };
}
function nodes(root: NativeNode): NativeNode[] {
	const result = [root];
	if (root.k === "section" && root.p?.collapsed) return result;
	for (const child of root.c ?? []) if ("k" in child) result.push(...nodes(child));
	return result;
}
function content(root: NativeNode): string {
	return nodes(root).map(n => n.k === "text" ? n.p?.text ?? n.p?.spans?.map(s => s.t).join("") ?? "" : n.k === "badge" ? n.p?.text ?? "" : "").join("\n");
}
function action(view: GraphInspector, act: string): void {
	view.handleNativeEvent({ type: "action", key: "", act, mods: [] });
}
function expectCompact(title: string | undefined): void {
	if (title === undefined) throw new Error("Missing tooltip");
	expect(title.split("\n").length).toBeLessThanOrEqual(3);
	for (const line of title.split("\n")) expect(line.length).toBeLessThanOrEqual(88);
}

describe("bounded graph carrier", () => {
	test("preserves cycles, shared parents, self loops and parallel edges without repairing topology", () => {
		const data = graph();
		expect(readGraph({ ...data }, () => false)).toBe(true);
		expect(graphRelations(data, "D").map(r => [r.node.id, r.direction, r.label])).toEqual([
			["B", "incoming", "calls"], ["C", "incoming", "structural relation"], ["A", "outgoing", "calls"], ["D", "self", "recurses"],
		]);
		expect(graphRelations(data, "A").map(r => r.index)).toEqual([0, 1, 4, 6]);
		expect(graphRelations(data, "isolated")).toEqual([]);
	});
	test("rejects unknown endpoints, duplicate IDs/roots and invalid hit carriers", () => {
		const data = graph();
		for (const value of [
			{ ...data, nodes: [...data.nodes, data.nodes[0]] }, { ...data, roots: ["missing"] }, { ...data, roots: ["A", "A"] },
			{ ...data, nodes: [{ id: "", label: "empty id", detail: null, hit: null }] },
			{ ...data, edges: [{ from: "A", to: "missing", label: "calls" }] },
			{ ...data, edges: [{ from: "missing", to: "A", label: "calls" }] },
			{ ...data, edges: [{ from: "A", to: "B", label: null }] }, { ...data, truncated: "yes" },
			{ ...data, caveats: [1] }, { ...data, query: undefined },
			{ ...data, nodes: [{ ...data.nodes[0], hit: {} }] },
		]) expect(readGraph(value, () => false)).toBe(false);
		let checked = 0;
		expect(readGraph({ ...data, nodes: [{ ...data.nodes[0], hit: { actual: true } }], edges: [] }, value => { checked++; return !!value; })).toBe(true);
		expect(checked).toBe(1);
	});
	test("validates optional per-node catalog states without rejecting legacy graphs", () => {
		const data = graph(), base = data.nodes[0];
		for (const indexing of ["pending", "processing", "ready", "failed", "unknown"]) {
			expect(readGraph({ ...data, nodes: [{ ...base, indexing }], edges: [] }, () => false)).toBe(true);
		}
		for (const fields of [
			{ indexing: null }, { indexing: 2 }, { indexing: "queued" }, { indexing: {} },
			{ indexingError: "orphan error" }, { indexing: "ready", indexingError: "contradiction" },
			{ indexing: "failed", indexingError: null }, { indexing: "failed", indexingError: "" },
			{ indexing: "failed", indexingError: " \n" }, { indexing: "failed", indexingError: "x".repeat(4097) },
			{ indexing: "failed", indexingError: "unsafe\u001b[31m" },
		]) expect(readGraph({ ...data, nodes: [{ ...base, ...fields }], edges: [] }, () => false)).toBe(false);
		const carrier = { ...data, version: 1, identity: { codebaseId: "test", name: null, localRoot: null, copy: "canonical" },
			nodes: [{ ...base, indexing: "failed", indexingError: "Backend unavailable\nRetry scheduled" }], edges: [] };
		expect(readDisplay(carrier, "trace")?.kind).toBe("graph");
		expect(readDisplay({ ...carrier, nodes: [{ ...base, indexing: "invented" }] }, "trace")).toBeUndefined();
		expect(readGraph({ ...data }, () => false)).toBe(true);
		expect(readGraph({ ...data, nodes: [{ ...base, indexing: "failed", indexingError: "x".repeat(4096) }], edges: [] }, () => false)).toBe(true);
	});
	test("accepts optional typed layer loading metadata and rejects non-boolean states", () => {
		const data = graph();
		for (const loading of [true, false]) expect(readGraph({ ...data, loading }, () => false)).toBe(true);
		for (const loading of [null, "true", 1, {}]) expect(readGraph({ ...data, loading }, () => false)).toBe(false);
		const view = new GraphInspector({ ...data, loading: true }, () => {}, () => ({} as Component));
		expect(content(view.describe(native))).toContain("Relationship layers loading");
		expect(nodes(view.describe(native)).some(node => node.key === "quick-0")).toBe(true);
	});
	test("accepts exact structural bounds and rejects overflow, rather than silently dropping relationships", () => {
		const data = graph();
		const bounded = { ...data, nodes: Array.from({ length: 64 }, (_, i) => ({ id: String(i), label: String(i), detail: null, hit: null })),
			roots: ["0"], edges: Array.from({ length: 128 }, (_, i) => ({ from: String(i % 64), to: String((i + 1) % 64), label: `edge ${i}` })) };
		expect(readGraph(bounded, () => false)).toBe(true);
		expect(readGraph({ ...bounded, nodes: [...bounded.nodes, { id: "64", label: "overflow", detail: null, hit: null }] }, () => false)).toBe(false);
		expect(readGraph({ ...bounded, edges: [...bounded.edges, bounded.edges[0]] }, () => false)).toBe(false);
	});
});

describe("native graph navigation", () => {
	test("wraps inline and overview diagrams in the shared well only on styled hosts", () => {
		const styled: DescribeContext = { ...native, feature: name => name === "styles" };
		for (const presentation of ["inline", "overview"] as const) {
			const view = new GraphInspector(graph(), () => {}, () => ({} as Component), null, undefined, "body", presentation);
			const root = view.describe(styled), well = root.c?.find(child => "k" in child && child.key === "graph-well");
			if (!well || !("k" in well)) throw new Error("Missing graph well");
			expect(asNativeElement(well)?.p.class).toBe("semctl-graph-well");
			expect(well.c).toHaveLength(1);
			expect(well.c?.[0] && "k" in well.c[0] ? asNativeElement(well.c[0])?.key : undefined).toBe("graph-preview");
			expect(root.c?.some(child => "k" in child && child.key === "graph-preview")).toBe(false);
			for (const cx of [native, { ...styled, supports: (kind: string) => kind !== "el" } as DescribeContext]) {
				const fallback = view.describe(cx);
				expect(nodes(fallback).some(node => node.key === "graph-well" || node.key === "graph-preview")).toBe(false);
				expect(content(fallback)).toContain("Native graph drawing unavailable · use node navigation below.");
			}
		}
	});
	test("keeps a supplied diagram directly in the column when the host has no styles", () => {
		const view = new GraphInspector(graph(), () => {}, () => ({} as Component));
		const diagram: NativeNode = { k: "text", key: "graph-preview", p: { text: "Diagram" } };
		const preview = spyOn(view as unknown as { diagram(cx: DescribeContext): NativeNode | undefined }, "diagram").mockReturnValue(diagram);
		try {
			const root = view.describe(native);
			expect(root.c).toContain(diagram);
			expect(nodes(root).some(node => node.key === "graph-well")).toBe(false);
		} finally { preview.mockRestore(); }
	});
	test("uses native state badges with the original titles and retains plain text fallbacks", () => {
		const view = new GraphInspector({ ...graph(), loading: true }, () => {}, () => ({} as Component));
		const root = view.describe(native), badges = nodes(root).filter(node => node.k === "badge");
		expect(badges.map(node => node.p)).toEqual([
			{ text: "Partial graph", tone: "warning", title: "The graph is truncated. The original result retains all returned data." },
			{ text: "Relationship layers loading", tone: "muted", title: "Available nodes remain usable." },
		]);
		const counts = nodes(root).find(node => node.k === "text" && node.p?.text === "5 nodes · 7 relationships");
		expect(counts?.p?.tone).toBe("muted");
		expect(counts?.p?.role).toBe("semctl.graph.hint");
		const fallback = view.describe({ ...native, supports: kind => kind !== "badge" });
		expect(nodes(fallback).some(node => node.k === "badge")).toBe(false);
		for (const badge of badges) {
			const label = nodes(fallback).find(node => node.k === "text" && node.p?.text === badge.p?.text);
			expect(label?.p?.tone).toBe(badge.p?.tone);
			expect(label?.p?.title).toBe(badge.p?.title);
		}
	});
	test("keeps drawing counts and the node selection hint in one quiet row", () => {
		const data = graph();
		data.nodes = Array.from({ length: 96 }, (_, index) => ({ id: String(index), label: `File ${index}`, category: "file",
			path: `module-${index % 4}/${index}.ts`, detail: null, hit: null }));
		data.roots = ["0"]; data.edges = [];
		const view = new GraphInspector(data, () => {}, () => ({} as Component), null, undefined, "body", "overview");
		const row = nodes(view.describe({ ...native, feature: name => name === "styles" })).find(node => node.key === "graph-drawing-summary");
		if (!row) throw new Error("Missing drawing summary");
		expect(row.k).toBe("row");
		expect(content(row)).toContain("group summaries · 0 routes drawn");
		expect(content(row)).toContain("Select a dot for details");
		const labels = nodes(row).filter(node => node.k === "text");
		expect(labels.map(node => [node.p?.role, node.p?.tone])).toEqual([
			["semctl.graph.hint", "muted"], ["semctl.graph.hint", "muted"],
		]);
		expect(labels[0].p?.grow).toBe(1);
		expect(labels[0].p?.title).toBeTruthy();
		expect(labels[1].p?.shrink).toBe(0);
		expect(content(view.describe(native))).not.toContain("Select a dot for details");
	});
	test("hosts without native graph elements retain names, hover details and relationship selection", () => {
		const view = new GraphInspector(graph(), () => {}, () => ({} as Component));
		const fallback: DescribeContext = { ...native, supports: kind => String(kind) !== "el" && kind !== "tree" && kind !== "section" };
		const root = view.describe(fallback);
		expect(nodes(root).some(node => node.k === "image" || asNativeElement(node) !== undefined)).toBe(false);
		action(view, "node:0");
		expect(content(view.describe(fallback))).toContain("Details A");
		action(view, "edge:0");
		expect(content(view.describe(fallback))).toContain("A → B · calls");
	});
	test("begins compact, preserves caveats and truncation, and does not create a source preview until selected", () => {
		let previews = 0;
		const data = graph();
		data.nodes[0].hit = hit({ symbol: "A" });
		const view = new GraphInspector(data, () => {}, () => { previews++; return { render: () => ["preview"], invalidate() {} }; });
		expect(nodes(view.describe(native)).find(node => node.key === "graph-notes")?.p?.title).toContain(data.caveats[0]);
		expect(nodes(view.describe(native)).some(node => node.p?.tone === "warning")).toBe(true);
		expect(nodes(view.describe(native)).some(n => n.key === "node-4")).toBe(false);
		expect(previews).toBe(0);
		action(view, "node:0");
		expect(previews).toBe(1);
		const root = view.describe(native);
		expect(nodes(root).some(n => n.k === "tree")).toBe(true);
		expect(content(root)).toContain("Details A");
		action(view, "clear");
		expect(content(view.describe(native))).not.toContain("Details A");
	});
	test("navigates a real directed cycle and retains the chosen edge label, including shared targets", () => {
		const view = new GraphInspector(graph(), () => {}, () => { throw new Error("No hit supplied"); });
		action(view, "node:0");
		view.handleNativeEvent({ type: "activate", key: "relationships", item: "edge-0" });
		expect(content(view.describe(native))).toContain("A → B · calls");
		view.handleNativeEvent({ type: "select", key: "relationships", item: "edge-2" });
		expect(content(view.describe(native))).toContain("B → D · calls");
		const tree = nodes(view.describe(native)).find(n => n.k === "tree" && n.key === "relationships");
		if (!tree || tree.k !== "tree") throw new Error("No native relationship tree");
		expect(tree.p?.nodes[0].children?.map(n => n.label)).toEqual(["← calls · B", "← structural relation · C", "→ calls · A", "↻ recurses · D"]);
		view.handleNativeEvent({ type: "activate", key: "relationships", item: "edge-4" });
		expect(content(view.describe(native))).toContain("D → A · calls");
	});
	test("keeps independent expansion and handles no-tree/narrow/reduced-motion native hosts", () => {
		const a = new GraphInspector(graph(), () => {}, () => ({} as Component));
		const b = new GraphInspector(graph(), () => {}, () => ({} as Component));
		a.handleNativeEvent({ type: "toggle", key: "card/graph-nodes", collapsed: false });
		expect(nodes(a.describe(native)).some(n => n.key === "node-4")).toBe(true);
		expect(nodes(b.describe(native)).some(n => n.key === "node-4")).toBe(false);
		a.move(1);
		const narrow = { ...native, cols: 24, dark: false, reduceMotion: true, supports: (kind: string) => kind !== "tree" && kind !== "section" } as DescribeContext;
		const fallback = a.describe(narrow);
		expect(content(fallback)).toContain("Details A");
		expect(content(fallback)).toContain("→ calls");
		expect(nodes(fallback).some(n => n.k === "spinner")).toBe(false);
		expect(a.render(24).every(line => line.length <= 24)).toBe(true);
		action(a, "edge:999");
		expect(content(a.describe(narrow))).toContain("Details A");
	});
	test("bounds initial neighbor occurrences and exposes all remaining parallel links through a native control", () => {
		const data = graph();
		data.edges = Array.from({ length: 8 }, (_, index) => ({ from: "A", to: "B", label: `actual edge ${index}` }));
		const view = new GraphInspector(data, () => {}, () => ({} as Component));
		view.handleNativeEvent({ type: "toggle", key: "graph-nodes", collapsed: false });
		const initial = nodes(view.describe(native)).find(n => n.key === "node-0");
		if (!initial || initial.k !== "tree") throw new Error("Missing tree");
		expect(initial.p?.nodes[0].children).toHaveLength(5);
		expect(initial.p?.nodes[0].children?.at(-1)?.id).toBe("more-0");
		view.handleNativeEvent({ type: "activate", key: "node-0", item: "more-0" });
		const expanded = nodes(view.describe(native)).find(n => n.key === "node-0");
		if (!expanded || expanded.k !== "tree") throw new Error("Missing expanded tree");
		expect(expanded.p?.nodes[0].children?.map(n => n.id)).toEqual(Array.from({ length: 8 }, (_, index) => `link-0-${index}`));
	});
	test("highlights only exact observed targets with native terminal pulse and suppresses motion when requested", () => {
		const data = graph();
		data.nodes[0].hit = hit({ symbol: "main", qualifiedName: "app.main", path: "src/main.ts" });
		let changes = 0;
		const view = new GraphInspector(data, () => changes++, () => ({} as Component));
		view.handleNativeEvent({ type: "toggle", key: "graph-nodes", collapsed: false });
		view.setActivity(["app.main.extra"]);
		let tree = nodes(view.describe(native)).find(n => n.key === "node-0");
		if (!tree || tree.k !== "tree") throw new Error("Missing tree");
		expect(typeof tree.p?.nodes[0].label).toBe("string");
		view.setActivity(["app.main"]);
		tree = nodes(view.describe(native)).find(n => n.key === "node-0");
		if (!tree || tree.k !== "tree") throw new Error("Missing active tree");
		expect(tree.p?.nodes[0].label).toEqual([{ t: "◇ A", s: "success", fx: "pulse" }]);
		const reduced = nodes(view.describe({ ...native, reduceMotion: true })).find(n => n.key === "node-0");
		if (!reduced || reduced.k !== "tree") throw new Error("Missing reduced-motion tree");
		expect(reduced.p?.nodes[0].label).toEqual([{ t: "◇ A", s: "success", fx: undefined }]);
		const before = changes;
		view.setActivity(["app.main"]);
		expect(changes).toBe(before);
		view.setActivity([]);
		tree = nodes(view.describe(native)).find(n => n.key === "node-0");
		if (!tree || tree.k !== "tree") throw new Error("Missing inactive tree");
		expect(typeof tree.p?.nodes[0].label).toBe("string");
	});
	test("keeps mixed catalog states visible in node navigation and selected details without tool activity", () => {
		const data = graph();
		data.nodes[0].indexing = "pending";
		data.nodes[1].indexing = "failed"; data.nodes[1].indexingError = "Embedding provider rejected this file";
		data.nodes[2].indexing = "ready"; data.nodes[3].indexing = "unknown";
		const view = new GraphInspector(data, () => {}, () => ({} as Component));
		view.handleNativeEvent({ type: "toggle", key: "graph-nodes", collapsed: false });
		const rendered = view.describe(native);
		expect(nodes(rendered).find(node => node.key === "node-1")?.p?.title).toContain("Embedding provider rejected this file");
		expect(content(rendered)).toContain("pending");
		expect(content(rendered)).toContain("failed");
		action(view, "node:1");
		expect(content(view.describe(native))).toContain("Embedding failed.");
		view.setActivity([]);
		expect(content(view.describe(native))).toContain("Embedding provider rejected this file");
		action(view, "node:4");
		expect(content(view.describe(native))).not.toContain("Embedding failed.");
	});
	test("node hovers stay compact with the first error line while selected details keep full descriptions", () => {
		const diagnostic = `Provider unavailable\n${"Exact failure reason. ".repeat(100)}`;
		for (const category of ["file", "group"] as const) {
			const data = graph();
			data.nodes[0] = { ...data.nodes[0], label: "src/main.ts", path: "src/main.ts", category,
				detail: "Full selected description", indexing: "failed", indexingError: diagnostic };
			const view = new GraphInspector(data, () => {}, () => ({} as Component));
			const title = nodes(view.describe(native)).find(node => node.key === "quick-0")?.p?.title!;
			expectCompact(title);
			expect(title.split("\n").filter(line => line === "src/main.ts")).toHaveLength(1);
			expect(title).toContain("Provider unavailable");
			expect(title).not.toContain("Exact failure reason");
			expect(title).not.toContain(data.nodes[0].detail!);
			action(view, "node:0");
			const selected = content(view.describe(native));
			expect(selected).toContain(diagnostic);
			expect(selected).toContain(data.nodes[0].detail!);
		}
	});
	test("indexing meter parts omit zero and unreported states and have no empty total", () => {
		const counts: GraphIndexingCounts = { ready: 0, processing: 0, pending: 0, failed: 0, unknown: 0, unreported: 3 };
		expect(indexingParts({ ...counts, unreported: 0 })).toBeUndefined();
		expect(indexingParts(counts)).toBeUndefined();
		expect(indexingParts({ ...counts, ready: 3, failed: 1 })).toEqual([
			{ value: 0.75, token: "success", label: "3 ready" }, { value: 0.25, token: "error", label: "1 failed" },
		]);
	});
	test("overview and scoped groups use ordered indexing meter parts with a text fallback", () => {
		const data = graph(), states = [["ready", 2], ["processing", 4], ["pending", 6], ["failed", 8], ["unknown", 10]] as const;
		data.nodes = states.flatMap(([indexing, count]) => Array.from({ length: count }, (_, index) => ({
			id: `${indexing}-${index}`, label: `${indexing}-${index}`, category: "file" as const,
			path: `src/${indexing}-${index}.ts`, indexing, detail: null, hit: null,
		})));
		data.edges = []; data.roots = [data.nodes[0].id];
		const expected = [
			{ value: 2 / 30, token: "success", label: "2 ready" }, { value: 4 / 30, token: "accent", label: "4 processing" },
			{ value: 6 / 30, token: "muted", label: "6 pending" }, { value: 8 / 30, token: "error", label: "8 failed" },
			{ value: 10 / 30, token: "dim", label: "10 unknown" },
		];
		const view = new GraphInspector(data, () => {}, () => ({} as Component), null, undefined, "body", "overview");
		const withoutMeter: DescribeContext = { ...native, supports: kind => kind !== "meter" };
		for (const scoped of [false, true]) {
			if (scoped) action(view, "group:0");
			const key = scoped ? "graph-scope" : "group-0", row = nodes(view.describe(native)).find(node => node.key === key)!;
			const meter = nodes(row).find(node => node.k === "meter");
			if (!meter || meter.k !== "meter") throw new Error("Missing group indexing meter");
			expect(meter.p?.parts).toEqual(expected);
			expect(meter.p?.value).toBe(2 / 30);
			expect(meter.p?.style).toBe("bar");
			expect(meter.p?.size).toBe("sm");
			expect(meter.p?.min).toEqual({ w: "10ch" });
			expect(meter.p?.max).toEqual({ w: "10ch" });
			expect(meter.p?.shrink).toBe(0);
			expect(meter.p?.title).toContain("Indexing:");
			for (const part of expected) expect(meter.p?.title).toContain(part.label);
			expect(meter.p?.aria).toBe(meter.p?.title);
			expect(content(row)).not.toContain("Indexing:");
			if (!scoped) {
				expect(row.p?.role).toBe("semctl.graph.group");
				expect(row.c?.find(node => "k" in node && node.k === "text" && node.p?.spans)?.p?.grow).toBe(1);
				expect(row.p?.title).toContain(meter.p?.title!);
				expect(row.p?.aria).toContain(meter.p?.title!);
				const count = row.c?.find(node => "k" in node && node.k === "row" && node.p?.min?.w === "4ch");
				expect(count && "k" in count ? count.p?.justify : undefined).toBe("end");
				expect(count && "k" in count ? nodes(count).find(node => node.k === "text")?.p?.text : undefined).toBe("30");
			}
			const fallback = nodes(view.describe(withoutMeter)).find(node => node.key === key)!;
			expect(nodes(fallback).some(node => node.k === "meter")).toBe(false);
			expect(content(fallback)).toContain(meter.p?.title!);
			expect(fallback.p?.title).toBe(row.p?.title);
			expect(fallback.p?.aria).toBe(row.p?.aria);
		}
	});
	test("every graph view hover stays within three short lines, even with long names and many caveats", () => {
		const data = graph(), long = "very-long-segment/".repeat(12);
		data.caveats = Array.from({ length: 7 }, (_, index) => `Caveat ${index}: ${"bounded detail ".repeat(10)}`);
		data.nodes = data.nodes.map(node => ({ ...node, label: `${long}${node.label}`, path: `${long}${node.id}.ts`, category: "file" as const,
			indexing: "failed" as const, indexingError: `${"Provider failure ".repeat(12)}\nsecond line` }));
		for (const presentation of ["inline", "overview"] as const) {
			const view = new GraphInspector(data, () => {}, () => ({} as Component), null, undefined, "body", presentation);
			view.handleNativeEvent({ type: "toggle", key: "graph-nodes", collapsed: false });
			const titles = nodes(view.describe(native)).map(node => node.p?.title).filter((title): title is string => typeof title === "string");
			expect(titles.length).toBeGreaterThan(0);
			for (const title of titles) expectCompact(title);
			const notes = nodes(view.describe(native)).find(node => node.key === "graph-notes");
			expect(notes?.p?.title).toContain("+5 more notes");
			expect(notes?.p?.aria).toContain("Caveat 6");
		}
	});
	test("group hovers stay compact and exploration retains every source", () => {
		const data = graph();
		data.nodes = Array.from({ length: 96 }, (_, index) => ({ id: String(index), label: `File ${index}`,
			category: "file", path: `src/file-${index}.ts`, detail: `Selected file ${index}`, hit: null }));
		data.roots = ["0"]; data.edges = [];
		const view = new GraphInspector(data, () => {}, () => ({} as Component), null, undefined, "body", "overview");
		const group = nodes(view.describe(native)).find(node => node.key === "group-0")!;
		expectCompact(group.p?.title);
		expect(group.p?.title).toContain("96 nodes");
		expect(group.p?.title).not.toContain("src/file-");
		action(view, "group:0");
		view.handleNativeEvent({ type: "toggle", key: "graph-nodes", collapsed: false });
		action(view, "more-nodes"); action(view, "more-nodes");
		const expanded = nodes(view.describe(native));
		expect(expanded.some(node => node.p?.title?.includes("src/file-95.ts"))).toBe(true);
		action(view, "node:95");
		expect(content(view.describe(native))).toContain("Selected file 95");
	});
	test("later reference evidence remains selectable with its complete directed binding", () => {
		const data = graph();
		data.nodes = ["A", "B"].map(id => ({ id, label: id, detail: null, hit: null, category: "file", path: `src/${id}.ts` }));
		data.edges = Array.from({ length: 12 }, (_, index) => ({ from: "A", to: "B", label: "references", kind: "reference",
			symbol: `run${index}`, moniker: `local::run${index}` }));
		const view = new GraphInspector(data, () => {}, () => ({} as Component));
		action(view, "node:0");
		action(view, "more-related");
		action(view, "edge:11");
		const root = view.describe(native), last = data.edges[11];
		expect(content(root)).toContain(`A → B · references · ${last.symbol}`);
		expect(nodes(root).some(node => node.p?.title === last.moniker)).toBe(true);
	});
	test("a same-named external target cannot inherit local request activity", () => {
		const data = graph();
		data.nodes[0].hit = hit({ codebaseId: "local", symbol: "run", path: "src/lib.rs" });
		data.nodes[1].hit = hit({ codebaseId: "foreign", symbol: "run", path: "src/lib.rs" });
		const view = new GraphInspector(data, () => {}, () => ({} as Component), "local");
		view.handleNativeEvent({ type: "toggle", key: "graph-nodes", collapsed: false });
		view.setActivity(["run", "src/lib.rs"]);
		const tree = nodes(view.describe(native)).find(n => n.key === "node-0");
		if (!tree || tree.k !== "tree") throw new Error("Missing tree");
		expect(Array.isArray(tree.p?.nodes[0].label)).toBe(true);
		const foreign = tree.p?.nodes[0].children?.find(n => n.id === "link-0-0");
		expect(typeof foreign?.label).toBe("string");
	});
	test("paginates every node without skipping the first entry of a later page", () => {
		const data = graph();
		data.nodes = Array.from({ length: 96 }, (_, i) => ({ id: String(i), label: `Node ${i}`, detail: `Detail ${i}`, hit: null }));
		data.roots = ["0"]; data.edges = [];
		const view = new GraphInspector(data, () => {}, () => { throw new Error("No source supplied"); });
		view.handleNativeEvent({ type: "toggle", key: "graph-nodes", collapsed: false });
		expect(nodes(view.describe(native)).some(node => node.key === "node-31")).toBe(true);
		expect(nodes(view.describe(native)).some(node => node.key === "node-32")).toBe(false);
		action(view, "more-nodes");
		expect(nodes(view.describe(native)).some(node => node.key === "node-32")).toBe(true);
		expect(nodes(view.describe(native)).some(node => node.key === "node-63")).toBe(true);
		expect(nodes(view.describe(native)).some(node => node.key === "node-64")).toBe(false);
		action(view, "node:32");
		expect(content(view.describe(native))).toContain("Detail 32");
		action(view, "more-nodes");
		expect(nodes(view.describe(native)).some(node => node.key === "node-95")).toBe(true);
		expect(content(view.describe(native))).toContain("Detail 32");
	});
	test("group drilldown keeps original node actions and crossing a link updates the group context", () => {
		const data = graph();
		data.nodes = Array.from({ length: 64 }, (_, index) => ({
			id: String(index), label: `File ${index}`, path: `module-${index % 2}/${index}.ts`,
			category: "file" as const, detail: `Selected file ${index}`, hit: null,
		}));
		data.edges = [{ from: "0", to: "1", label: "cross-group import", kind: "import" }];
		data.roots = ["63"];
		const view = new GraphInspector(data, () => {}, () => { throw new Error("No source supplied"); }, null, undefined, "body", "overview");
		const cx: DescribeContext = { ...native, feature: name => name === "styles" };
		const group = nodes(view.describe(cx)).find(node => node.p?.actions?.click === "group:0")!;
		action(view, group.p!.actions!.click!);
		view.handleNativeEvent({ type: "toggle", key: "graph-nodes", collapsed: false });
		const scoped = nodes(view.describe(cx));
		expect(scoped.find(node => node.key === "node-1")?.p?.title).toContain("module-0/0.ts");
		expect(scoped.some(node => node.key === "node-2")).toBe(false);
		action(view, "node:1");
		expect(content(view.describe(cx))).toContain("Selected file 0");
		action(view, "edge:0");
		const crossed = view.describe(cx);
		expect(content(crossed)).toContain("Selected file 1");
		const breadcrumb = nodes(crossed).find(node => node.key === "graph-scope")!;
		expect(content(breadcrumb)).toContain("module-1");
		action(view, "overview");
		expect(nodes(view.describe(cx)).some(node => node.key === "selection")).toBe(false);
		expect(nodes(view.describe(cx)).some(node => node.p?.actions?.click === "group:0")).toBe(true);
	});
	test("appearance edits retain drilldown, exact selected source and relationship navigation", () => {
		const data = graph(); let previews = 0;
		data.nodes = Array.from({ length: 96 }, (_, index) => ({ id: String(index), label: `File ${index}`,
			category: "file", path: `module-${index % 30}/${index}.ts`, detail: `Selected file ${index}`, hit: null }));
		data.nodes[0].hit = hit({ symbol: "sourceZero", path: "module-0/0.ts" });
		data.roots = ["0"]; data.edges = [{ from: "0", to: "1", label: "exact cross-group import", kind: "import" }];
		const view = new GraphInspector(data, () => {}, () => {
			previews++;
			return { render: () => ["Source zero"], describe: () => ({ k: "text", p: { text: "Source zero" } }) };
		}, "appearance-scope", undefined, "body", "overview");
		const cx: DescribeContext = { ...native, feature: feature => feature === "styles" };
		let appearance: Readonly<GraphAppearance> = DEFAULT_GRAPH_APPEARANCE;
		view.setAppearance(appearance);
		view.describe(cx);
		action(view, "group:0"); action(view, "node:0");
		const scope = content(nodes(view.describe(cx)).find(node => node.key === "graph-scope")!);
		expect(previews).toBe(1);
		for (const patch of [{ layout: "radial" }, { detail: "calm" }, { color: "neutral" }, { labels: "hover" }, { links: "clear" }, { motion: "off" }] as const) {
			appearance = { ...appearance, ...patch };
			view.setAppearance(appearance);
			const root = view.describe(cx), diagram = nodes(root).map(asNativeElement).find(node => node?.key === "graph-preview");
			expect(content(nodes(root).find(node => node.key === "graph-scope")!)).toBe(scope);
			expect(content(root)).toContain("Selected file 0");
			expect(nodes(root).some(node => node.key === "preview-0")).toBe(true);
			expect(diagram?.p.attrs?.["data-layout"]).toBe(appearance.layout);
			expect(diagram?.p.attrs?.["data-motion"]).toBe(appearance.motion === "off" ? "reduced" : "full");
		}
		expect(previews).toBe(1);
		action(view, "edge:0");
		expect(content(view.describe(cx))).toContain("File 0 → File 1 · exact cross-group import");
		expect(content(nodes(view.describe(cx)).find(node => node.key === "graph-scope")!)).toContain("module-1");
	});
	test("project detail budgets change only the summary drawing and keep every exact node reachable", () => {
		const data = graph();
		data.nodes = Array.from({ length: 96 }, (_, index) => ({ id: String(index), label: `File ${index}`,
			category: "file", path: `module-${index % 30}/${index}.ts`, detail: `Exact node ${index}`, hit: null }));
		data.roots = ["0"]; data.edges = [];
		const view = new GraphInspector(data, () => {}, () => { throw new Error("No source supplied"); }, "detail-budget", undefined, "body", "overview");
		const cx: DescribeContext = { ...native, feature: feature => feature === "styles" };
		view.setAppearance({ ...DEFAULT_GRAPH_APPEARANCE, detail: "calm" });
		const calm = nodes(view.describe(cx)).map(asNativeElement).filter(node => node?.p.attrs?.["data-node"] !== undefined);
		view.setAppearance({ ...DEFAULT_GRAPH_APPEARANCE, detail: "detailed" });
		const detailed = nodes(view.describe(cx)).map(asNativeElement).filter(node => node?.p.attrs?.["data-node"] !== undefined);
		expect(calm).toHaveLength(12);
		expect(detailed).toHaveLength(24);
		view.handleNativeEvent({ type: "toggle", key: "graph-nodes", collapsed: false });
		action(view, "more-nodes"); action(view, "more-nodes");
		expect(nodes(view.describe(cx)).some(node => node.key === "node-95")).toBe(true);
		action(view, "node:95");
		expect(content(view.describe(cx))).toContain("Exact node 95");
	});
	test("motion off suppresses observed node pulses and inspector forwarding retains the selection", () => {
		const data = graph(), display = { ...data, version: 1 as const,
			identity: { codebaseId: "appearance-forwarding", name: null, localRoot: null, copy: "canonical" as const } };
		const view = new SemctlInspector(display, () => {});
		view.move(1);
		view.setActivity(["A"]);
		view.setAppearance({ ...DEFAULT_GRAPH_APPEARANCE, motion: "off", layout: "radial" });
		const root = view.describe(native);
		const nested = root?.c?.find(child => child instanceof GraphInspector);
		if (!(nested instanceof GraphInspector)) throw new Error("No forwarded graph inspector");
		nested.handleNativeEvent({ type: "toggle", key: "graph-nodes", collapsed: false });
		const body = nested.describe(native), tree = nodes(body).find(node => node.key === "node-0");
		if (!tree || tree.k !== "tree") throw new Error("No observed node tree");
		expect(tree.p?.nodes[0].label).toEqual([{ t: "◇ A", s: "success", fx: undefined }]);
		expect(content(body)).toContain("Details A");
	});
	test("selected relationship pagination keeps later parallel links selectable", () => {
		const data = graph();
		data.edges = Array.from({ length: 25 }, (_, index) => ({ from: "A", to: "B", label: `relation ${index}` }));
		const view = new GraphInspector(data, () => {}, () => { throw new Error("No source supplied"); });
		action(view, "node:0");
		const first = nodes(view.describe(native)).find(node => node.k === "tree" && node.key === "relationships");
		if (!first || first.k !== "tree") throw new Error("Missing selected relationships");
		expect(first.p?.nodes[0].children?.some(node => node.id === "edge-24")).toBe(false);
		for (let page = 0; page < data.edges.length; page++) {
			const root = view.describe(native), tree = nodes(root).find(node => node.k === "tree" && node.key === "relationships");
			if (!tree || tree.k !== "tree") throw new Error("Missing selected relationships");
			const last = tree.p?.nodes[0].children?.find(node => node.id === "edge-24");
			if (last) { view.handleNativeEvent({ type: "activate", key: "relationships", item: last.id }); break; }
			const more = nodes(root).find(node => node.p?.actions?.click === "more-related");
			if (!more?.p?.actions?.click) throw new Error("Later relationships are unreachable");
			action(view, more.p.actions.click);
		}
		expect(content(view.describe(native))).toContain("A → B · relation 24");
	});
});

test("graph toolbar gives the overview a checkout subline without changing other operation headings or actions", () => {
	for (const id of ["overview", "trace"]) {
		const operation = { id, label: id === "overview" ? "Indexed overview" : "Symbol neighbourhood", operation: id, fields: [] };
		const view = new GraphPage({
			operation, operations: [operation], cwd: "/workspace/checkout-name",
			state: () => ({ loading: false, text: "Result" }), activity: () => ({ operations: [], targets: [], producer: null }),
			appearance: new GraphAppearanceStore(), createInspector: () => ({} as Component),
			requestRender() {}, height: () => 24, matches: () => false, close() {}, refresh() {}, showAppearance() {},
		});
		const root = view.describe(native), heading = nodes(root).find(node => node.key === "heading");
		if (!heading) throw new Error("Missing graph heading");
		const title = nodes(heading).find(node => node.p?.role === "semctl.graph.title");
		expect(title?.p?.text).toBe(operation.label);
		expect(title?.p?.title).toContain("configured semctl CLI · /workspace/checkout-name");
		const subline = nodes(heading).find(node => node.p?.role === "semctl.graph.hint");
		if (id === "overview") {
			expect(subline?.p?.text).toBe("checkout-name");
			expect(subline?.p?.tone).toBe("muted");
			expect(subline?.p?.title).toBe("/workspace/checkout-name");
		} else expect(subline).toBeUndefined();
		for (const action of ["analyses", "appearance", "refresh", "info", "close"]) {
			expect(nodes(root).some(node => node.p?.actions?.click === action)).toBe(true);
		}
		expect(content(root)).toContain("A analyses · S appearance · I details · R refresh · Esc close");
		view.dispose();
	}
});

test("existing graph results repaint shared structural cues on profile changes without moving geometry or creating previews", () => {
	const data = graph(), codebase = "retained-tool-graph";
	for (const node of data.nodes) { node.category = "file"; node.path = `src/${node.id}.ts`; }
	data.nodes[0].indexing = "failed"; data.nodes[0].indexingError = "Provider unavailable";
	let renders = 0;
	const view = new GraphInspector(data, () => renders++, () => { throw new Error("Color changes must not request source"); }, codebase);
	const cx = { ...native, feature: (name: string) => name === "styles" }, geometry = nativeGraphLayout(data, "inline"), revision = conceptRevision();
	const shared = view.describe(cx), color = lookupGraphColor(codebase, data.nodes[0], "concepts");
	if (!color) throw new Error("No source category");
	const cue = nodes(shared).map(asNativeElement).find(node => node?.key === "graph-color-cue");
	expect(cue?.p.class).toContain(conceptClass(color, true)); expect(cue?.p.title).toContain("Code structure:");
	expect(cue?.p.title).not.toContain("Inferred concept:");
	const before = renders;
	setGraphColorMode("neutral");
	const neutral = view.describe(cx);
	expect(renders).toBeGreaterThan(before); expect(conceptRevision()).toBe(revision);
	expect(nodes(neutral).map(asNativeElement).some(node => node?.key === "graph-color-cue")).toBe(false);
	expect(content(neutral)).toContain("failed");
	expect(nativeGraphLayout(data, "inline")).toBe(geometry);
	setGraphColorMode("structure");
	expect(nodes(view.describe(cx)).map(asNativeElement).find(node => node?.key === "graph-color-cue")?.p.class).toContain(conceptClass(color, true));
});

test("aggregate graph controls use the same shared palette and retain drawing positions through palette-only updates", () => {
	const data = graph(), codebase = "aggregate-palette";
	data.nodes = Array.from({ length: 96 }, (_, index) => ({ id: String(index), label: `File ${index}`, detail: null, hit: null,
		category: "file", path: `pkg${index % 4}/file${index}.ts` }));
	data.roots = ["0"]; data.edges = data.nodes.slice(1).map((node, index) => ({ from: String(index), to: node.id, label: "imports" }));
	const view = new GraphInspector(data, () => {}, () => { throw new Error("No source selection"); }, codebase, undefined, "body", "overview");
	const cx = { ...native, feature: (name: string) => name === "styles" };
	const before = view.describe(cx), revision = conceptRevision();
	const positions = nodes(before).map(asNativeElement).filter(node => node?.p.attrs?.["data-node"] !== undefined).map(node => [node?.p.attrs?.["data-node"], node?.p.class?.match(/sg-x\d+ sg-y\d+ sg-fx\d+ sg-fy\d+/)?.[0]]);
	const cue = nodes(before).map(asNativeElement).find(node => node?.key === "group-cue-0");
	expect(cue?.p.title).toContain("Code structure:");
	setGraphColorMode("neutral");
	expect(nodes(view.describe(cx)).map(asNativeElement).some(node => node?.key?.startsWith("group-cue-"))).toBe(false);
	setGraphColorMode("structure");
	const after = view.describe(cx);
	expect(conceptRevision()).toBe(revision);
	expect(nodes(after).map(asNativeElement).filter(node => node?.p.attrs?.["data-node"] !== undefined).map(node => [node?.p.attrs?.["data-node"], node?.p.class?.match(/sg-x\d+ sg-y\d+ sg-fx\d+ sg-fy\d+/)?.[0]])).toEqual(positions);
	expect(nodes(after).map(asNativeElement).find(node => node?.key === "group-cue-0")?.p.class).toBe(cue?.p.class);
});
