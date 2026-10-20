import { expect, test } from "bun:test";
import type { GraphData } from "./graph";
import { DEFAULT_GRAPH_APPEARANCE, type GraphAppearance } from "./graph-appearance";
import { bindConceptFamily } from "./graph-data/doc-concepts.js";
import { bindTermVector } from "./graph-data/embedding.js";
import { bindDrawingWeight, layoutGraph } from "./graph-data/layout.js";
import { conceptPalette } from "./graph-data/palette.js";
import { nativeGraphLayout, type NativeGraphLayout, type NativeGraphTerritory } from "./graph-native-layout";
import type { GraphPreviewSize } from "./graph-preview";

const choices: GraphAppearance["layout"][] = ["force", "radial", "layers", "components", "map"];
/** Layouts that encode recorded topology only and draw no territories. */
const topologyChoices = ["radial", "layers", "components"] as const;
const sizes: GraphPreviewSize[] = ["inline", "compact", "overview"];
function fixture(count = 24): GraphData {
	return { kind: "graph", title: "Geometry", query: null, roots: ["n00"], caveats: ["Local indexed evidence."], truncated: false,
		nodes: Array.from({ length: count }, (_, index) => ({ id: `n${String(index).padStart(2, "0")}`, label: `Node ${index}`,
			category: "file", path: `src/n${index}.ts`, hit: null, detail: null })),
		edges: Array.from({ length: Math.max(0, count - 1) }, (_, index) => ({ from: "n00", to: `n${String(index + 1).padStart(2, "0")}`, label: "calls" })),
	};
}
/** Two lexical identifier families; each node path stem carries its family terms. */
function lexical(): GraphData {
	const families: [string, string, string[]][] = [["cache/entry", "CacheEntry", ["Store", "Read", "Evict", "Write"]], ["query/execution", "QueryExecution", ["Plan", "Run", "Parse", "Explain"]]];
	return { kind: "graph", title: "Lexical", query: null, roots: ["CacheEntryStore"], caveats: [], truncated: false,
		nodes: families.flatMap(([directory, prefix, verbs]) => verbs.map(verb => ({ id: `${prefix}${verb}`, label: `${prefix}${verb}`,
			category: "file" as const, path: `${directory}/${prefix}${verb}.ts`, hit: null, detail: null }))),
		edges: families.flatMap(([, prefix, verbs]) => verbs.slice(1).map(verb => ({ from: `${prefix}${verbs[0]}`, to: `${prefix}${verb}`, label: "calls" }))),
	};
}
/** Overview-like groups: two doc concept families and three groups without a family, with bound vectors and weights. */
function concepts(): GraphData {
	const families = { architecture: { key: "family:docs/architecture", label: "Architecture" }, policy: { key: "family:docs/policy", label: "Policy" } };
	const groups: [string, string[], keyof typeof families | undefined, number][] = [
		["arch-boot", ["boot", "chain", "vmx"], "architecture", 9], ["arch-dispatch", ["dispatch", "exit", "vmx"], "architecture", 14],
		["arch-memory", ["memory", "translation", "vmx"], "architecture", 6], ["policy-compat", ["compatibility", "cpu", "policy"], "policy", 5],
		["policy-cpu", ["admission", "cpu", "policy"], "policy", 7], ["policy-threat", ["attribution", "threat", "policy"], "policy", 4],
		["tests", ["contract", "test"], undefined, 12], ["tools", ["script", "tool"], undefined, 8], ["windows", ["kernel", "windows"], undefined, 3],
	];
	const nodes = groups.map(([id, terms, family, weight]) => {
		const node: GraphData["nodes"][number] = { id, label: id, category: "group", hit: null, detail: null };
		bindTermVector(node, new Map(terms.map(term => [term, 1 / Math.sqrt(terms.length)])));
		if (family) bindConceptFamily(node, families[family]);
		bindDrawingWeight(node, weight);
		return node;
	});
	const links: [string, string, number][] = [["arch-dispatch", "arch-boot", 6], ["arch-dispatch", "arch-memory", 4], ["arch-boot", "arch-memory", 2],
		["policy-cpu", "policy-compat", 3], ["policy-cpu", "policy-threat", 1], ["tests", "arch-dispatch", 5], ["tools", "tests", 2],
		["windows", "arch-memory", 1], ["policy-cpu", "arch-boot", 1]];
	const edges = links.map(([from, to, count]) => {
		const edge: GraphData["edges"][number] = { from, to, label: `${count} import links`, kind: "import" };
		bindDrawingWeight(edge, count);
		return edge;
	});
	return { kind: "graph", title: "Concepts", query: null, roots: [], caveats: [], truncated: false, nodes, edges };
}
const geometry = (layout: NativeGraphLayout) => layout.dots.map(dot => [dot.node.id, dot.x, dot.y]).sort();
/** Clockwise from the top around `center`, the same reading order as the radial rings. */
const clockwise = (layout: NativeGraphLayout, center: string) => {
	const hub = layout.dots.find(dot => dot.node.id === center)!;
	const angle = (dot: NativeGraphLayout["dots"][number]) => (Math.atan2(dot.y - hub.y, dot.x - hub.x) + Math.PI * 2.5) % (Math.PI * 2);
	return layout.dots.filter(dot => dot !== hub).sort((a, b) => angle(a) - angle(b)).map(dot => dot.node.id);
};
/** Territories never imply membership: member centers inside, every other dot center and territory center outside. */
function expectStrictTerritories(layout: NativeGraphLayout): void {
	const reach = (territory: NativeGraphTerritory, x: number, y: number) => ((x - territory.x) / territory.rx) ** 2 + ((y - territory.y) / territory.ry) ** 2;
	for (const territory of layout.territories) {
		expect(territory.x - territory.rx).toBeGreaterThanOrEqual(2 - 1e-9);
		expect(territory.x + territory.rx).toBeLessThanOrEqual(layout.width - 2 + 1e-9);
		expect(territory.y - territory.ry).toBeGreaterThanOrEqual(2 - 1e-9);
		expect(territory.y + territory.ry).toBeLessThanOrEqual(layout.height - 2 + 1e-9);
		expect(Math.PI * territory.rx * territory.ry).toBeLessThanOrEqual(layout.width * layout.height * .55);
		expect(territory.color).toEqual(conceptPalette(territory.key));
		expect(territory.members.length).toBeGreaterThan(1);
		for (const dot of layout.dots) {
			if (territory.members.includes(dot.node.id)) expect(reach(territory, dot.x, dot.y)).toBeLessThanOrEqual(1);
			else expect(reach(territory, dot.x, dot.y)).toBeGreaterThanOrEqual(1);
		}
		for (const other of layout.territories) if (other !== territory) expect(reach(territory, other.x, other.y)).toBeGreaterThanOrEqual(1);
	}
}

