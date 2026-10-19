import { describe, expect, test } from "bun:test";
import type { DescribeContext, NativeNode } from "@oh-my-pi/pi-tui";
import { LiveGraphPopup, type LiveGraphState } from "./live-graph";
import type { GraphActivity } from "./graph-activity";
import type { Display } from "./inspector";
import { graphWindow, projectSummary } from "./graph-projection";
import { asNativeElement, type NativeElement } from "./native-elements";
import { groupIndexing, indexingCounts, indexingSummary } from "./graph-indexing";
import { DEFAULT_GRAPH_APPEARANCE } from "./graph-appearance";

type Overview = Extract<Display, { kind: "graph" }>;
function overview(paths: string[]): Overview {
	return { version: 1, kind: "graph", identity: { codebaseId: "popup-summary-isolated", name: null, copy: "checkout", localRoot: "D:/repo" },
		title: "Project overview", query: null, truncated: false, caveats: [], roots: paths.length ? ["0"] : [],
		nodes: paths.map((path, index) => ({ id: String(index), label: path, path, category: "file", detail: null, hit: null })), edges: [] };
}

describe("representative live project graph", () => {
	test("preserves membership, direction, relation kinds and the count of internal and parallel links", () => {
		const data = overview(["src/api/a.ts", "src/api/b.ts", "src/db/c.ts", "src/cache/d.ts"]);
		data.edges = [
			{ from: "0", to: "1", label: "contains", kind: "containment" },
			{ from: "0", to: "2", label: "imports", kind: "import" },
			{ from: "1", to: "2", label: "imports", kind: "import" },
			{ from: "2", to: "0", label: "imports", kind: "import" },
			{ from: "2", to: "3", label: "references", kind: "reference", symbol: "Connect", moniker: "fixture::Connect" },
			{ from: "2", to: "3", label: "member", kind: "association" },
			{ from: "3", to: "3", label: "references", kind: "reference", symbol: "Connect", moniker: "fixture::Connect" },
		];
		const summary = projectSummary(data), api = summary.groupOf.get("0")!, db = summary.groupOf.get("2")!, cache = summary.groupOf.get("3")!;
		expect(summary.groupOf.get("1")).toBe(api);
		expect(new Set([api, db, cache]).size).toBe(3);
		expect(summary.graph.edges.map(edge => [edge.from, edge.to, edge.kind])).toEqual(expect.arrayContaining([
			[api, db, "import"], [db, api, "import"], [db, cache, "association"], [db, cache, undefined],
		]));
		expect(summary.graph.edges).toHaveLength(4);
		expect(summary.groups.find(group => group.id === api)?.internal).toBe(1);
		expect(summary.groups.find(group => group.id === cache)?.internal).toBe(1);
		expect(summary.drawnLinks).toBe(5);
		expect(summary.omitted).toBe(0);
		expect(summary.graph.roots).toEqual([api]);
	});
	test("bounds dense previews without losing node membership or unreported link counts", () => {
		const data = overview(Array.from({ length: 180 }, (_, index) => [`module-${index}/a.ts`, `module-${index}/b.ts`]).flat());
		for (let group = 0; group < 180; group++) for (let step = 1; step <= 5; step++) {
			data.edges.push({ from: String(group * 2), to: String(((group + step) % 180) * 2), kind: "import", label: "imports" });
		}
		const summary = projectSummary(data);
		expect(summary.groups.flatMap(group => group.members.map(node => node.id)).sort()).toEqual(data.nodes.map(node => node.id).sort());
		expect(summary.omitted).toBeGreaterThan(0);
		expect(summary.graph.truncated).toBe(true);
		expect(summary.groups.reduce((sum, group) => sum + group.internal, 0) + summary.drawnLinks + summary.omitted).toBe(data.edges.length);
		expect(projectSummary({ ...data, nodes: [...data.nodes].reverse(), edges: [...data.edges].reverse() }).graph).toEqual(summary.graph);
	});
	test("keeps every original group addressable when the drawing folds groups together", () => {
		const paths = Array.from({ length: 24 }, (_, index) => `module-${index}/file.ts`), data = overview(paths);
		const summary = projectSummary(data);
		expect(summary.allGroups.map(group => group.members.map(node => node.path))).toEqual(expect.arrayContaining(paths.map(path => [path])));
		const folded = summary.groups.find(group => group.id === "other-groups")!;
		expect(folded.members.map(node => node.id).sort()).toEqual(summary.allGroups.slice(15).flatMap(group => group.members.map(node => node.id)).sort());
		expect(new Set(summary.allGroups.map(group => group.id)).size).toBe(24);
	});
	test("counts mixed indexing evidence and folds affected members without changing structural groups", () => {
		const data = overview(Array.from({ length: 24 }, (_, index) => `module-${index}/file.ts`));
		const before = projectSummary(data), folded = before.groups.find(group => group.id === "other-groups")!;
		const affected = new Map(folded.members.slice(0, 5).map((node, index) => [node.id, (["failed", "processing", "pending", "ready", "unknown"] as const)[index]]));
		const updated: Overview = { ...data, nodes: data.nodes.map(node => ({ ...node, ...(affected.has(node.id) ? { indexing: affected.get(node.id) } : {}) })) };
		const summary = projectSummary(updated), group = summary.groups.find(group => group.id === "other-groups")!;
		expect(summary.groups.map(group => [group.id, group.members.map(node => node.id)])).toEqual(before.groups.map(group => [group.id, group.members.map(node => node.id)]));
		expect(summary.graph.edges).toEqual(before.graph.edges);
		expect(summary.graph.roots).toEqual(before.graph.roots);
		expect(group.indexing).toEqual({ failed: 1, processing: 1, pending: 1, ready: 1, unknown: 1, unreported: folded.members.length - 5 });
		const dot = summary.graph.nodes.find(node => node.id === group.id)!;
		expect(dot.indexing).toBe("failed");
		for (const state of ["failed", "processing", "pending", "ready", "unknown"]) expect(dot.detail).toContain(`1 ${state}`);
		expect(summary.graph.nodes.filter(node => node.indexing !== undefined)).toHaveLength(1);
		for (const original of summary.allGroups.filter(group => !group.members.some(node => affected.has(node.id)))) {
			expect(original.indexing.unreported).toBe(original.members.length);
		}
		const mixed = overview(["src/api/a.ts", "src/api/b.ts", "src/api/c.ts", "src/db/d.ts", "src/cache/e.ts"]);
		mixed.nodes[0].indexing = "pending"; mixed.nodes[1].indexing = "failed"; mixed.nodes[2].indexing = "ready"; mixed.nodes[3].indexing = "unknown";
		const mixedSummary = projectSummary(mixed);
		expect(mixedSummary.graph.nodes.find(node => node.id === mixedSummary.groupOf.get("0"))?.detail).toContain("1 failed · 1 pending · 1 ready");
		expect(mixedSummary.graph.nodes.find(node => node.id === mixedSummary.groupOf.get("3"))?.indexing).toBe("unknown");
		expect(mixedSummary.graph.nodes.find(node => node.id === mixedSummary.groupOf.get("4"))?.indexing).toBeUndefined();
		const partiallyReported = overview(["src/a.ts", "src/b.ts"]);
		partiallyReported.nodes[0].indexing = "ready";
		const partiallyReportedGroup = projectSummary(partiallyReported).graph.nodes[0];
		expect(partiallyReportedGroup.indexing).toBe("unknown");
		expect(partiallyReportedGroup.detail).toContain("1 ready · 1 unreported");
	});
	test("excludes unreported synthetic hierarchy without hiding legacy sources or explicit structural evidence", () => {
		const data = overview(["src/a.ts"]);
		data.nodes[0].indexing = "ready";
		const hierarchy: Overview["nodes"] = (["directory", "project", "group"] as const).map(category => ({
			id: category, label: category, category, detail: null, hit: null, ...(category === "directory" ? { path: "src" } : {}),
		}));
		data.nodes.push(...hierarchy);
		const readyCounts = indexingCounts(data.nodes);
		expect(readyCounts).toEqual({ ready: 1, failed: 0, processing: 0, pending: 0, unknown: 0, unreported: 0 });
		expect(groupIndexing(readyCounts)).toBe("ready");
		expect(indexingSummary(readyCounts)).toBe("Indexing: 1 ready");
		const summary = projectSummary(data), fileGroup = summary.graph.nodes.find(node => node.id === summary.groupOf.get("0"))!;
		expect(fileGroup.indexing).toBe("ready");
		expect(summary.graph.nodes.filter(node => node.id !== fileGroup.id).every(node => node.indexing === undefined)).toBe(true);
		expect(summary.groupOf.get("directory")).toBe(fileGroup.id);
		const legacyFile = { ...data.nodes[0], id: "legacy-file", indexing: undefined };
		const legacyGeneric = { id: "legacy-generic", label: "Symbol", detail: null, hit: null };
		const hitNode = { ...hierarchy[0], id: "hit-node", hit: { path: "src/legacy.ts" } as NonNullable<Overview["nodes"][number]["hit"]> };
		const unknownDirectory = { ...hierarchy[0], indexing: "unknown" as const };
		const failedProject = { ...hierarchy[1], indexing: "failed" as const };
		const mixedCounts = indexingCounts([...data.nodes, legacyFile, legacyGeneric, hitNode, unknownDirectory, failedProject]);
		expect(mixedCounts).toEqual({ ready: 1, failed: 1, processing: 0, pending: 0, unknown: 1, unreported: 3 });
		expect(groupIndexing(mixedCounts)).toBe("failed");
	});
	test("bounds the drawing while retaining exact source topology for native navigation", () => {
		const data = overview(Array.from({ length: 60 }, (_, index) => `src/${index}.ts`));
		data.edges = Array.from({ length: 150 }, (_, index) => ({ from: String(index % 48), to: String((index + 1) % 48), label: `call ${index}` }));
		data.edges.push({ from: "59", to: "0", label: "outside drawing" });
		const window = graphWindow(data, data.nodes, "0");
		expect(window.nodes.map(node => node.id)).toEqual(data.nodes.slice(0, 48).map(node => node.id));
		expect(window.edges).toEqual(data.edges.slice(0, 128));
		expect(window.roots).toEqual(["0"]);
		expect(window.truncated).toBe(true);
		expect(data.nodes).toHaveLength(60);
		expect(data.edges.at(-1)).toEqual({ from: "59", to: "0", label: "outside drawing" });
	});
	test("does not present an incomplete source snapshot as a complete overview", () => {
		const summary = projectSummary({ ...overview(["src/a.ts"]), truncated: true, caveats: ["upstream edge limit"] });
		expect(summary.graph.truncated).toBe(true);
		expect(summary.graph.caveats).toContain("upstream edge limit");
		expect(summary.groups.flatMap(group => group.members.map(node => node.path))).toEqual(["src/a.ts"]);
	});
	test("preserves typed layer loading through summary and focused graph projections", () => {
		const data: Overview = { ...overview(["src/api/a.ts", "src/db/b.ts"]), loading: true, truncated: true, caveats: ["Import layer is loading"] };
		const summary = projectSummary(data);
		expect(summary.graph.loading).toBe(true);
		expect(graphWindow(data, data.nodes).loading).toBe(true);
		expect(summary.graph.caveats).toContain("Import layer is loading");
		expect(projectSummary({ ...data, loading: false }).graph.loading).toBe(false);
		expect(projectSummary(overview(["src/a.ts"])).graph.loading).toBeUndefined();
	});
});

