import { describe, expect, test } from "bun:test";
import type { DescribeContext, NativeNode } from "@oh-my-pi/pi-tui";
import { LiveGraphPopup, type LiveGraphState } from "./live-graph";
import type { GraphActivity } from "./graph-activity";
import type { Display } from "./inspector";
import { graphWindow, projectSummary } from "./graph-projection";

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
});

const native: DescribeContext = {
	cols: 64, dark: true, reduceMotion: false, stylesheets: true,
	supports: kind => kind === "el" || kind === "section" || kind === "meter", feature: name => name === "styles",
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
	return tree(root).filter((node): node is Extract<NativeNode, { k: "el" }> => node.k === "el" && typeof node.p?.attrs?.["data-node"] === "string");
}
function popupState(display: Overview, activity: Partial<GraphActivity> = {}): LiveGraphState {
	return { request: { loading: false, text: "", display }, activity: { operations: [], targets: [], producer: null, ...activity } };
}
function popup(state: LiveGraphState) {
	return new LiveGraphPopup(state, () => {}, { close() {}, refresh() {}, expand() {} });
}
function action(view: LiveGraphPopup, act: string): void {
	view.handleNativeEvent({ type: "action", key: "", act, mods: [] });
}

describe("compact live popup observations", () => {
	test("maps exact active and newest returned or failed receipts independently to displayed groups", () => {
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
			expect(api.p?.aria).toContain("failed");
			expect(api.p?.aria).not.toContain("returned");
			expect(db.p?.aria).toContain("returned");
			expect(db.p?.aria).not.toContain("active");
			expect(cache.p?.aria).not.toMatch(/active|returned|failed/);
			view.update(popupState(data, { operations: ["call_graph"], targets: ["src/api/a.ts"] }));
			const expired = dots(view.describe(native)).find(node => node.p?.attrs?.["data-node"] === summary.groupOf.get("0"))!;
			expect(expired.p?.aria).toContain("active");
			expect(expired.p?.aria).not.toMatch(/returned|failed/);
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
			const group = tree(body).find(node => node.k === "text" && node.p?.actions?.click?.startsWith("group:"));
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
			expect(tree(active).some(node => node.k === "text" && node.p?.title?.includes("Producer job 17"))).toBe(true);
			view.update({ ...popupState(data, { requests: { sent: 2, received: 1, failed: 1 } }),
				request: { loading: false, text: "", display: data, error: "Private backend diagnostic: connection refused" } });
			const failed = view.describe(native);
			expect(tree(failed).some(node => node.k === "text" && node.p?.tone === "error")).toBe(true);
			expect(content(failed)).not.toContain("Private backend diagnostic");
			expect(tree(failed).some(node => node.k === "text" && node.p?.title?.includes("Private backend diagnostic"))).toBe(true);
			expect(tree(failed).some(node => node.k === "meter")).toBe(false);
		} finally { view.dispose(); }
	});
});