for (const choice of choices) for (const size of sizes) test(`${choice}/${size} is deterministic, bounded and retains disconnected, cyclic and directed evidence`, () => {
	const data = fixture();
	data.roots.push("n22");
	data.edges = [
		...data.edges.filter(edge => Number(edge.to.slice(1)) < 20),
		{ from: "n01", to: "n00", label: "imports" },
		{ from: "n01", to: "n02", label: "calls" }, { from: "n02", to: "n01", label: "calls" },
		{ from: "n00", to: "n00", label: "calls" }, { from: "n00", to: "n00", label: "imports" },
		{ from: "n20", to: "n21", label: "calls" }, { from: "n21", to: "n20", label: "calls" },
	];
	const original = structuredClone(data), layout = nativeGraphLayout(data, size, choice);
	expect(nativeGraphLayout(data, size, choice)).toBe(layout);
	expect(nativeGraphLayout(structuredClone(data), size, choice)).toEqual(layout);
	expect(layout.dots.map(dot => dot.node.id).sort()).toEqual(data.nodes.map(node => node.id).sort());
	expect(layout.dots.filter(dot => dot.root).map(dot => dot.node.id).sort()).toEqual([...data.roots].sort());
	expect(layout.routes.flatMap(route => route.link.indices).sort((a, b) => a - b)).toEqual(data.edges.map((_, index) => index));
	for (const dot of layout.dots) {
		expect(Number.isFinite(dot.x) && Number.isFinite(dot.y)).toBe(true);
		expect(dot.x).toBeGreaterThanOrEqual(12);
		expect(dot.x).toBeLessThanOrEqual(layout.width - 12);
		expect(dot.y).toBeGreaterThanOrEqual(12);
		expect(dot.y).toBeLessThanOrEqual(layout.height - 12);
	}
	for (const route of layout.routes) {
		expect(route.points.length).toBeGreaterThan(1);
		for (const [x, y] of route.points) {
			expect(Number.isFinite(x) && Number.isFinite(y)).toBe(true);
			expect(x).toBeGreaterThanOrEqual(0); expect(x).toBeLessThanOrEqual(layout.width);
			expect(y).toBeGreaterThanOrEqual(0); expect(y).toBeLessThanOrEqual(layout.height);
		}
		const from = layout.dots.find(dot => dot.node.id === route.link.from)!, to = layout.dots.find(dot => dot.node.id === route.link.to)!;
		expect(Math.hypot(route.points[0][0] - from.x, route.points[0][1] - from.y)).toBeLessThan(14);
		expect(Math.hypot(route.points.at(-1)![0] - to.x, route.points.at(-1)![1] - to.y)).toBeLessThan(14);
	}
	if (size === "compact") for (let a = 0; a < layout.dots.length; a++) for (let b = a + 1; b < layout.dots.length; b++) {
		expect(Math.hypot(layout.dots[a].x - layout.dots[b].x, layout.dots[a].y - layout.dots[b].y)).toBeGreaterThanOrEqual(24.6);
	}
	const forward = layout.routes.find(route => route.link.from === "n00" && route.link.to === "n01")!;
	const reverse = layout.routes.find(route => route.link.from === "n01" && route.link.to === "n00")!;
	expect(forward.points).not.toEqual([...reverse.points].reverse());
	const self = layout.routes.filter(route => route.link.from === "n00" && route.link.to === "n00");
	expect(self).toHaveLength(2);
	expect(self[0].points).not.toEqual(self[1].points);
	expect(data).toEqual(original);
});