const native: DescribeContext = {
	cols: 64, dark: true, reduceMotion: false,
	supports: kind => String(kind) === "el" || kind === "section" || kind === "meter", feature: name => name === "styles",
};
function tree(root: NativeNode): NativeNode[] {
	const result = [root];
	for (const child of root.c ?? []) if ("k" in child) result.push(...tree(child));
	return result;
}
function content(root: NativeNode): string {
	return tree(root).map(node => node.k === "text" ? node.p?.text ?? "" : "").join("\n");
}
function dots(root: NativeNode) {
	return tree(root).map(asNativeElement).filter((node): node is NativeElement => typeof node?.p.attrs?.["data-node"] === "string");
}
function popupState(display: Overview, activity: Partial<GraphActivity> = {}): LiveGraphState {
	return { request: { loading: false, text: "", display }, activity: { operations: [], targets: [], producer: null, ...activity } };
}
function popup(state: LiveGraphState) {
	return new LiveGraphPopup(state, () => {}, { close() {}, refresh() {}, expand() {}, appearance() {} });
}
function action(view: LiveGraphPopup, act: string): void {
	view.handleNativeEvent({ type: "action", key: "", act, mods: [] });
}
function selectedGroup(root: NativeNode): string | undefined {
	return dots(root).find(node => node.p.attrs?.["aria-pressed"] === true)?.p.attrs?.["data-node"] as string | undefined;
}
function inspection(root: NativeNode): NativeNode {
	const inspector = tree(root).find(node => node.p?.role === "semctl.live.selection");
	if (!inspector) throw new Error("Missing live group inspector");
	return inspector;
}
function neighbors(root: NativeNode): NativeNode[] {
	return tree(inspection(root)).filter(node => node.p?.role === "semctl.live.neighbor");
}
function expectLinks(root: NativeNode, incoming: number, outgoing: number, internal: number): void {
	const evidence = tree(inspection(root)).find(node => node.p?.role === "semctl.live.links")?.p?.aria;
	expect(evidence).toContain(`${incoming} incoming links`);
	expect(evidence).toContain(`${outgoing} outgoing links`);
	expect(evidence).toContain(`${internal} internal links`);
}

