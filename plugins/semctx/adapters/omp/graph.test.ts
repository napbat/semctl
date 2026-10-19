import { describe, expect, test } from "bun:test";
import type { Component, DescribeContext, NativeNode } from "@oh-my-pi/pi-tui";
import type { Hit } from "./inspector";
import { GraphInspector, graphRelations, readGraph, type GraphData } from "./graph";

const native: DescribeContext = { cols: 80, dark: true, reduceMotion: false, stylesheets: true, supports: () => true, feature: () => false };
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
	return nodes(root).map(n => n.k === "text" ? n.p?.text ?? n.p?.spans?.map(s => s.t).join("") ?? "" : "").join("\n");
}
function action(view: GraphInspector, act: string): void {
	view.handleNativeEvent({ type: "action", key: "", act, mods: [] });
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
	test("hosts without native graph elements retain names, hover details and relationship selection", () => {
		const view = new GraphInspector(graph(), () => {}, () => ({} as Component));
		const fallback: DescribeContext = { ...native, supports: kind => kind !== "el" && kind !== "tree" && kind !== "section" };
		const root = view.describe(fallback);
		expect(nodes(root).some(node => node.k === "image" || node.k === "el")).toBe(false);
		const first = nodes(root).find(node => node.key === "node-0");
		expect(first?.p?.title).toContain("Details A");
		action(view, "node:0");
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
		expect(tree.p?.nodes[0].label).toEqual([{ t: "◇ A", s: "accent", fx: "pulse" }]);
		const reduced = nodes(view.describe({ ...native, reduceMotion: true })).find(n => n.key === "node-0");
		if (!reduced || reduced.k !== "tree") throw new Error("Missing reduced-motion tree");
		expect(reduced.p?.nodes[0].label).toEqual([{ t: "◇ A", s: "accent", fx: undefined }]);
		const before = changes;
		view.setActivity(["app.main"]);
		expect(changes).toBe(before);
		view.setActivity([]);
		tree = nodes(view.describe(native)).find(n => n.key === "node-0");
		if (!tree || tree.k !== "tree") throw new Error("Missing inactive tree");
		expect(typeof tree.p?.nodes[0].label).toBe("string");
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
		expect(scoped.find(node => node.key === "node-1")?.p?.title).toContain("Selected file 0");
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