test("compact force fills the canvas and separates persistent catalog rings without changing hit targets", () => {
	const data = fixture(), layout = nativeGraphLayout(data, "compact", "force");
	const xs = layout.dots.map(dot => dot.x), ys = layout.dots.map(dot => dot.y);
	expect(Math.max(...xs) - Math.min(...xs)).toBeGreaterThan(200);
	expect(Math.max(...ys) - Math.min(...ys)).toBeGreaterThan(110);
	for (let a = 0; a < layout.dots.length; a++) for (let b = a + 1; b < layout.dots.length; b++) {
		expect(Math.hypot(layout.dots[a].x - layout.dots[b].x, layout.dots[a].y - layout.dots[b].y)).toBeGreaterThanOrEqual(25.7);
	}
	const pending = { ...data, nodes: data.nodes.map(node => ({ ...node, indexing: "failed" as const, indexingError: "Provider unavailable" })) };
	expect(nativeGraphLayout(pending, "compact", "force").dots.map(({ x, y }) => [x, y])).toEqual(layout.dots.map(({ x, y }) => [x, y]));
});

test("layout cache distinguishes the actual layout and size, and the default is the concept map", () => {
	expect(DEFAULT_GRAPH_APPEARANCE.layout).toBe("map");
	const data = lexical(), baseline = nativeGraphLayout(data, "overview");
	expect(nativeGraphLayout(data, "overview", "map")).toBe(baseline);
	for (const choice of choices.filter(choice => choice !== "map")) {
		const changed = nativeGraphLayout(data, "overview", choice);
		expect(changed).not.toBe(baseline);
		expect(changed.dots.map(({ x, y }) => [x, y])).not.toEqual(baseline.dots.map(({ x, y }) => [x, y]));
		expect(nativeGraphLayout(data, "overview", choice)).toBe(changed);
	}
	const compact = nativeGraphLayout(data, "compact");
	expect(compact).not.toBe(baseline);
	expect(nativeGraphLayout(data, "compact", "map")).toBe(compact);
});