describe("live group inspection", () => {
	test("counts directed source links and navigates neighboring groups without opening another surface", () => {
		const data = overview(["src/api/a.ts", "src/api/b.ts", "src/db/c.ts", "src/cache/d.ts"]);
		data.edges = [
			{ from: "0", to: "1", label: "999 aggregate links" }, { from: "0", to: "0", label: "self" },
			{ from: "0", to: "2", label: "imports", kind: "import" }, { from: "1", to: "2", label: "imports", kind: "import" },
			{ from: "2", to: "0", label: "references", kind: "reference" }, { from: "1", to: "3", label: "calls", kind: "association" },
			{ from: "3", to: "3", label: "unrelated self" },
		];
		const summary = projectSummary(data, data.identity.codebaseId), api = summary.groupOf.get("0")!, db = summary.groupOf.get("2")!;
		let opened = 0;
		const view = new LiveGraphPopup(popupState(data), () => {}, { close() {}, refresh() {}, expand() { opened++; }, appearance() {} });
		try {
			const initial = view.describe(native), hint = content(inspection(initial));
			action(view, dots(initial).find(node => node.p.attrs?.["data-node"] === api)!.p.actions!.click!);
			const selected = view.describe(native);
			expect(selectedGroup(selected)).toBe(api);
			expectLinks(selected, 1, 3, 2);
			expect(tree(inspection(selected)).some(node => node.p?.aria === "2 member nodes")).toBe(true);
			expect(neighbors(selected)).toHaveLength(2);
			const dbAction = dots(selected).find(node => node.p.attrs?.["data-node"] === db)!.p.actions!.click!;
			const neighbor = neighbors(selected).find(node => node.p?.actions?.click === dbAction)!;
			expect(neighbor.p?.aria).toContain("1 incoming links");
			expect(neighbor.p?.aria).toContain("2 outgoing links");
			action(view, neighbor.p!.actions!.click!);
			const moved = view.describe(native);
			expect(selectedGroup(moved)).toBe(db);
			expectLinks(moved, 2, 1, 0);
			expect(opened).toBe(0);
			action(view, "clear-group");
			const cleared = view.describe(native);
			expect(selectedGroup(cleared)).toBeUndefined();
			expect(content(inspection(cleared))).toBe(hint);
			expect(neighbors(cleared)).toHaveLength(0);
			view.handleNativeEvent({ type: "toggle", key: "groups", collapsed: false });
			const explorer = view.describe(native), graphAction = dots(explorer).find(node => node.p.attrs?.["data-node"] === api)!.p.actions!.click!;
			const group = tree(explorer).find(node => node.p?.role === "semctl.live.group" && node.p.actions?.click === graphAction)!;
			expect(group.p?.aria).toContain("2 nodes");
			action(view, group.p!.actions!.click!);
			expect(selectedGroup(view.describe(native))).toBe(api);
			expectLinks(view.describe(native), 1, 3, 2);
			expect(opened).toBe(0);
		} finally { view.dispose(); }
	});
	test("inspects omitted source routes while bounding and deterministically ordering neighboring controls", () => {
		const data = overview(Array.from({ length: 12 }, (_, index) => `module-${index}/a.ts`));
		for (const from of data.nodes) for (const to of data.nodes) if (from !== to) data.edges.push({ from: from.id, to: to.id, label: "1000 links" });
		data.edges.push({ from: "0", to: "1", label: "parallel" }, { from: "0", to: "1", label: "parallel" });
		const summary = projectSummary(data, data.identity.codebaseId), id = summary.groupOf.get("0")!;
		expect(summary.omitted).toBeGreaterThan(0);
		expect(summary.graph.edges.filter(edge => edge.from === id || edge.to === id).length).toBeLessThan(22);
		const view = popup(popupState(data));
		try {
			const initial = view.describe(native);
			action(view, dots(initial).find(node => node.p.attrs?.["data-node"] === id)!.p.actions!.click!);
			const selected = view.describe(native);
			expectLinks(selected, 11, 13, 0);
			const controls = neighbors(selected).map(node => node.p?.actions?.click);
			expect(controls).toHaveLength(3);
			expect(new Set(controls).size).toBe(3);
			const parallel = summary.groupOf.get("1")!;
			expect(controls[0]).toBe(dots(selected).find(node => node.p.attrs?.["data-node"] === parallel)!.p.actions!.click);
			view.update(popupState({ ...data, nodes: [...data.nodes].reverse(), edges: [...data.edges].reverse() }));
			const reordered = view.describe(native);
			expect(selectedGroup(reordered)).toBe(id);
			expectLinks(reordered, 11, 13, 0);
			expect(neighbors(reordered).map(node => node.p?.actions?.click)).toEqual(controls);
		} finally { view.dispose(); }
	});
	test("counts links within and across folded groups using original edge endpoints", () => {
		const data = overview(Array.from({ length: 24 }, (_, index) => `module-${index}/a.ts`));
		const summary = projectSummary(data, data.identity.codebaseId), folded = summary.groups.find(group => group.id === "other-groups")!;
		const [a, b] = folded.members, [first, second] = summary.groups.filter(group => group.id !== folded.id);
		const c = first.members[0], d = second.members[0];
		data.edges = [
			{ from: a.id, to: b.id, label: "between original groups" }, { from: b.id, to: b.id, label: "self" },
			{ from: a.id, to: c.id, label: "out" }, { from: b.id, to: c.id, label: "parallel out" },
			{ from: c.id, to: b.id, label: "in" }, { from: d.id, to: a.id, label: "in" },
			{ from: c.id, to: c.id, label: "not folded" },
		];
		const view = popup(popupState(data));
		try {
			const initial = view.describe(native);
			action(view, dots(initial).find(node => node.p.attrs?.["data-node"] === folded.id)!.p.actions!.click!);
			const selected = view.describe(native);
			expectLinks(selected, 2, 2, 2);
			expect(tree(inspection(selected)).some(node => node.p?.aria === `${folded.members.length} member nodes`)).toBe(true);
			expect(neighbors(selected).map(node => node.p?.actions?.click)).toEqual(expect.arrayContaining(
				dots(selected).filter(node => node.p.attrs?.["data-node"] === first.id || node.p.attrs?.["data-node"] === second.id).map(node => node.p.actions!.click)));
		} finally { view.dispose(); }
	});
	test("retains stable action identities through reorder and rejects disappeared, foreign and malformed actions before repaint", () => {
		const data = overview(["src/api/a.ts", "src/db/b.ts"]), summary = projectSummary(data, data.identity.codebaseId);
		const api = summary.groupOf.get("0")!, view = popup(popupState(data));
		try {
			const initial = view.describe(native), oldAction = dots(initial).find(node => node.p.attrs?.["data-node"] === api)!.p.actions!.click!;
			const changed = { ...data, nodes: [...data.nodes,
				{ ...data.nodes[1], id: "db-2", path: "src/db/c.ts" }, { ...data.nodes[1], id: "db-3", path: "src/db/d.ts" }] };
			expect(projectSummary(changed, data.identity.codebaseId).groups.findIndex(group => group.id === api))
				.not.toBe(summary.groups.findIndex(group => group.id === api));
			view.update(popupState(changed));
			action(view, oldAction);
			expect(selectedGroup(view.describe(native))).toBe(api);
			for (const invalid of ["group:0", "group:not-json", `group:${JSON.stringify([data.identity.codebaseId, "missing"])}`, `group:${JSON.stringify(["foreign", api])}`]) {
				action(view, invalid);
				expect(selectedGroup(view.describe(native))).toBe(api);
			}
			view.update(popupState({ ...changed, nodes: [...changed.nodes].reverse() }, { targets: ["src/api/a.ts"] }));
			expect(selectedGroup(view.describe({ ...native, reduceMotion: true }))).toBe(api);
			const remaining = { ...changed, nodes: changed.nodes.filter(node => node.id !== "0"), roots: [] };
			view.update(popupState(remaining));
			action(view, oldAction);
			expect(selectedGroup(view.describe(native))).toBeUndefined();
			view.update(popupState({ ...data, identity: { ...data.identity, codebaseId: "foreign" } }));
			action(view, oldAction);
			expect(selectedGroup(view.describe(native))).toBeUndefined();
		} finally { view.dispose(); }
	});
	test("keeps observed zero counts qualified during loading and partial maps with bounded selection tooltips", () => {
		for (const loading of [false, true]) {
			const data = { ...overview([`${"long-directory-".repeat(80)}/a.ts`]), loading, truncated: true, caveats: ["Import layer is incomplete", "x".repeat(4_000)] };
			const view = popup(popupState(data));
			try {
				const initial = view.describe(native);
				action(view, dots(initial)[0].p.actions!.click!);
				const selected = view.describe(native), inspector = inspection(selected);
				expectLinks(selected, 0, 0, 0);
				const evidence = tree(inspector).find(node => node.p?.role === "semctl.live.links")?.p?.aria;
				expect(evidence).toMatch(loading ? /loading/i : /partial/i);
				expect(evidence).toContain(data.caveats[0]);
				expect(content(inspector)).not.toMatch(/isolated|no links|no relationships/i);
				expect(neighbors(selected)).toHaveLength(0);
				for (const node of tree(inspector)) if (node.p?.title) expect(node.p.title.length).toBeLessThanOrEqual(720);
			} finally { view.dispose(); }
		}
	});
	test("preserves directed inspection and neighboring navigation in unstyled and plain terminal fallbacks", () => {
		const data = overview(["src/api/a.ts", "src/db/b.ts"]);
		data.edges = [{ from: "0", to: "1", label: "imports" }, { from: "0", to: "1", label: "imports" }];
		for (const supports of [native.supports, () => false]) {
			const view = popup(popupState(data)), fallback = { ...native, supports, feature: () => false };
			try {
				const initial = view.describe(fallback);
				const groups = tree(initial).filter(node => node.p?.role === "semctl.live.group");
				if (!groups.length) view.handleNativeEvent({ type: "toggle", key: "groups", collapsed: false });
				const rows = tree(view.describe(fallback)).filter(node => node.p?.role === "semctl.live.group");
				const api = projectSummary(data, data.identity.codebaseId).groups.find(group => group.members.some(node => node.id === "0"))!;
				action(view, rows.find(node => node.p?.title?.startsWith(api.label))!.p!.actions!.click!);
				const selected = view.describe(fallback);
				expectLinks(selected, 0, 2, 0);
				expect(neighbors(selected)).toHaveLength(1);
				expect(view.render(120).join("\n")).toContain(api.label);
				expect(view.render(120).join("\n")).toContain("2 out");
				action(view, neighbors(selected)[0].p!.actions!.click!);
				expectLinks(view.describe(fallback), 2, 0, 0);
				action(view, "clear-group");
				expect(neighbors(view.describe(fallback))).toHaveLength(0);
			} finally { view.dispose(); }
		}
	});
});

