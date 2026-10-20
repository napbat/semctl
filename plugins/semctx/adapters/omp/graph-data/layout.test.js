import { describe, expect, test } from "bun:test";
import { bindDrawingWeight, drawingWeight, category, createSimulation, layoutGraph, edgeGeometry, representativeLinks, relationKind, relationStyle, exactActivity, sharedConceptFamilies } from "./layout.js";
import { bindConceptFamily } from "./doc-concepts.js";
import { bindTermVector, graphEmbedding, semanticFamilies } from "./embedding.js";

const hit = (values = {}) => ({ path: "src/graph.js", project: null, kind: "function", symbol: "walk", qualifiedName: "Graph.walk", codebaseId: "local", score: null, snippet: null, snippetStartLine: null, snippetEndLine: null, snippetStartByte: null, snippetEndByte: null, focusLine: null, focusByte: null, binding: { state: "bound", reason: null }, ...values });
const node = (id, values = {}) => ({ id, label: id, detail: null, hit: hit(values) });
function fixture() {
	return { nodes: [node("A", { project: "core" }), node("B", { project: "core" }), node("C", { path: "ui/view.js" }), node("D", { codebaseId: "foreign" }), { id: "isolated", label: "Isolated", detail: null, hit: null }], edges: [{ from: "A", to: "B", label: "calls" }, { from: "A", to: "B", label: "also calls" }, { from: "B", to: "A", label: "returns" }, { from: "C", to: "B", label: "shares" }, { from: "B", to: "B", label: "recurses" }, { from: "B", to: "B", label: "also recurses" }, { from: "A", to: "D", label: "external" }], roots: ["A", "C"], caveats: ["Bounded"], truncated: false };
}
const coordinates = positions => [...positions].map(([id, point]) => [id, point.x, point.y, point.radius, point.category]);