for (const size of sizes) test(`map/${size} places related nodes nearer, stays bounded and ignores input order and display metadata`, () => {
	const data = lexical();
	data.edges.push({ from: "CacheEntryRead", to: "QueryExecutionRun", label: "calls" }, { from: "CacheEntryStore", to: "CacheEntryStore", label: "calls" });
	const layout = nativeGraphLayout(data, size, "map");
	expect(geometry(layout)).not.toEqual(geometry(nativeGraphLayout(data, size, "force")));
	let near = 0, nearPairs = 0, far = 0, farPairs = 0;
	for (let a = 0; a < layout.dots.length; a++) for (let b = a + 1; b < layout.dots.length; b++) {
		const p = layout.dots[a], q = layout.dots[b], distance = Math.hypot(p.x - q.x, p.y - q.y);
		expect(distance).toBeGreaterThanOrEqual(p.radius + q.radius + 10 - .2);
		if (p.node.id.startsWith("CacheEntry") === q.node.id.startsWith("CacheEntry")) { near += distance; nearPairs++; } else { far += distance; farPairs++; }
	}
	expect(near / nearPairs).toBeLessThan(far / farPairs / 1.5);
	for (const dot of layout.dots) {
		expect(dot.x).toBeGreaterThanOrEqual(12); expect(dot.x).toBeLessThanOrEqual(layout.width - 12);
		expect(dot.y).toBeGreaterThanOrEqual(12); expect(dot.y).toBeLessThanOrEqual(layout.height - 12);
	}
	expect(layout.routes.flatMap(route => route.link.indices).sort((a, b) => a - b)).toEqual(data.edges.map((_, index) => index));
	const changed = structuredClone(data);
	changed.nodes.reverse(); changed.edges.reverse();
	for (const node of changed.nodes) {
		node.label = "Different label"; node.detail = "Different detail"; node.indexing = "failed"; node.indexingError = "Provider unavailable";
	}
	expect(geometry(nativeGraphLayout(changed, size, "map"))).toEqual(geometry(layout));
	expect(nativeGraphLayout(structuredClone(data), size, "map")).toEqual(layout);
});

test("concept map orientation is canonical: the heaviest node's side is on the left", () => {
	for (const [heavy, left, right] of [["CacheEntryEvict", "CacheEntry", "QueryExecution"], ["QueryExecutionRun", "QueryExecution", "CacheEntry"]]) {
		const data = lexical();
		for (const node of data.nodes) bindDrawingWeight(node, node.id === heavy ? 9 : 1);
		const layout = nativeGraphLayout(data, "overview", "map");
		const mean = (prefix: string) => layout.dots.filter(dot => dot.node.id.startsWith(prefix)).reduce((sum, dot) => sum + dot.x, 0) / 4;
		expect(mean(left)).toBeLessThan(mean(right));
	}
});

test("concept map falls back to force geometry without term or family evidence", () => {
	for (const size of sizes) {
		const data = fixture(12), map = nativeGraphLayout(data, size, "map");
		expect(map).toEqual(nativeGraphLayout(data, size, "force"));
		expect(nativeGraphLayout(data, size, "map")).toBe(map);
	}
	// Identical vectors make every pair equally similar, so the map has no shape of its own.
	const same = lexical();
	for (const node of same.nodes) node.path = "cache/entry/CacheEntry.ts";
	expect(nativeGraphLayout(same, "overview", "map")).toEqual(nativeGraphLayout(same, "overview", "force"));
	const single = lexical();
	single.nodes = single.nodes.slice(0, 1); single.edges = [];
	expect(nativeGraphLayout(single, "overview", "map")).toEqual(nativeGraphLayout(single, "overview", "force"));
});

test("Layers ranks the condensation graph, keeping a cycle together rather than inventing a tree", () => {
	const data = fixture(4);
	data.edges = [{ from: "n00", to: "n01", label: "calls" }, { from: "n01", to: "n00", label: "calls" },
		{ from: "n01", to: "n02", label: "calls" }];
	const layout = nativeGraphLayout(data, "overview", "layers"), byId = new Map(layout.dots.map(dot => [dot.node.id, dot]));
	expect(byId.get("n00")!.x).toBe(byId.get("n01")!.x);
	expect(byId.get("n02")!.x).toBeGreaterThan(byId.get("n01")!.x);
	expect(byId.has("n03")).toBe(true);
});

