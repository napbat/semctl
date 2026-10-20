import { describe, expect, test } from "bun:test";
import type { DescribeContext, NativeNode } from "@oh-my-pi/pi-tui";
import { LiveGraphPopup, livePopupOverlay, type LiveGraphState } from "./live-graph";
import type { GraphActivity } from "./graph-activity";
import type { Display } from "./inspector";
import { graphWindow, projectSummary } from "./graph-projection";
import { asNativeElement, type NativeElement } from "./native-elements";
import { groupIndexing, indexingCounts, indexingParts, indexingSummary } from "./graph-indexing";
import { DEFAULT_GRAPH_APPEARANCE, type GraphAppearance } from "./graph-appearance";
import { nativeGraphLayout } from "./graph-native-layout";

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
	return new LiveGraphPopup(state, () => {}, { close() {}, refresh() {}, expand() {}, appearance() {}, resize() {} });
}
/** Every hover line stays short: at most 3 lines of at most 88 characters. */
function expectCompactTitles(root: NativeNode): void {
	for (const node of tree(root)) {
		const lines = node.p?.title?.split("\n") ?? [];
		expect(lines.length).toBeLessThanOrEqual(3);
		for (const line of lines) expect(line.length).toBeLessThanOrEqual(88);
	}
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
		const view = new LiveGraphPopup(popupState(data), () => {}, { close() {}, refresh() {}, expand() { opened++; }, appearance() {}, resize() {} });
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
	test("fills the empty inspector with a short glyph legend that names a tint only when the drawing has tinted areas", () => {
		const names = ["cache-entry-store", "cache-entry-read", "cache-entry-evict", "cache-entry-write",
			"query-execution-plan", "query-execution-run", "query-execution-parse", "query-execution-explain"];
		const data = overview(names.flatMap(name => [`src/${name}/index.ts`, `src/${name}/${name}.ts`])), view = popup(popupState(data));
		const pair = overview(["src/api/a.ts", "src/db/c.ts"]), quiet = popup(popupState(pair));
		const summary = projectSummary(data, data.identity.codebaseId), id = summary.groupOf.get("0")!;
		const legendOf = (root: NativeNode) => tree(inspection(root)).filter(node => node.p?.role === "semctl.live.legend");
		try {
			expect(DEFAULT_GRAPH_APPEARANCE.layout).toBe("map");
			expect(nativeGraphLayout(summary.graph, "compact", "map").territories.map(territory => territory.kind)).toContain("lexical");
			const initial = view.describe(native), legend = legendOf(initial);
			expect(legend).toHaveLength(1);
			expect((legend[0].c ?? []).map(child => "k" in child ? [child.key, asNativeElement(child)?.p.title] : [])).toEqual([
				["size", "Dot size = member nodes, mostly files"], ["weight", "Line weight = links between groups"], ["tint", "Tint = shared identifier terms"]]);
			const parts = tree(legend[0]).map(asNativeElement).filter((node): node is NativeElement => !!node);
			const glyphs = parts.filter(node => /^semctl-legend-(size|weight|tint)$/.test(node.p.class ?? ""));
			expect(glyphs.map(node => [node.p.class, node.p.attrs?.["aria-hidden"]])).toEqual([["semctl-legend-size", true], ["semctl-legend-weight", true], ["semctl-legend-tint", true]]);
			expect(parts.flatMap(node => node.p.text ?? [])).toEqual(["files", "links", "shared terms"]);
			expect(legend[0].p?.title?.split("\n")).toEqual(["Dot size = member nodes · line weight = links between groups", "Tint = shared identifier terms"]);
			expect(legend[0].p?.title).not.toMatch(/embedding/i);
			expect(content(inspection(initial))).toContain("Select a group to inspect links");
			// A host without styles draws no map, so its plain legend names no tint.
			const plain = legendOf(view.describe({ ...native, feature: () => false }));
			expect(plain.map(node => [node.k, node.p?.text, node.p?.tone])).toEqual([["text", "Dot size = files · line weight = links", "muted"]]);
			expect(plain[0].p?.title?.split("\n")).toHaveLength(1);
			view.update({ ...popupState(data), appearance: { ...DEFAULT_GRAPH_APPEARANCE, layout: "force" } });
			expect(tree(legendOf(view.describe(native))[0]).flatMap(node => asNativeElement(node)?.p.text ?? [])).toEqual(["files", "links", "shared terms"]);
			view.update({ ...popupState(data), appearance: { ...DEFAULT_GRAPH_APPEARANCE, layout: "radial" } });
			const radial = legendOf(view.describe(native))[0];
			expect(tree(radial).flatMap(node => asNativeElement(node)?.p.text ?? [])).toEqual(["files", "links"]);
			expect(radial.p?.title).not.toContain("Tint");
			expect(tree(legendOf(quiet.describe(native))[0]).flatMap(node => asNativeElement(node)?.p.text ?? [])).toEqual(["files", "links"]);
			view.update(popupState(data));
			action(view, dots(view.describe(native)).find(node => node.p.attrs?.["data-node"] === id)!.p.actions!.click!);
			const selected = view.describe(native);
			expect(tree(inspection(selected)).some(node => node.p?.role === "semctl.live.legend")).toBe(false);
			expect(tree(inspection(selected)).some(node => node.p?.role === "semctl.live.links")).toBe(true);
		} finally { view.dispose(); quiet.dispose(); }
	});
	test("fills the idle third inspector row with the busiest groups as short selectable chips", () => {
		const data = overview(["src/api/a.ts", "src/db/b.ts", "src/cache/c.ts", "src/web/d.ts", "src/cli/e.ts"]);
		data.edges = [["0", "1"], ["0", "2"], ["0", "3"], ["1", "2"], ["3", "4"], ["4", "4"]].map(([from, to]) => ({ from, to, label: "imports" }));
		const groupOf = projectSummary(data, data.identity.codebaseId).groupOf;
		const names = new Map([[groupOf.get("0")!, "api"], [groupOf.get("1")!, "db"], [groupOf.get("2")!, "cache"], [groupOf.get("3")!, "web"]]);
		// api has 3 cross-group links; db, cache and web tie at 2 and order by id; cli has 1.
		const expected = [groupOf.get("0")!, ...[groupOf.get("1")!, groupOf.get("2")!, groupOf.get("3")!].sort()].slice(0, 3);
		const view = popup(popupState(data)), quiet = popup(popupState(overview(["src/api/a.ts", "src/db/b.ts"])));
		try {
			const row = inspection(view.describe(native)).c?.[2] as NativeNode;
			expect(row.key).toBe("neighbors");
			expect(row.c?.[0]).toMatchObject({ k: "text", key: "busiest-label", p: { text: "Busiest:", tone: "muted" } });
			const chips = tree(row).filter(node => node.p?.role === "semctl.live.busiest");
			expect(chips.map(node => [node.key, asNativeElement(node)?.p.class])).toEqual(expected.map(id => [`busiest-${id}`, "semctl-live-neighbor"]));
			expect(chips.map(chip => tree(chip).filter(node => node.k === "text").map(node => node.p?.text))).toEqual(expected.map(id => [names.get(id)]));
			expect(chips.every(node => node.p?.grow === 1 && node.p?.basis === 0)).toBe(true);
			expect(chips[0].p?.title).toContain("src/api");
			expect(chips[0].p?.title).toContain("3 cross-group links · 0 incoming · 3 outgoing");
			expect(chips[0].p?.aria).toContain("3 cross-group links · 0 incoming links · 3 outgoing links");
			action(view, chips[1].p!.actions!.click!);
			const selected = view.describe(native);
			expect(selectedGroup(selected)).toBe(expected[1]);
			expect(tree(inspection(selected)).some(node => node.p?.role === "semctl.live.busiest")).toBe(false);
			const api = neighbors(selected).find(node => node.p?.actions?.click === chips[0].p?.actions?.click)!;
			expect(tree(api).filter(node => node.k === "text").map(node => node.p?.text)).toEqual(["src/api", "1"]);
			action(view, "clear-group");
			const fallback = tree(inspection(view.describe({ ...native, feature: () => false }))).filter(node => node.p?.role === "semctl.live.busiest");
			expect(fallback.map(node => [node.k, node.key])).toEqual(expected.map(id => ["row", `busiest-${id}`]));
			expect(content(inspection(quiet.describe(native)).c?.[2] as NativeNode)).toContain("No cross-group links in this map");
			// Wider popups show more busiest chips; the ranking stays the same.
			const ranked = [groupOf.get("0")!, ...[groupOf.get("1")!, groupOf.get("2")!, groupOf.get("3")!].sort(), groupOf.get("4")!];
			const busiestAt = (size: GraphAppearance["popup"]) => {
				view.update({ ...popupState(data), appearance: { ...DEFAULT_GRAPH_APPEARANCE, popup: size } });
				return tree(inspection(view.describe(native))).filter(node => node.p?.role === "semctl.live.busiest").map(node => node.key);
			};
			expect(busiestAt("medium")).toEqual(ranked.slice(0, 4).map(id => `busiest-${id}`));
			expect(busiestAt("large")).toEqual(ranked.map(id => `busiest-${id}`));
			expect(busiestAt("small")).toEqual(ranked.slice(0, 3).map(id => `busiest-${id}`));
		} finally { view.dispose(); quiet.dispose(); }
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
			const busiest = tree(initial).filter(node => node.p?.role === "semctl.live.busiest");
			expect(busiest.map(node => node.key)).not.toContain(`busiest-${folded.id}`);
			expect(busiest.map(node => node.key)).toContain(`busiest-${first.id}`);
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
				expectCompactTitles(inspector);
				expectCompactTitles(tree(selected).find(node => node.p?.role === "semctl.live.header")!);
				expect(tree(selected).find(node => node.key === "partial")?.p?.title).toBe(loading ? undefined : "Partial indexed map\nImport layer is incomplete");
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
			expect(tree(chosen).find(node => node.p?.role === "semctl.live.indexing")).toMatchObject({ k: "meter", p: { value: 0 } });
			expect(tree(chosen).find(node => node.p?.role === "semctl.live.indexing-label")?.p?.text).toBe("0% ready");
			expect(tree(chosen).find(node => node.p?.role === "semctl.live.indexing")?.p?.title).toContain("1 failed · 1 pending · 22 unreported");
			expect(tree(chosen).filter(node => node.k === "text" && node.p?.title?.includes("1 failed · 1 pending")).length).toBeGreaterThan(0);
			view.update({ ...popupState(display), request: { loading: true, text: "", display, error: "Temporary map refresh failure" } });
			const retained = dots(view.describe({ ...native, reduceMotion: true })).find(node => node.p.attrs?.["data-node"] === "other-groups")!;
			expect(retained.p.class).toContain("sg-indexing-failed");
			expect(retained.p.aria).not.toContain("recently");
			expect(retained.p.attrs?.["aria-pressed"]).toBe(true);
			view.handleNativeEvent({ type: "toggle", key: "groups", collapsed: false });
			const row = (cx: DescribeContext) => tree(view.describe(cx)).find(node => node.p?.role === "semctl.live.group" && node.p?.actions?.click === failed.p.actions?.click)!;
			expect(row(native).p?.title).toContain("1 failed · 1 pending");
			const meter = tree(row(native)).find(node => node.p?.role === "semctl.live.group.indexing");
			expect(meter).toMatchObject({ k: "meter", p: { style: "bar", size: "sm", min: { w: "6ch" }, max: { w: "6ch" },
				parts: [{ value: 0.5, token: "muted", label: "1 pending" }, { value: 0.5, token: "error", label: "1 failed" }] } });
			expect(meter?.p?.title).toContain("Indexing: 1 failed · 1 pending");
			expect(meter?.p).not.toHaveProperty("label");
			expect(tree(row(native)).some(node => node.p?.role === "semctl.live.indexing-label")).toBe(false);
			expect(tree(row(native)).some(node => node.p?.role === "semctl.live.group.status")).toBe(false);
			const badge = tree(row({ ...native, supports: kind => String(kind) === "el" || kind === "section" })).find(node => node.p?.role === "semctl.live.group.status");
			expect(asNativeElement(badge!)?.p).toMatchObject({ text: "1 failed", class: "semctl-live-badge-error" });
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
			const indexing = tree(rendered).find(node => node.p?.role === "semctl.live.indexing");
			expect(indexing).toMatchObject({ k: "meter", p: { value: 0.5 } });
			expect(tree(rendered).find(node => node.p?.role === "semctl.live.indexing-label")?.p?.text).toBe("50% ready");
			expect(indexing?.p?.title).toContain("1 ready · 1 unknown · 1 unreported");
			expect(tree(rendered).find(node => node.key === "progress")?.p?.value).toBe(0);
		} finally { view.dispose(); }
	});
	test("reads as a titled card with a subline, a ready-share meter and a cached graph well", () => {
		const data: Overview = { ...overview(["src/api/a.ts", "src/api/b.ts", "src/db/c.ts", "src/cache/d.ts"]), truncated: true, caveats: ["upstream edge limit"] };
		data.nodes[0].indexing = "ready"; data.nodes[1].indexing = "ready"; data.nodes[2].indexing = "failed";
		const live: DescribeContext = { ...native, supports: kind => ["el", "section", "meter", "icon"].includes(String(kind)) };
		const view = popup(popupState(data)), single = popup(popupState(overview(["src/a.ts"])));
		try {
			const card = view.describe(live), header = tree(card).find(node => node.p?.role === "semctl.live.header")!;
			expect(tree(header).find(node => node.p?.role === "semctl.live.title")?.p?.text).toBe("Project map");
			const subline = tree(header).find(node => node.p?.role === "semctl.live.subline");
			expect(subline?.p?.text).toBe("4 files · 3 groups");
			expect(subline?.p?.title).toContain("4 indexed nodes · 3 groups");
			expect(tree(header).find(node => node.key === "partial")).toMatchObject({ k: "icon", p: { name: "warn", role: "semctl.live.partial", tone: "warning" } });
			expect(tree(header).find(node => node.key === "partial")?.p?.title).toContain("upstream edge limit");
			expect(tree(single.describe(live)).find(node => node.p?.role === "semctl.live.subline")?.p?.text).toBe("1 file · 1 group");
			// Toolbar: resize grip at the moving top-left corner, the title, then the action buttons.
			const toolbar = tree(header).find(node => node.key === "toolbar")!;
			const [grip, title, buttons] = (toolbar.c ?? []) as NativeNode[];
			expect(asNativeElement(grip)?.p).toMatchObject({ tag: "button", class: "semctl-icon-action", role: "semctl.live.resize", actions: { click: "resize" },
				title: "Popup size: Small\nClick to enlarge · Shift+click for large" });
			expect(grip.c?.[0]).toMatchObject({ k: "icon", p: { name: "grip" } });
			expect(title).toMatchObject({ k: "text", p: { role: "semctl.live.title", text: "Project map", actions: { dblclick: "resize:toggle" } } });
			expect((buttons.c ?? []).map(child => "k" in child ? child.key : undefined)).toEqual(["expand", "appearance", "refresh", "close"]);
			expectCompactTitles(header);
			const row = tree(card).find(node => node.p?.role === "semctl.live.indexing-row");
			expect(row).toMatchObject({ k: "row", key: "indexing-row", p: { gap: "sm", align: "center" } });
			const [meter, share] = (row?.c ?? []) as NativeNode[];
			expect(meter).toMatchObject({ k: "meter", key: "indexing", p: { role: "semctl.live.indexing", style: "bar", size: "sm", grow: 1, min: { w: 0 } } });
			expect(meter.p).not.toHaveProperty("label");
			expect(meter.p?.parts).toEqual(indexingParts(indexingCounts(data.nodes)));
			expect(meter.p?.title).toContain("Indexing: 1 failed · 2 ready · 1 unreported");
			expect(share).toMatchObject({ k: "text", p: { text: "66% ready", role: "semctl.live.indexing-label", tone: "muted", shrink: 0, title: meter.p?.title } });
			expect(row?.c).toHaveLength(2);
			const well = tree(card).find(node => asNativeElement(node)?.p.class === "semctl-graph-well");
			expect(well?.key).toBe("graph-well");
			expect(well?.c?.map(child => "k" in child ? child.key : undefined)).toEqual(["graph-preview"]);
			expect(tree(view.describe(live)).find(node => node.key === "graph-well")).toBe(well);
			const plain = view.describe({ ...native, supports: kind => kind === "section", feature: () => false });
			expect(tree(plain).find(node => node.p?.role === "semctl.live.indexing")).toMatchObject({ k: "text", p: { text: "1 failed · 2 ready · 1 unreported" } });
			expect(tree(plain).some(node => node.key === "graph-well" || node.k === "meter" || node.p?.role === "semctl.live.indexing-row")).toBe(false);
			expect(tree(plain).find(node => node.key === "partial")).toMatchObject({ k: "text", p: { text: "Partial map", tone: "warning" } });
			expect(tree(plain).find(node => node.p?.role === "semctl.live.subline")?.p?.text).toBe("4 files · 3 groups");
		} finally { view.dispose(); single.dispose(); }
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
			const label = projectSummary(data, data.identity.codebaseId).groups[0].label;
			expect(tree(selected).some(node => node.p?.title?.startsWith(`${label} · 1 file`))).toBe(true);
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
			const warning = tree(failed).find(node => node.p?.role === "semctl.live.warning");
			expect(warning?.p?.text).toBe("!");
			expect(tree(tree(failed).find(node => node.key === "status-slot")!).some(node => node === warning)).toBe(false);
			expect(content(failed)).not.toContain("Private backend diagnostic");
			expect(tree(failed).some(node => node.k === "text" && node.p?.title?.includes("Private backend diagnostic"))).toBe(true);
			expect(tree(failed).find(node => node.k === "meter")?.p?.value).toBeNull();
		} finally { view.dispose(); }
	});
	test("loading states use native spinner and shimmer when supported and keep static fallbacks", () => {
		const loading: LiveGraphState = { request: { loading: true, text: "" }, activity: { operations: [], targets: [], producer: null } };
		const live: DescribeContext = { ...native, supports: kind => ["el", "section", "meter", "icon", "spinner", "shimmer"].includes(String(kind)) };
		const empty = (root: NativeNode) => tree(root).find(node => node.key === "empty")!;
		const view = popup(loading);
		try {
			const animated = view.describe(live);
			expect(tree(empty(animated)).find(node => node.k === "spinner")?.p).toMatchObject({ style: "dots" });
			expect(tree(empty(animated)).find(node => node.k === "shimmer")?.p).toMatchObject({ text: "Reading the indexed map" });
			expect(tree(animated).find(node => node.p?.role === "semctl.live.title")?.p?.text).toBe("Project map");
			expect(tree(animated).some(node => node.p?.role === "semctl.live.subline")).toBe(false);
			const refresh = asNativeElement(tree(animated).find(node => node.key === "refresh")!)!;
			expect(refresh.p.actions).toBeUndefined();
			expect(refresh.p.attrs?.["aria-disabled"]).toBe(true);
			expect(refresh.c?.[0]).toMatchObject({ k: "spinner", p: { style: "orbit", aria: "Refreshing" } });
			for (const still of [view.describe({ ...live, reduceMotion: true }), view.describe(native)]) {
				expect(tree(still).some(node => node.k === "spinner" || node.k === "shimmer")).toBe(false);
				expect(content(empty(still))).toContain("Reading the indexed map");
			}
			view.update({ ...loading, appearance: { ...DEFAULT_GRAPH_APPEARANCE, motion: "off" } });
			expect(tree(view.describe(live)).some(node => node.k === "spinner" || node.k === "shimmer")).toBe(false);
			view.update({ ...loading, request: { loading: false, text: "", error: "offline" } });
			const failed = view.describe(live);
			expect(tree(failed).some(node => node.k === "spinner" || node.k === "shimmer")).toBe(false);
			expect(content(empty(failed))).toContain("The indexed map is unavailable");
			expect(tree(tree(failed).find(node => node.key === "refresh")!).find(node => node.k === "icon")?.p?.name).toBe("repeat");
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

describe("resizable live popup", () => {
	test("the header grip steps sizes with wrap, Shift+click steps back and a title double-click toggles small and large", () => {
		const data = overview(["src/api/a.ts", "src/db/b.ts"]), requested: GraphAppearance["popup"][] = [];
		let state: LiveGraphState = popupState(data);
		const view: LiveGraphPopup = new LiveGraphPopup(state, () => {}, { close() {}, refresh() {}, expand() {}, appearance() {}, resize(size) {
			// The host persists the size through the appearance store and passes the new value back.
			requested.push(size);
			state = { ...state, appearance: { ...DEFAULT_GRAPH_APPEARANCE, popup: size } };
			view.update(state);
		} });
		const click = (act: string, mods: string[] = []) => view.handleNativeEvent({ type: "action", key: "", act, mods });
		const grip = () => asNativeElement(tree(view.describe(native)).find(node => node.key === "resize")!)!;
		try {
			expect(grip().p.title).toBe("Popup size: Small\nClick to enlarge · Shift+click for large");
			click("resize"); click("resize");
			expect(grip().p.title).toBe("Popup size: Large\nClick for small · Shift+click to shrink");
			click("resize");
			click("resize", ["shift"]); click("resize", ["shift"]);
			expect(grip().p.title).toBe("Popup size: Medium\nClick to enlarge · Shift+click to shrink");
			click("resize", ["shift"]); click("resize", ["shift", "ctrl"]);
			click("resize:toggle"); click("resize:toggle");
			expect(requested).toEqual(["medium", "large", "small", "large", "medium", "small", "large", "small", "large"]);
			expectCompactTitles(tree(view.describe(native)).find(node => node.p?.role === "semctl.live.header")!);
		} finally { view.dispose(); }
	});
	test("each size maps to a Tern overlay size and a preview size without moving dots within a preview size", () => {
		expect((["small", "medium", "large"] as const).map(size => livePopupOverlay(size, false).size)).toEqual(["sm", "md", "lg"]);
		expect(livePopupOverlay("medium", true)).toEqual({ role: "omp.overlay.semctl-live", modal: false, size: "md", anchor: "top", hidden: true, min: { w: 0 } });
		const data = overview(["src/api/a.ts", "src/api/b.ts", "src/db/c.ts", "src/cache/d.ts", "src/web/e.ts"]);
		data.edges = [{ from: "0", to: "2", label: "imports" }, { from: "2", to: "3", label: "imports" }, { from: "3", to: "4", label: "imports" }];
		const view = popup(popupState(data));
		const drawing = (size: GraphAppearance["popup"]) => {
			view.update({ ...popupState(data), appearance: { ...DEFAULT_GRAPH_APPEARANCE, popup: size } });
			const root = view.describe(native), preview = asNativeElement(tree(root).find(node => node.key === "graph-preview")!)!;
			return { preview: preview.p.class?.match(/\bsg-(compact|inline|overview)\b/)?.[1],
				positions: dots(root).map(dot => [dot.p.attrs?.["data-node"], dot.p.class?.match(/sg-x\d+ sg-y\d+ sg-fx\d+ sg-fy\d+/)?.[0]]) };
		};
		try {
			const small = drawing("small");
			expect(small.preview).toBe("compact");
			expect(drawing("medium").preview).toBe("inline");
			expect(drawing("large").preview).toBe("overview");
			expect(drawing("small")).toEqual(small);
		} finally { view.dispose(); }
	});
	test("group hovers name the group, its file count and catalog state in at most three short lines", () => {
		const data = overview(["src/api/a.ts", "src/api/b.ts", `src/${"deep-".repeat(30)}/c.ts`]);
		data.nodes[0].indexing = "failed";
		const view = popup(popupState(data));
		try {
			view.handleNativeEvent({ type: "toggle", key: "groups", collapsed: false });
			const rows = tree(view.describe(native)).filter(node => node.p?.role === "semctl.live.group");
			const api = projectSummary(data, data.identity.codebaseId).groups.find(group => group.members.some(node => node.id === "0"))!;
			expect(rows.find(node => node.p?.title?.startsWith(api.label))?.p?.title?.split("\n")).toEqual([`${api.label} · 2 files`, indexingSummary(api.indexing)]);
			expect(rows).toHaveLength(2);
			for (const row of rows) expectCompactTitles(row);
		} finally { view.dispose(); }
	});
});

test("compact failure hovers keep the full diagnostic reachable with and without a retained map", () => {
	const error = `Overview failed: ${"request context ".repeat(12)}\nServer details\nRetry details\nActionable cause on line four`;
	for (const display of [undefined, overview(["src/api/a.ts"])]) {
		const view = popup({ request: { loading: false, text: "", error, display }, activity: { operations: [], targets: [], producer: null } });
		try {
			const closed = view.describe(native);
			expect(tree(closed).some(node => node.p?.role === "semctl.live.diagnostic-text")).toBe(false);
			const section = tree(closed).find(node => node.p?.role === "semctl.live.diagnostics");
			if (!section?.key) throw new Error("Missing diagnostic disclosure");
			view.handleNativeEvent({ type: "toggle", key: section.key, collapsed: false });
			expect(tree(view.describe(native)).find(node => node.p?.role === "semctl.live.diagnostic-text")?.p?.text).toBe(error);
			const fallback = view.describe({ ...native, supports: kind => kind !== "section" && native.supports(kind) });
			expect(content(fallback)).toContain("Actionable cause on line four");
		} finally { view.dispose(); }
	}
});