describe("deterministic force layout", () => {
	test("settles to finite stable coordinates independent of viewport or input node and edge order", () => {
		const data = fixture(), original = JSON.stringify(data), first = layoutGraph(data, 360), second = layoutGraph({ ...data, nodes: [...data.nodes].reverse(), edges: [...data.edges].reverse() }, 1400);
		expect(coordinates(first.positions)).toEqual(coordinates(second.positions)); expect(JSON.stringify(data)).toBe(original);
		for (const point of first.positions.values()) { expect(Number.isFinite(point.x)).toBe(true); expect(Number.isFinite(point.y)).toBe(true); expect(point.radius).toBeGreaterThan(0); expect(point.x - point.radius).toBeGreaterThanOrEqual(first.bounds.x); expect(point.y - point.radius).toBeGreaterThanOrEqual(first.bounds.y); expect(point.x + point.radius).toBeLessThanOrEqual(first.bounds.x + first.width + .000001); expect(point.y + point.radius).toBeLessThanOrEqual(first.bounds.y + first.height + .000001); }
	});
	test("retains isolated nodes, mixed codebases and all exact directed relationships", () => {
		const data = fixture(), simulation = createSimulation(data);
		expect(simulation.positions.size).toBe(data.nodes.length); expect(simulation.links.map(link => [link.from, link.to, link.label])).toEqual(data.edges.map(edge => [edge.from, edge.to, edge.label]));
		expect(simulation.positions.get("D").node.hit.codebaseId).toBe("foreign"); expect(simulation.positions.has("isolated")).toBe(true);
	});
	test("only real metadata determines category anchors; query scores do not imply similarity", () => {
		expect(category({ id: "file", category: "file", path: "lib\\part\\file.ts", project: "engine", hit: hit({ project: "wrong" }) }).key).toBe('module:["engine","lib/part"]');
		expect(category({ id: "root", category: "project", path: "", project: "", hit: null })).toEqual({ key: "project:", label: ".", type: "Project" });
		expect(category({ category: "file", path: "lib\\part\\file.ts", hit: null }).key).toBe('module:["","lib/part"]');
		expect(category(node("kind", { path: null, kind: "trait" })).key).toBe("kind:trait"); expect(category({ hit: null }).key).toBe("structure");
		const data = fixture(), changed = { ...data, nodes: data.nodes.map(item => ({ ...item, hit: item.hit ? { ...item.hit, score: 999 } : null })) };
		expect(coordinates(createSimulation(data).positions)).toEqual(coordinates(createSimulation(changed).positions));
	});
	test("reuses previous positions without relayout when a graph gains a node", () => {
		const data = fixture(), settled = layoutGraph(data), updated = createSimulation({ ...data, nodes: [...data.nodes, node("new")] }, settled.positions);
		for (const [id, old] of settled.positions) { expect(updated.positions.get(id).x).toBe(old.x); expect(updated.positions.get(id).y).toBe(old.y); }
	});
	test("finite cooling stops the layout without changing settled positions", () => {
		const simulation = createSimulation(fixture()); let ticks = 0;
		while (simulation.tick()) { if (++ticks > 100) throw new Error("Force simulation did not stop."); }
		expect(simulation.alpha).toBeLessThan(.005); const before = coordinates(simulation.positions); expect(simulation.tick()).toBe(false); expect(coordinates(simulation.positions)).toEqual(before);
	});
	test("Barnes–Hut repulsion visits substantially fewer cells than all-pairs for large maps", () => {
		const count = 2048, data = { nodes: Array.from({ length: count }, (_, index) => ({ id: `file-${String(index).padStart(4, "0")}`, label: `File ${index}`, category: "file", path: `src/file-${index}.ts`, hit: null })), edges: [], roots: [] };
		const simulation = createSimulation(data); simulation.tick(); expect(simulation.stats.visits).toBeLessThan(count * count / 4); expect(simulation.positions.size).toBe(count);
	});
	test("parallel, reciprocal and repeated self-links have distinct finite curved geometry", () => {
		const data = fixture(), result = layoutGraph(data), geometry = data.edges.map((_, index) => edgeGeometry(data.edges, index, result.positions));
		expect(new Set(geometry.map(route => JSON.stringify([route.from, route.control, route.to]))).size).toBe(data.edges.length);
		for (const route of geometry) { expect(route.from.every(Number.isFinite)).toBe(true); expect(route.to.every(Number.isFinite)).toBe(true); expect(route.control.every(Number.isFinite)).toBe(true); }
		expect(geometry[4].self).toBe(true); expect(geometry[5].self).toBe(true);
		const old = geometry[0]; result.positions.get("A").x += 100; expect(edgeGeometry(data.edges, 0, result.positions)).not.toEqual(old);
	});
	test("directory nodes belong to their own reported branch, within a project namespace", () => {
		const directory = { id: "d", category: "directory", path: "crates/semctl/src/query", project: "crates/semctl", hit: null };
		const child = { ...directory, id: "f", category: "file", path: directory.path + "/parser/reader.rs" };
		expect(category(directory).key).toBe(category(child).key);
		expect(category(child).label).toBe("src/query");
		expect(category({ ...child, project: "another-project" }).key).not.toBe(category(child).key);
		expect(category({ id: "adapter", category: "file", path: "plugins/semctx/adapters/omp/index.ts", project: "", hit: null }).label).toBe("plugins/semctx/adapters");
		expect(category(node("symbol", { project: "engine", path: "lib/part/file.ts" })).key).toBe("project:engine");
	});
	test("unconnected groups with shared identifier terms stay nearer than unrelated groups", () => {
		const nodes = [
			{ id: "dom-frame", label: "RenderFrame SurfacePainter", category: "file", path: "src/render-dom/frame.ts", hit: null },
			{ id: "dom-surface", label: "RenderSurface FramePainter", category: "file", path: "src/render-dom/surface.ts", hit: null },
			{ id: "gpu-frame", label: "RenderFrame SurfacePainter", category: "file", path: "src/render-gpu/frame.ts", hit: null },
			{ id: "gpu-surface", label: "RenderSurface FramePainter", category: "file", path: "src/render-gpu/surface.ts", hit: null },
			{ id: "storage-journal", label: "TransactionJournal Checkpoint", category: "file", path: "src/storage/journal.ts", hit: null },
			{ id: "storage-record", label: "TransactionRecord Checkpoint", category: "file", path: "src/storage/record.ts", hit: null },
		];
		const data = { nodes, edges: [], roots: [] }, positions = layoutGraph(data).positions;
		const mean = branch => {
			const points = [...positions.values()].filter(point => point.node.path.startsWith(`src/${branch}/`));
			return { x: points.reduce((sum, point) => sum + point.x, 0) / points.length, y: points.reduce((sum, point) => sum + point.y, 0) / points.length };
		};
		const dom = mean("render-dom"), gpu = mean("render-gpu"), storage = mean("storage"), distance = (a, b) => Math.hypot(a.x - b.x, a.y - b.y);
		expect(distance(dom, gpu)).toBeLessThan(distance(dom, storage)); expect(distance(dom, gpu)).toBeLessThan(distance(gpu, storage));
		expect(coordinates(positions)).toEqual(coordinates(layoutGraph({ ...data, nodes: [...nodes].reverse() }).positions));
	});
	test("lexical families cohere and exclude unrelated connected nodes from their member radius", () => {
		const nodes = [
			...["render-a", "render-b", "render-c"].map(id => ({ id, label: "RenderFrame SurfacePainter", hit: null })),
			...["storage-a", "storage-b", "storage-c"].map(id => ({ id, label: "TransactionJournal RecordWriter", hit: null })),
			{ id: "router", label: "BoundaryRouter", hit: null }, { id: "metrics", label: "MetricsCounter", hit: null },
		];
		for (const node of nodes) bindTermVector(node, new Map([[node.id.startsWith("render-") ? "renderer" : node.id.startsWith("storage-") ? "transaction" : node.id, 1]]));
		const edges = [
			...nodes.filter(node => node.id.startsWith("render-")).map(node => ({ from: "router", to: node.id, label: "calls" })),
			...nodes.filter(node => node.id.startsWith("storage-")).map(node => ({ from: "metrics", to: node.id, label: "calls" })),
			{ from: "router", to: "metrics", label: "calls" },
		];
		const data = { nodes, edges, roots: [] }, previous = new Map([
			["render-a", { x: -120, y: -30 }], ["render-b", { x: -20, y: -30 }], ["render-c", { x: -70, y: 60 }],
			["storage-a", { x: 20, y: -30 }], ["storage-b", { x: 120, y: -30 }], ["storage-c", { x: 70, y: 60 }],
			["router", { x: -70, y: 0 }], ["metrics", { x: 70, y: 0 }],
		]);
		const ids = nodes.map(node => node.id).sort(), families = semanticFamilies(ids, graphEmbedding(data).vectors), simulation = createSimulation(data, previous);
		expect(families.map(family => family.members)).toEqual([["render-a", "render-b", "render-c"], ["storage-a", "storage-b", "storage-c"]]);
		while (simulation.tick()) { /* Use the same finite cooling as settled previews. */ }
		for (const family of families) {
			const members = family.members.map(id => simulation.positions.get(id)), memberIds = new Set(family.members);
			const x = members.reduce((sum, point) => sum + point.x / members.length, 0), y = members.reduce((sum, point) => sum + point.y / members.length, 0);
			const memberRadius = Math.max(...members.map(point => Math.hypot(point.x - x, point.y - y)));
			for (const point of simulation.positions.values()) if (!memberIds.has(point.node.id)) expect(Math.hypot(point.x - x, point.y - y)).toBeGreaterThan(memberRadius);
		}
		const shuffled = createSimulation({ ...data, nodes: [...nodes].reverse(), edges: [...edges].reverse() }, previous);
		while (shuffled.tick()) { /* Use the same finite cooling as settled previews. */ }
		expect(coordinates(simulation.positions)).toEqual(coordinates(shuffled.positions));
	});
	test("shared doc concept families keep families with two or more members, largest first", () => {
		const family = key => ({ key: `family:${key}`, label: key }), nodes = ["c", "a", "b", "d", "e", "f"].map(id => ({ id, label: id, hit: null }));
		bindConceptFamily(nodes[0], family("docs/zeta")); bindConceptFamily(nodes[1], family("docs/zeta"));
		for (const node of nodes.slice(2, 5)) bindConceptFamily(node, family("docs/alpha"));
		bindConceptFamily(nodes[5], family("docs/single"));
		expect(sharedConceptFamilies(nodes)).toEqual([
			{ key: "family:docs/alpha", label: "docs/alpha", members: ["b", "d", "e"] },
			{ key: "family:docs/zeta", label: "docs/zeta", members: ["a", "c"] }]);
		expect(sharedConceptFamilies([...nodes].reverse())).toEqual(sharedConceptFamilies(nodes));
		expect(sharedConceptFamilies(nodes.map(node => ({ ...node })))).toEqual([]);
	});
	test("doc concept families anchor and cohere their members without packing weighted dots together", () => {
		const family = key => ({ key: `family:${key}`, label: key });
		const concepts = () => {
			const nodes = ["arch-a", "arch-b", "arch-c", "policy-a", "policy-b", "policy-c", "tests", "tools"].map((id, index) => {
				const node = { id, label: id, category: "group", hit: null, detail: null };
				bindTermVector(node, new Map([[id.split("-")[0], 1]]));
				if (id.includes("-")) bindConceptFamily(node, family(id.startsWith("arch") ? "docs/architecture" : "docs/policy"));
				bindDrawingWeight(node, 4 + index);
				return node;
			});
			const edges = [["arch-a", "arch-b"], ["arch-b", "arch-c"], ["policy-a", "policy-b"], ["tests", "arch-a"], ["tests", "policy-c"], ["tools", "tests"], ["arch-c", "policy-a"]]
				.map(([from, to]) => ({ from, to, label: "imports", kind: "import" }));
			return { nodes, edges, roots: [] };
		};
		const settle = data => { const simulation = createSimulation(data); while (simulation.tick()) { /* Finite cooling. */ } return simulation; };
		const simulation = settle(concepts());
		for (const [key, prefix] of [["family:docs/architecture", "arch-"], ["family:docs/policy", "policy-"]]) {
			const members = [...simulation.positions.values()].filter(point => point.category === key);
			expect(members.map(point => point.node.id).sort()).toEqual(["a", "b", "c"].map(suffix => prefix + suffix));
			const x = members.reduce((sum, point) => sum + point.x / members.length, 0), y = members.reduce((sum, point) => sum + point.y / members.length, 0);
			const memberRadius = Math.max(...members.map(point => Math.hypot(point.x - x, point.y - y)));
			for (const point of simulation.positions.values()) if (!point.node.id.startsWith(prefix)) expect(Math.hypot(point.x - x, point.y - y)).toBeGreaterThan(memberRadius);
			for (let a = 0; a < members.length; a++) for (let b = a + 1; b < members.length; b++) {
				expect(Math.hypot(members[a].x - members[b].x, members[a].y - members[b].y)).toBeGreaterThan(members[a].radius + members[b].radius + 10);
			}
		}
		const reversed = concepts();
		reversed.nodes.reverse(); reversed.edges.reverse();
		expect(coordinates(settle(reversed).positions)).toEqual(coordinates(simulation.positions));
	});
	test("drawing weights stay out of wire data and ignore invalid values", () => {
		const item = { id: "aggregate" }, edge = { from: "a", to: "b" }, original = JSON.stringify([item, edge]);
		expect(drawingWeight(item)).toBeUndefined();
		for (const weight of [NaN, Infinity, -Infinity, 0, -1]) { bindDrawingWeight(item, weight); expect(drawingWeight(item)).toBeUndefined(); }
		bindDrawingWeight(item, 4); bindDrawingWeight(edge, .5);
		expect(drawingWeight(item)).toBe(4); expect(drawingWeight(edge)).toBe(.5);
		for (const weight of [NaN, Infinity, -Infinity, 0, -1]) { bindDrawingWeight(item, weight); expect(drawingWeight(item)).toBe(4); }
		expect(JSON.stringify([item, edge])).toBe(original);
	});
	test("bound aggregate weights give larger simulation radii than unweighted siblings", () => {
		const heavy = { id: "heavy", label: "Heavy", hit: null }, light = { id: "light", label: "Light", hit: null }, plain = { id: "plain", label: "Plain", hit: null };
		bindDrawingWeight(heavy, 9); bindDrawingWeight(light, 1);
		const positions = createSimulation({ nodes: [plain, light, heavy], edges: [], roots: [] }).positions;
		expect(positions.get("heavy").radius).toBe(8); expect(positions.get("light").radius).toBeCloseTo(2.5 + 5.5 / 3);
		expect(positions.get("heavy").radius).toBeGreaterThan(positions.get("light").radius); expect(positions.get("light").radius).toBeGreaterThan(positions.get("plain").radius);
	});
	test("weighted module anchors stay separated even with cross-module imports", () => {
		const nodes = ["query", "engine"].flatMap(branch => Array.from({ length: 8 }, (_, index) => ({ id: `${branch}-${index}`, label: `${branch}-${index}`, category: "file", project: "", path: `src/${branch}/file-${index}.rs`, hit: null })));
		const edges = [];
		for (let index = 0; index < 8; index++) for (let target = 0; target < 8; target++) edges.push({ from: `query-${index}`, to: `engine-${target}`, label: "imports", kind: "import" });
		const data = { nodes, edges, roots: [] }, result = layoutGraph(data);
		const clouds = ["query", "engine"].map(branch => {
			const points = [...result.positions.values()].filter(point => point.node.path.startsWith(`src/${branch}/`)), x = points.reduce((sum, point) => sum + point.x, 0) / points.length, y = points.reduce((sum, point) => sum + point.y, 0) / points.length;
			return { x, y, radius: points.reduce((sum, point) => sum + Math.hypot(point.x - x, point.y - y), 0) / points.length };
		});
		expect(Math.hypot(clouds[0].x - clouds[1].x, clouds[0].y - clouds[1].y)).toBeGreaterThan(2 * (clouds[0].radius + clouds[1].radius));
		const augmented = layoutGraph({ ...data, edges: [...edges, ...nodes.map(item => ({ from: "query-0", to: item.id, label: "within project root", kind: "association" }))] });
		expect([...result.positions].map(([id, point]) => [id, point.x, point.y])).toEqual([...augmented.positions].map(([id, point]) => [id, point.x, point.y]));
	});
	test("group placement follows dependency topology, not alphabetical packing or transport order", () => {
		const nodes = ["a-isolate", "b-leaf", "c-hub", "d-leaf", "e-isolate"].map(id => ({ id, label: id, category: "file", path: `${id}/node.rs`, hit: null }));
		const edges = [{ from: "c-hub", to: "b-leaf", kind: "reference", label: "references" }, { from: "c-hub", to: "d-leaf", kind: "reference", label: "references" }];
		const data = { nodes, edges, roots: [] }, simulation = createSimulation(data), shuffled = createSimulation({ ...data, nodes: [...nodes].reverse(), edges: [...edges].reverse() });
		expect(simulation.categories.map(group => group.label)).toEqual(["c-hub", "b-leaf", "d-leaf", "a-isolate", "e-isolate"]);
		expect(coordinates(simulation.positions)).toEqual(coordinates(shuffled.positions));
		expect(coordinates(simulation.positions)).not.toEqual(coordinates(createSimulation({ ...data, edges: [] }).positions));
		expect(simulation.links).toHaveLength(edges.length);
	});
	test("only explicit relation kinds and known legacy labels acquire a typed pattern", () => {
		expect(relationKind({ label: "calls" })).toBe("generic");
		expect(relationKind({ label: "inherits" })).toBe("generic");
		expect(relationKind({ label: "references" })).toBe("generic");
		expect(relationKind({ kind: "unknown", label: "imports" })).toBe("generic");
		expect(relationKind({ label: "contains reported child" })).toBe("containment");
		expect(relationKind({ label: "within project root" })).toBe("association");
		expect(relationKind({ label: "imports" })).toBe("import");
		expect(relationKind({ kind: "reference", label: "calls" })).toBe("reference");
		expect(relationStyle({ label: "calls" }).dash).toEqual([]);
		expect(relationStyle({ kind: "reference", label: "calls" }).dash).toEqual([]);
		expect(relationStyle({ kind: "containment", label: "contains" }).dash).toEqual([]);
		expect(relationStyle({ kind: "association", label: "" }).dash.length).toBeGreaterThan(0);
		expect(relationStyle({ kind: "import", label: "" }).dash.length).toBeGreaterThan(0);
		expect(relationStyle({ kind: "association", label: "" }).dash).not.toEqual(relationStyle({ kind: "import", label: "" }).dash);
	});
	test("representative routes collapse parallel links only within exact endpoints, direction and kind", () => {
		const nodes = ["a", "b"].map(id => ({ id, label: id, hit: null }));
		const edges = [
			{ from: "a", to: "b", kind: "reference", label: "references", symbol: "First", moniker: "real::First" },
			{ from: "a", to: "b", kind: "reference", label: "references", symbol: "Second", moniker: "real::Second" },
			{ from: "a", to: "b", kind: "import", label: "imports" },
			{ from: "b", to: "a", kind: "reference", label: "references", symbol: "Reverse", moniker: "real::Reverse" },
			{ from: "a", to: "a", kind: "reference", label: "references", symbol: "Self", moniker: "real::Self" },
		];
		const data = { nodes, edges, roots: [] }, original = JSON.stringify(data), pairs = representativeLinks(data);
		expect(pairs).toHaveLength(4);
		expect(pairs.flatMap(pair => pair.indices).sort((a, b) => a - b)).toEqual([0, 1, 2, 3, 4]);
		for (const pair of pairs) {
			expect(pair.count).toBe(pair.indices.length);
			for (const index of pair.indices) expect([edges[index].from, edges[index].to, relationKind(edges[index])]).toEqual([pair.from, pair.to, pair.kind]);
		}
		const parallel = pairs.find(pair => pair.count === 2);
		expect(parallel.indices).toEqual([0, 1]); expect(parallel.symbol).toBeNull(); expect(parallel.moniker).toBeNull();
		const summary = values => values.map(pair => [pair.from, pair.to, pair.kind, pair.count, pair.symbol, pair.moniker]);
		expect(summary(pairs)).toEqual(summary(representativeLinks({ ...data, edges: [...edges].reverse() })));
		expect(JSON.stringify(data)).toBe(original);
		const positions = createSimulation(data).positions;
		const routes = pairs.map((_, index) => edgeGeometry(pairs, index, positions));
		expect(new Set(routes.map(route => JSON.stringify([route.from, route.control, route.to]))).size).toBe(pairs.length);
	});
});

describe("local interaction data", () => {
	test("observed activity requires exact targets and refuses foreign-codebase matches", () => {
		const item = node("A"); expect(exactActivity(item, new Set(["graph.js", "src/", "Graph"]), "local")).toBe(false);
		expect(exactActivity(item, new Set(["src/graph.js"]), "local")).toBe(true);
		expect(exactActivity({ ...item, hit: { ...item.hit, codebaseId: "other" } }, new Set(["A", "src/graph.js", "Graph.walk"]), "local")).toBe(false);
		expect(exactActivity({ id: "file", path: "lib.rs", hit: null }, new Set(["lib.rs"]), "local")).toBe(true);
	});
});