test("Layers removes a two-edge crossing without changing directed ranks", () => {
	const data = fixture(4);
	data.edges = [{ from: "n00", to: "n03", label: "calls" }, { from: "n01", to: "n02", label: "calls" }];
	const layout = nativeGraphLayout(data, "overview", "layers"), byId = new Map(layout.dots.map(dot => [dot.node.id, dot]));
	const sourceOrder = byId.get("n00")!.y - byId.get("n01")!.y;
	const targetOrder = byId.get("n03")!.y - byId.get("n02")!.y;
	expect(sourceOrder * targetOrder).toBeGreaterThan(0);
	expect(byId.get("n00")!.x).toBe(byId.get("n01")!.x);
	expect(byId.get("n02")!.x).toBe(byId.get("n03")!.x);
	expect(byId.get("n03")!.x).toBeGreaterThan(byId.get("n00")!.x);
	expect(layout.routes.flatMap(route => route.link.indices).sort()).toEqual([0, 1]);
});

test("Layers crossing sweeps keep cycle members adjacent within one rank", () => {
	const data = fixture(6);
	data.edges = [
		{ from: "n00", to: "n01", label: "calls" }, { from: "n01", to: "n00", label: "calls" },
		{ from: "n02", to: "n03", label: "calls" }, { from: "n03", to: "n02", label: "calls" },
		{ from: "n01", to: "n05", label: "calls" }, { from: "n02", to: "n04", label: "calls" },
	];
	const layout = nativeGraphLayout(data, "overview", "layers"), byId = new Map(layout.dots.map(dot => [dot.node.id, dot]));
	for (const group of [["n00", "n01"], ["n02", "n03"]]) {
		const members = group.map(id => byId.get(id)!);
		expect(members[0].x).toBe(members[1].x);
		const between = layout.dots.filter(dot => dot.x === members[0].x && dot.y > Math.min(...members.map(dot => dot.y)) && dot.y < Math.max(...members.map(dot => dot.y)));
		expect(between).toHaveLength(0);
	}
});

test("Components separates recorded connected sets without inventing a hierarchy", () => {
	const data = fixture(8);
	data.edges = [
		{ from: "n00", to: "n01", label: "calls" }, { from: "n01", to: "n02", label: "calls" }, { from: "n02", to: "n00", label: "calls" },
		{ from: "n03", to: "n04", label: "imports" }, { from: "n04", to: "n05", label: "imports" },
	];
	const layout = nativeGraphLayout(data, "overview", "components");
	const groups = [["n00", "n01", "n02"], ["n03", "n04", "n05"], ["n06"], ["n07"]].map(ids => {
		const points = layout.dots.filter(dot => ids.includes(dot.node.id));
		return { left: Math.min(...points.map(dot => dot.x)), right: Math.max(...points.map(dot => dot.x)),
			top: Math.min(...points.map(dot => dot.y)), bottom: Math.max(...points.map(dot => dot.y)) };
	});
	for (let a = 0; a < groups.length; a++) for (let b = a + 1; b < groups.length; b++) {
		const p = groups[a], q = groups[b];
		expect(p.right < q.left || q.right < p.left || p.bottom < q.top || q.bottom < p.top).toBe(true);
	}
	expect(layout.dots).toHaveLength(8);
	expect(layout.routes.flatMap(route => route.link.indices).sort()).toEqual([0, 1, 2, 3, 4]);
});

test("Radial rings isolated nodes around one dominant map instead of a detached row", () => {
	const data = fixture(10);
	data.edges = data.edges.filter(edge => Number(edge.to.slice(1)) < 7);
	const layout = nativeGraphLayout(data, "overview", "radial"), byId = new Map(layout.dots.map(dot => [dot.node.id, dot]));
	const root = byId.get("n00")!, isolated = ["n07", "n08", "n09"].map(id => byId.get(id)!);
	expect(new Set(isolated.map(dot => dot.y)).size).toBeGreaterThan(1);
	for (const axis of ["x", "y"] as const) {
		expect(Math.min(...isolated.map(dot => dot[axis]))).toBeLessThan(root[axis]);
		expect(Math.max(...isolated.map(dot => dot[axis]))).toBeGreaterThan(root[axis]);
	}
	const reversed = structuredClone(data);
	reversed.nodes.reverse(); reversed.edges.reverse();
	expect(nativeGraphLayout(reversed, "overview", "radial").dots.map(dot => [dot.node.id, dot.x, dot.y]).sort())
		.toEqual(layout.dots.map(dot => [dot.node.id, dot.x, dot.y]).sort());
});