describe("compact live popup observations", () => {
	test("maps exact observations to groups while active requests supersede older receipts", () => {
		const data = overview(["src/api/a.ts", "src/api/b.ts", "src/db/c.ts", "src/cache/d.ts"]);
		const summary = projectSummary(data, data.identity.codebaseId);
		const view = popup(popupState(data, {
			operations: ["call_graph"], targets: ["src/api/a.ts", "src/cache/d.ts.extra"],
			recent: [
				{ id: 8, operation: "call_graph", outcome: "failed", targets: ["src/api/b.ts"] },
				{ id: 9, operation: "imports", outcome: "returned", targets: ["src/db/c.ts"] },
				{ id: 3, operation: "find_definition", outcome: "returned", targets: ["src/api/a.ts"] },
			],
		}));
		try {
			const rendered = dots(view.describe(native));
			const api = rendered.find(node => node.p?.attrs?.["data-node"] === summary.groupOf.get("0"))!;
			const db = rendered.find(node => node.p?.attrs?.["data-node"] === summary.groupOf.get("2"))!;
			const cache = rendered.find(node => node.p?.attrs?.["data-node"] === summary.groupOf.get("3"))!;
			expect(api.p?.aria).toContain("active");
			expect(api.p?.aria).not.toMatch(/failed|returned/);
			expect(db.p?.aria).toContain("returned");
			expect(db.p?.aria).not.toContain("active");
			expect(cache.p?.aria).not.toMatch(/active|returned|failed/);
			view.update(popupState(data, { operations: ["call_graph"], targets: ["src/api/a.ts"] }));
			const expired = dots(view.describe(native)).find(node => node.p?.attrs?.["data-node"] === summary.groupOf.get("0"))!;
			expect(expired.p?.aria).toContain("active");
			expect(expired.p?.aria).not.toMatch(/returned|failed/);
		} finally { view.dispose(); }
	});
	test("shows folded catalog failures while collapsed and preserves them after receipt expiry and refresh failure", () => {
		const data = overview(Array.from({ length: 24 }, (_, index) => `module-${index}/file.ts`));
		const folded = projectSummary(data).groups.find(group => group.id === "other-groups")!;
		const failedId = folded.members[0].id, pendingId = folded.members[1].id;
		const display: Overview = { ...data, nodes: data.nodes.map(node => node.id === failedId ? { ...node, indexing: "failed", indexingError: "Provider rejected embedding" }
			: node.id === pendingId ? { ...node, indexing: "pending" } : node) };
		const view = popup(popupState(display, { recent: [{ id: 1, operation: "read_source", outcome: "returned", targets: [folded.members[0].path!] }] }));
		try {
			const initial = view.describe(native), collapsed = tree(initial).find(node => node.k === "section" && node.key === "groups");
			if (!collapsed || collapsed.k !== "section") throw new Error("Missing groups disclosure");
			expect(collapsed.p?.collapsed).toBe(true);
			const failed = dots(initial).find(node => node.p.attrs?.["data-node"] === "other-groups")!;
			expect(failed.p.class).toContain("sg-indexing-failed");
			expect(failed.p.aria).toContain("1 failed");
			expect(failed.p.aria).toContain("1 pending");
			expect(failed.p.aria).toContain("recently returned");
			expect(dots(initial).filter(node => node.p.class?.includes("sg-indexing-"))).toHaveLength(1);
			action(view, failed.p.actions!.click!);
			const chosen = view.describe(native);
			expect(content(chosen)).toContain("1 failed · 1 pending");
			expect(tree(chosen).filter(node => node.k === "text" && node.p?.title?.includes("1 failed · 1 pending")).length).toBeGreaterThan(0);
			view.update({ ...popupState(display), request: { loading: true, text: "", display, error: "Temporary map refresh failure" } });
			const retained = dots(view.describe({ ...native, reduceMotion: true })).find(node => node.p.attrs?.["data-node"] === "other-groups")!;
			expect(retained.p.class).toContain("sg-indexing-failed");
			expect(retained.p.aria).not.toContain("recently");
			expect(retained.p.attrs?.["aria-pressed"]).toBe(true);
			view.handleNativeEvent({ type: "toggle", key: "groups", collapsed: false });
			const list = tree(view.describe(native)).find(node => node.p?.role === "semctl.live.group" && node.p?.actions?.click === failed.p.actions?.click);
			expect(list?.p?.title).toContain("1 failed · 1 pending");
			view.update(popupState(data));
			expect(dots(view.describe(native)).some(node => node.p.class?.includes("sg-indexing-"))).toBe(false);
		} finally { view.dispose(); }
	});

	test("never paints aggregate producer failures onto unknown or legacy files", () => {
		const data = overview(["src/api/a.ts", "src/db/b.ts", "src/cache/c.ts"]);
		data.nodes[0].indexing = "unknown"; data.nodes[1].indexing = "ready";
		const view = popup(popupState(data, { producer: { text: "Embedding failed · 0/2", tone: "error", progress: 0, details: "Aggregate job failure" } }));
		try {
			const rendered = view.describe(native);
			expect(dots(rendered)).toHaveLength(3);
			expect(dots(rendered).some(node => node.p.class?.includes("sg-indexing-") || node.p.class?.includes("sg-active"))).toBe(false);
			expect(content(rendered)).toContain("1 ready · 1 unknown · 1 unreported");
			expect(tree(rendered).find(node => node.k === "meter")?.p?.value).toBe(0);
		} finally { view.dispose(); }
	});
	test("renders available graph and group actions while typed relationship layers load", () => {
		const data: Overview = { ...overview(["src/api/a.ts", "src/db/b.ts"]), loading: true, truncated: true, caveats: ["Import layer is loading"] };
		const view = popup(popupState(data));
		try {
			const rendered = view.describe(native);
			expect(dots(rendered)).toHaveLength(2);
			expect(tree(rendered).some(node => node.p?.title?.includes(data.caveats[0]))).toBe(true);
			expect(dots(rendered).some(node => node.p.class?.includes("sg-active") || node.p.class?.includes("sg-indexing-"))).toBe(false);
			action(view, dots(rendered)[0].p.actions!.click!);
			expect(dots(view.describe(native)).some(node => node.p.attrs?.["aria-pressed"] === true)).toBe(true);
			view.update(popupState({ ...data, loading: false, truncated: false, caveats: [] }));
			expect(dots(view.describe(native))).toHaveLength(2);
		} finally { view.dispose(); }
	});

	test("rejects foreign-codebase hits and partial targets while mapping receipts into folded groups", () => {
		const data = overview(Array.from({ length: 24 }, (_, index) => `module-${index}/file.ts`));
		data.nodes.push({ id: "foreign", label: "Foreign", path: "foreign/a.ts", category: "file", detail: null,
			hit: { codebaseId: "different-codebase", path: "foreign/a.ts" } as NonNullable<Overview["nodes"][number]["hit"]> });
		const summary = projectSummary(data, data.identity.codebaseId);
		const folded = summary.groups.find(group => group.id === "other-groups")!;
		const retained = summary.groups.find(group => group.id !== "other-groups" && !group.members.some(node => node.id === "foreign"))!;
		const target = folded.members.find(node => node.id !== "foreign")!.path!;
		const view = popup(popupState(data, {
			targets: ["foreign/a.ts", `${retained.members[0].path}.extra`],
			recent: [
				{ id: 1, operation: "read_source", outcome: "returned", targets: [target] },
				{ id: 2, operation: "read_source", outcome: "failed", targets: ["foreign/a.ts", `${retained.members[0].path}.extra`] },
			],
		}));
		try {
			const rendered = dots(view.describe(native));
			expect(rendered.some(node => node.p?.aria?.includes(", active"))).toBe(false);
			expect(rendered.some(node => node.p?.aria?.includes("failed"))).toBe(false);
			expect(rendered.find(node => node.p?.attrs?.["data-node"] === "other-groups")?.p?.aria).toContain("returned");
			expect(rendered.filter(node => node.p?.aria?.includes("returned"))).toHaveLength(1);
		} finally { view.dispose(); }
	});

	test("retains selected groups and disclosure state across observations and resets missing or foreign selections", () => {
		const data = overview(["src/api/a.ts", "src/db/b.ts"]), view = popup(popupState(data));
		try {
			const initial = view.describe(native), first = dots(initial)[0], group = first.p?.attrs?.["data-node"];
			action(view, first.p!.actions!.click!);
			view.handleNativeEvent({ type: "toggle", key: "groups", collapsed: false });
			view.update(popupState({ ...data, nodes: [...data.nodes] }, { requests: { sent: 3, received: 2, failed: 0 } }));
			const retained = view.describe(native);
			expect(dots(retained).find(node => node.p?.attrs?.["data-node"] === group)?.p?.attrs?.["aria-pressed"]).toBe(true);
			expect(tree(retained).find(node => node.k === "section")?.p).toMatchObject({ collapsed: false });
			const chosen = projectSummary(data, data.identity.codebaseId).groups.find(candidate => candidate.id === group)!;
			const remaining = { ...data, nodes: data.nodes.filter(node => !chosen.members.some(member => member.id === node.id)), roots: [] };
			view.update(popupState(remaining));
			expect(dots(view.describe(native)).some(node => node.p?.attrs?.["aria-pressed"] === true)).toBe(false);
			const replacement = dots(view.describe(native))[0];
			action(view, replacement.p!.actions!.click!);
			expect(dots(view.describe(native)).some(node => node.p?.attrs?.["aria-pressed"] === true)).toBe(true);
			view.update(popupState({ ...remaining, identity: { ...data.identity, codebaseId: "other-checkout" } }));
			expect(dots(view.describe(native)).some(node => node.p?.attrs?.["aria-pressed"] === true)).toBe(false);
			expect(tree(view.describe(native)).find(node => node.k === "section")?.p).toMatchObject({ collapsed: false });
		} finally { view.dispose(); }
	});

	test("keeps group selection usable without graph or disclosure capabilities", () => {
		const data = overview(["src/api/a.ts"]), view = popup(popupState(data));
		try {
			const fallback = { ...native, supports: () => false }, body = view.describe(fallback);
			expect(dots(body)).toHaveLength(0);
			const group = tree(body).find(node => node.p?.actions?.click?.startsWith("group:"));
			expect(group).toBeDefined();
			action(view, group!.p!.actions!.click!);
			const selected = view.describe(fallback);
			const clear = tree(selected).find(node => node.p?.actions?.click === "clear-group");
			expect(clear).toBeDefined();
			expect(tree(selected).some(node => node.p?.title?.includes("src/api/a.ts"))).toBe(true);
			action(view, clear!.p!.actions!.click!);
			expect(tree(view.describe(fallback)).some(node => node.p?.actions?.click === "clear-group")).toBe(false);
		} finally { view.dispose(); }
	});

	test("preserves zero producer progress, reduced motion and visible failures without exposing diagnostic prose", () => {
		const data = overview(["src/a.ts"]), view = popup(popupState(data));
		try {
			view.update(popupState(data, {
				operations: ["search_codebase"], targets: ["src/a.ts"],
				producer: { text: "Embedding · queued", tone: "muted", progress: 0, details: "Producer job 17; aggregate embedding observation" },
				requests: { sent: 2, received: 0, failed: 1 },
			}));
			const active = view.describe({ ...native, reduceMotion: true });
			expect(tree(active).find(node => node.k === "meter")?.p).toMatchObject({ value: 0 });
			expect(tree(active).find(node => node.key === "graph-preview")?.p).toMatchObject({ attrs: { "data-motion": "reduced" } });
			view.update({ ...popupState(data, { requests: { sent: 2, received: 1, failed: 1 } }),
				request: { loading: false, text: "", display: data, error: "Private backend diagnostic: connection refused" } });
			const failed = view.describe(native);
			expect(tree(failed).map(asNativeElement).some(node => node?.p.attrs?.["data-alert"] === true)).toBe(true);
			expect(content(failed)).not.toContain("Private backend diagnostic");
			expect(tree(failed).some(node => node.k === "text" && node.p?.title?.includes("Private backend diagnostic"))).toBe(true);
			expect(tree(failed).find(node => node.k === "meter")?.p?.value).toBeNull();
		} finally { view.dispose(); }
	});
	test("appearance changes preserve visible selection and clear a group folded by a lower detail level", () => {
		const data = overview(Array.from({ length: 32 }, (_, index) => `module-${index}/file.ts`));
		const state = { ...popupState(data), appearance: { ...DEFAULT_GRAPH_APPEARANCE, detail: "detailed" as const } };
		const view = popup(state);
		try {
			const initial = dots(view.describe(native));
			expect(initial).toHaveLength(24);
			const selected = initial.filter(node => node.p.attrs?.["data-node"] !== "other-groups").at(-1)!;
			action(view, selected.p.actions!.click!);
			view.update({ ...state, appearance: { ...state.appearance, layout: "radial", color: "structure", motion: "off" } });
			const changed = view.describe(native);
			expect(dots(changed).find(node => node.p.attrs?.["aria-pressed"] === true)?.p.attrs?.["data-node"]).toBe(selected.p.attrs?.["data-node"]);
			view.update({ ...state, appearance: { ...state.appearance, detail: "calm" } });
			const folded = dots(view.describe(native));
			expect(folded).toHaveLength(12);
			expect(folded.some(node => node.p.attrs?.["data-node"] === "other-groups")).toBe(true);
			expect(folded.some(node => node.p.attrs?.["aria-pressed"] === true)).toBe(false);
			action(view, folded[0].p.actions!.click!);
			expect(dots(view.describe(native)).some(node => node.p.attrs?.["aria-pressed"] === true)).toBe(true);
		} finally { view.dispose(); }
	});
});