for (const choice of topologyChoices) test(`${choice} ignores input ordering and non-topology metadata`, () => {
	const data = fixture(12), baseline = nativeGraphLayout(data, "overview", choice);
	const changed = structuredClone(data);
	changed.nodes.reverse(); changed.edges.reverse();
	for (const node of changed.nodes) {
		node.label = "Different label"; node.path = "another/path.ts"; node.category = "directory";
		node.detail = "Different detail"; node.indexing = "failed"; node.indexingError = "Provider unavailable";
	}
	const layout = nativeGraphLayout(changed, "overview", choice);
	expect(layout.dots.map(dot => [dot.node.id, dot.x, dot.y]).sort()).toEqual(baseline.dots.map(dot => [dot.node.id, dot.x, dot.y]).sort());
});

for (const choice of ["layers", "components"] as const) test(`${choice} handles empty and fully disconnected evidence`, () => {
	const empty = fixture(0); empty.roots = [];
	expect(nativeGraphLayout(empty, "compact", choice).dots).toEqual([]);
	expect(nativeGraphLayout(empty, "compact", choice).routes).toEqual([]);
	const disconnected = fixture(64); disconnected.edges = [];
	const layout = nativeGraphLayout(disconnected, "compact", choice);
	expect(layout.dots).toHaveLength(64);
	expect(layout.routes).toEqual([]);
	expect(layout.dots.every(dot => Number.isFinite(dot.x) && Number.isFinite(dot.y))).toBe(true);
});

for (const choice of choices) test(`${choice} handles a singleton self-loop and the full native node budget`, () => {
	const singleton = fixture(1);
	singleton.edges = [{ from: "n00", to: "n00", label: "calls" }];
	const one = nativeGraphLayout(singleton, "compact", choice);
	expect(one.dots).toHaveLength(1); expect(one.routes).toHaveLength(1);
	expect(new Set(one.routes[0].points.map(point => point.join(","))).size).toBeGreaterThan(2);
	const data = fixture(64), full = nativeGraphLayout(data, "compact", choice);
	expect(full.dots).toHaveLength(64); expect(full.routes).toHaveLength(63);
	for (const dot of full.dots) {
		expect(dot.x).toBeGreaterThanOrEqual(12); expect(dot.x).toBeLessThanOrEqual(full.width - 12);
		expect(dot.y).toBeGreaterThanOrEqual(12); expect(dot.y).toBeLessThanOrEqual(full.height - 12);
	}
});

test("inline force uses the wide canvas with a bounded stretch", () => {
	const data = fixture(), simulation = [...layoutGraph(data).positions.values()], layout = nativeGraphLayout(data, "inline", "force");
	const extent = (values: number[]) => Math.max(...values) - Math.min(...values);
	const sx = extent(layout.dots.map(dot => dot.x)) / extent(simulation.map(point => point.x));
	const sy = extent(layout.dots.map(dot => dot.y)) / extent(simulation.map(point => point.y));
	expect(sx).toBeGreaterThan(sy);
	expect(sx / sy).toBeLessThanOrEqual(1.45 + 1e-9);
});

test("bound node weights give monotonic dot radii that never overlap in compact", () => {
	const data = fixture();
	data.nodes.forEach((node, index) => { if (index) bindDrawingWeight(node, index); });
	for (const choice of choices) {
		const layout = nativeGraphLayout(data, "compact", choice), ordered = [...layout.dots].sort((a, b) => (a.weight ?? 0) - (b.weight ?? 0));
		expect(ordered[0].node.id).toBe("n00");
		expect(ordered[0].radius).toBe(3.5);
		expect("weight" in ordered[0]).toBe(false);
		expect(ordered.at(-1)).toMatchObject({ radius: 9.5, weight: 23 });
		for (let at = 1; at < ordered.length; at++) expect(ordered[at].radius).toBeGreaterThanOrEqual(ordered[at - 1].radius);
		expect(ordered[1].radius).toBeLessThan(ordered.at(-1)!.radius);
		for (const dot of layout.dots) expect(dot.radius * 2 % 1).toBe(0);
		for (let a = 0; a < layout.dots.length; a++) for (let b = a + 1; b < layout.dots.length; b++) {
			const p = layout.dots[a], q = layout.dots[b];
			expect(Math.hypot(p.x - q.x, p.y - q.y)).toBeGreaterThanOrEqual(p.radius + q.radius);
		}
	}
});

test("route tiers appear only for bound edge weights and the heaviest route gets tier 4", () => {
	const plain = fixture(6);
	for (const choice of choices) {
		const layout = nativeGraphLayout(plain, "overview", choice);
		expect(layout.routes.some(route => "tier" in route)).toBe(false);
		expect(layout.dots.some(dot => "weight" in dot)).toBe(false);
	}
	const weighted = fixture(6);
	weighted.edges.push({ from: "n00", to: "n05", label: "calls" });
	[1, 2, 4, 8, 3, 9].forEach((weight, index) => bindDrawingWeight(weighted.edges[index], weight));
	for (const choice of choices) {
		const tiers = Object.fromEntries(nativeGraphLayout(weighted, "overview", choice).routes.map(route => [route.link.to, route.tier]));
		expect(tiers).toEqual({ n01: 1, n02: 2, n03: 3, n04: 4, n05: 4 });
	}
});

for (const size of sizes) test(`${size} lexical territories exist only for force and the concept map, stay inside the canvas and cover only their members`, () => {
	for (const choice of ["force", "map"] as const) {
		const data = lexical(), layout = nativeGraphLayout(data, size, choice);
		if (size !== "compact" || choice === "map") expect(layout.territories.length).toBeGreaterThan(0);
		expectStrictTerritories(layout);
		for (const territory of layout.territories) {
			expect(new Set(territory.members.map(id => id.startsWith("CacheEntry"))).size).toBe(1);
			expect(territory.kind).toBe("lexical");
			expect(territory.title).toBe(`${territory.label} · shared terms\nLexical identifier similarity`);
		}
		expect(nativeGraphLayout(structuredClone(data), size, choice).territories).toEqual(layout.territories);
		for (const topology of topologyChoices) expect(nativeGraphLayout(data, size, topology).territories).toEqual([]);
		const small = lexical();
		small.nodes = small.nodes.slice(0, 3); small.edges = small.edges.slice(0, 2);
		expect(nativeGraphLayout(small, size, choice).territories).toEqual([]);
	}
});

for (const size of sizes) test(`${size} doc concept families replace lexical territories in force and the concept map`, () => {
	for (const choice of ["force", "map"] as const) {
		const layout = nativeGraphLayout(concepts(), size, choice);
		expect(layout.territories.length).toBeGreaterThan(0);
		expectStrictTerritories(layout);
		for (const territory of layout.territories) {
			const architecture = territory.key === "family:docs/architecture";
			expect(territory.kind).toBe("family");
			expect(territory.label).toBe(architecture ? "Architecture" : "Policy");
			expect(territory.members).toEqual(architecture ? ["arch-boot", "arch-dispatch", "arch-memory"] : ["policy-compat", "policy-cpu", "policy-threat"]);
			expect(territory.terms).toEqual([]);
			expect(territory.title).toBe(`${territory.label} · concept family\n3 concepts from docs in ${architecture ? "docs/architecture" : "docs/policy"}`);
		}
		const reordered = concepts();
		reordered.nodes.reverse(); reordered.edges.reverse();
		const again = nativeGraphLayout(reordered, size, choice);
		expect(geometry(again)).toEqual(geometry(layout));
		expect(again.territories).toEqual(layout.territories);
	}
	for (const topology of topologyChoices) expect(nativeGraphLayout(concepts(), size, topology).territories).toEqual([]);
});

test("concept map keeps doc concept families together and apart from other families", () => {
	const layout = nativeGraphLayout(concepts(), "overview", "map"), byId = new Map(layout.dots.map(dot => [dot.node.id, dot]));
	const spread = (ids: string[]) => {
		const dots = ids.map(id => byId.get(id)!), x = dots.reduce((sum, dot) => sum + dot.x, 0) / dots.length, y = dots.reduce((sum, dot) => sum + dot.y, 0) / dots.length;
		return { x, y, radius: Math.max(...dots.map(dot => Math.hypot(dot.x - x, dot.y - y))) };
	};
	const architecture = spread(["arch-boot", "arch-dispatch", "arch-memory"]), policy = spread(["policy-compat", "policy-cpu", "policy-threat"]);
	expect(Math.hypot(architecture.x - policy.x, architecture.y - policy.y)).toBeGreaterThan(architecture.radius + policy.radius);
});

for (const size of ["inline", "overview"] as const) test(`map/${size} reserves family islands before placing similar ungrouped hubs`, () => {
	const data = concepts();
	// A three-concept family and a pair, surrounded by high-weight structural nodes with the same terms.
	data.nodes = data.nodes.filter(node => node.id !== "policy-threat");
	data.edges = data.edges.filter(edge => edge.from !== "policy-threat" && edge.to !== "policy-threat");
	for (let index = 0; index < 16; index++) {
		const node: GraphData["nodes"][number] = { id: `structure-${index}`, label: `Structure ${index}`, category: "group", hit: null, detail: null };
		bindTermVector(node, new Map([["vmx", .8], ["policy", .6]]));
		bindDrawingWeight(node, index ? 12 : 200);
		data.nodes.push(node);
		for (const to of ["arch-boot", "arch-dispatch", "arch-memory", "policy-compat", "policy-cpu"]) data.edges.push({ from: node.id, to, label: "imports", kind: "import" });
	}
	const layout = nativeGraphLayout(data, size, "map");
	expect(layout.territories.map(territory => territory.key).sort()).toEqual(["family:docs/architecture", "family:docs/policy"]);
	expectStrictTerritories(layout);
	expect(layout.routes.flatMap(route => route.link.indices).sort((a, b) => a - b)).toEqual(data.edges.map((_, index) => index));
	const reordered = { ...data, nodes: [...data.nodes].reverse(), edges: [...data.edges].reverse() };
	expect(geometry(nativeGraphLayout(reordered, size, "map"))).toEqual(geometry(layout));
	expect(nativeGraphLayout(reordered, size, "map").territories).toEqual(layout.territories);
});

test("Radial orders each ring by the concept map angle so that related nodes sit side by side", () => {
	const leaves = ["CacheEntryStore", "QueryExecutionPlan", "CacheEntryRead", "QueryExecutionRun", "CacheEntryEvict", "QueryExecutionParse", "CacheEntryWrite", "QueryExecutionExplain"];
	const data: GraphData = { kind: "graph", title: "Radial", query: null, roots: ["hub"], caveats: [], truncated: false,
		nodes: [{ id: "hub", label: "hub", category: "file", path: "src/hub.ts", hit: null, detail: null }, ...leaves.map((label, index) => ({ id: `n${index}`, label,
			category: "file" as const, path: `${label.startsWith("Cache") ? "cache/entry" : "query/execution"}/${label}.ts`, hit: null, detail: null }))],
		edges: leaves.map((_, index) => ({ from: "hub", to: `n${index}`, label: "calls" })) };
	const layout = nativeGraphLayout(data, "overview", "radial"), ring = clockwise(layout, "hub");
	const family = (id: string) => leaves[Number(id.slice(1))].startsWith("Cache");
	// Without the concept map the ring alternates families in ID order; with it each family forms one arc.
	expect(ring.filter((id, index) => family(id) !== family(ring[(index + 1) % ring.length]))).toHaveLength(2);
	const reversed = structuredClone(data);
	reversed.nodes.reverse(); reversed.edges.reverse();
	expect(geometry(nativeGraphLayout(reversed, "overview", "radial"))).toEqual(geometry(layout));
	// Without term evidence the ring keeps the topology order, read from the first ID.
	const plain = fixture(9), order = clockwise(nativeGraphLayout(plain, "overview", "radial"), "n00"), start = order.indexOf("n01");
	expect([...order.slice(start), ...order.slice(0, start)]).toEqual(plain.nodes.slice(1).map(node => node.id));
});

for (const size of sizes) for (const choice of ["force", "map"] as const) test(`${choice}/${size} territories drop rather than cover unrelated nodes linked into a family`, () => {
	const data = lexical();
	for (let index = 0; index < 6; index++) {
		data.nodes.push({ id: `n${index}`, label: `Node ${index}`, category: "file", path: `src/n${index}.ts`, hit: null, detail: null });
		data.edges.push({ from: index < 3 ? "CacheEntryStore" : "QueryExecutionPlan", to: `n${index}`, label: "calls" });
	}
	data.edges.push({ from: "CacheEntryRead", to: "QueryExecutionRun", label: "calls" });
	const layout = nativeGraphLayout(data, size, choice);
	expectStrictTerritories(layout);
	expect(nativeGraphLayout(structuredClone(data), size, choice).territories).toEqual(layout.territories);
});
