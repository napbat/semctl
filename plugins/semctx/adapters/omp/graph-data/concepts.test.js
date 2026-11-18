import { describe, expect, test } from "bun:test";
import { inferConcepts } from "./concepts.js";
import { createSimulation } from "./layout.js";
import { conceptPalette } from "./palette.js";
const file = (id, path = `src/${id}.rs`) => ({ id, label: id, path, category: "file", project: "", hit: null });
const reference = (from, to, symbol, moniker = `real::${symbol}`) => ({ from, to, label: "references", kind: "reference", symbol, moniker });
function modules() {
	const nodes = ["a0", "a1", "a2", "b0", "b1", "b2", "unsupported"].map(id => file(id));
	nodes.push({ id: "workspace", label: "Workspace", category: "group", path: "", hit: null });
	const edges = [];
	for (const prefix of ["a", "b"]) for (let from = 0; from < 3; from++) for (let to = 0; to < 3; to++) if (from !== to) edges.push(reference(`${prefix}${from}`, `${prefix}${to}`, prefix === "a" ? "GraphQuery" : "VectorStore"));
	for (let from = 0; from < 3; from++) for (let to = 0; to < 3; to++) edges.push(reference(`a${from}`, `b${to}`, "SharedIdentifier"));
	edges.push({ from: "unsupported", to: "a0", label: "imports", kind: "import" });
	return { nodes, edges, roots: ["workspace"] };
}
const assignments = result => result.groups.map(group => ({ key: group.key, members: group.members, symbols: group.symbols.map(item => [item.symbol, item.moniker, item.count]) }));

describe("inferred concepts from actual bindings", () => {
	test("does not invent concepts from paths, containment, imports or incomplete identities", () => {
		const data = { nodes: [file("a", "query/parser.rs"), file("b", "query/reader.rs")], edges: [{ from: "a", to: "b", kind: "containment", label: "contains" }, { from: "a", to: "b", kind: "import", label: "imports" }, { from: "a", to: "b", kind: "reference", label: "references", symbol: "Unresolved" }], roots: [] };
		const result = inferConcepts(data); expect(result.available).toBe(false); expect(result.assignments.size).toBe(0); expect(result.unassigned).toEqual(["a", "b"]);
		expect(inferConcepts({ ...data, edges: [reference("a", "a", "SelfOnly")] }).available).toBe(false);
	});
	test("finds two reference communities despite a ubiquitous shared identifier", () => {
		const data = modules(), result = inferConcepts(data);
		expect(result.available).toBe(true); expect(result.groups.map(group => group.members)).toEqual([["a0", "a1", "a2"], ["b0", "b1", "b2"]]);
		expect(result.groups.map(group => group.symbols[0].symbol)).toEqual(["GraphQuery", "VectorStore"]);
		expect(result.unassigned).toEqual(["unsupported"]); expect(result.assignments.has("workspace")).toBe(false);
	});
	test("retains verifiable original edge indices, symbol identities and complete member files", () => {
		const data = modules(), result = inferConcepts(data);
		for (const group of result.groups) {
			expect(group.relationCount).toBe(group.edges.length); const members = new Set(group.members);
			for (const index of group.edges) { const edge = data.edges[index]; expect(edge.kind).toBe("reference"); expect(members.has(edge.from)).toBe(true); expect(members.has(edge.to)).toBe(true); expect(group.symbols.some(item => item.moniker === edge.moniker)).toBe(true); }
			for (const item of group.symbols) expect(item.count).toBe(group.edges.filter(index => data.edges[index].moniker === item.moniker).length);
		}
	});
	test("is deterministic under transport reorder and independent of renamed directory/project categories", () => {
		const data = modules(), reordered = { ...data, nodes: [...data.nodes].reverse().map(node => node.category === "file" ? { ...node, path: `unrelated/${node.id}.ts`, project: "different" } : node), edges: [...data.edges].reverse() };
		const evidence = value => assignments(value).map(({ members, symbols }) => ({ members, symbols }));
		expect(evidence(inferConcepts(data))).toEqual(evidence(inferConcepts(reordered)));
	});
	test("keeps concept identity and palette stable when transport ordinals shift", () => {
		const data = modules(), before = inferConcepts(data);
		const renamed = new Map(data.nodes.map((node, index) => [node.id, `node-${300 - index}`]));
		const changed = { nodes: [{ id: "new-context", category: "directory", path: "other" }, ...data.nodes.map(node => ({ ...node, id: renamed.get(node.id) })).reverse()],
			edges: data.edges.map(edge => ({ ...edge, from: renamed.get(edge.from), to: renamed.get(edge.to) })).reverse(), roots: [] };
		const after = inferConcepts(changed);
		expect(after.groups.map(group => group.key)).toEqual(before.groups.map(group => group.key));
		for (const node of data.nodes.filter(node => node.category === "file")) {
			const original = before.assignments.get(node.id), updated = after.assignments.get(renamed.get(node.id));
			expect(updated?.key).toBe(original?.key);
			if (original) expect(conceptPalette(updated.key)).toEqual(conceptPalette(original.key));
		}
	});
	test("does not assign ambiguous paths or foreign file identities", () => {
		const nodes = [file("a", "src/a.rs"), file("b", "src/b.rs"), { ...file("foreign"), hit: { codebaseId: "foreign" } }];
		const data = { identity: { codebaseId: "local" }, nodes, edges: [reference("a", "b", "Local"), reference("a", "foreign", "Foreign")], roots: [] };
		expect(inferConcepts(data).assignments.has("foreign")).toBe(false);
		expect(inferConcepts({ ...data, nodes: [...nodes, file("duplicate", "src/a.rs")] }).assignments.size).toBe(0);
	});
	test("excludes containment and redundant project associations from inference", () => {
		const data = modules(), augmented = { ...data, edges: [...data.edges] };
		for (const node of data.nodes.filter(node => node.category === "file")) for (let repeat = 0; repeat < 6; repeat++) augmented.edges.push({ from: "workspace", to: node.id, label: "within project root", kind: "association" }, { from: "workspace", to: node.id, label: "contains", kind: "containment" });
		expect(assignments(inferConcepts(data))).toEqual(assignments(inferConcepts(augmented)));
	});
	test("concept assignments drive different physical anchors, not merely renamed directory labels", () => {
		const data = modules(), result = inferConcepts(data), categoryFor = node => result.assignments.get(node.id) ?? { key: "unassigned", label: "Unassigned", type: "No concept evidence" };
		const simulation = createSimulation(data, undefined, categoryFor);
		expect(simulation.positions.get("a0").category).toBe(result.assignments.get("a0").key); expect(simulation.positions.get("b0").category).not.toBe(simulation.positions.get("a0").category);
		const mean = members => members.reduce((value, id) => ({ x: value.x + simulation.positions.get(id).x / members.length, y: value.y + simulation.positions.get(id).y / members.length }), { x: 0, y: 0 });
		const a = mean(result.groups[0].members), b = mean(result.groups[1].members); expect(Math.hypot(a.x - b.x, a.y - b.y)).toBeGreaterThan(100);
	});
	test("keeps complete identifier provenance beyond the compact label and exposes factual structure", () => {
		const nodes = [file("a", "query/reader.rs"), file("b", "query/parser.rs")];
		const edges = Array.from({ length: 12 }, (_, index) => reference("a", "b", `Identifier${index}`));
		const result = inferConcepts({ nodes, edges, roots: [] }), group = result.groups[0];
		expect(result.groups).toHaveLength(1); expect(group.symbols).toHaveLength(12);
		expect(new Set(group.symbols.map(item => item.moniker))).toEqual(new Set(edges.map(edge => edge.moniker)));
		expect(group.relationCount).toBe(12); expect(group.directories).toEqual(["query"]); expect(group.projects).toEqual([""]);
		expect(result.unassigned).toEqual([]);
	});
	test("disambiguates repeated identifier labels with reported paths, not invented concepts", () => {
		const nodes = [file("a0", "first/a.rs"), file("a1", "first/b.rs"), file("b0", "second/a.rs"), file("b1", "second/b.rs")];
		const edges = [reference("a0", "a1", "SharedIdentity", "first::SharedIdentity"), reference("b0", "b1", "SharedIdentity", "second::SharedIdentity")];
		const result = inferConcepts({ nodes, edges, roots: [] });
		expect(result.groups.map(group => group.label)).toEqual(["Shared Identity · first", "Shared Identity · second"]);
		expect(result.groups.map(group => group.members)).toEqual([["a0", "a1"], ["b0", "b1"]]);
	});
	test("community work is bounded by sparse adjacency visits, not all file pairs", () => {
		const count = 1200, data = { nodes: Array.from({ length: count }, (_, index) => file(String(index).padStart(4, "0"))), edges: [], roots: [] };
		for (let index = 0; index < count; index++) data.edges.push(reference(data.nodes[index].id, data.nodes[(index + 1) % count].id, `Identifier${index % 40}`));
		const result = inferConcepts(data); expect(result.stats.passes).toBeLessThanOrEqual(16); expect(result.stats.visits).toBeLessThanOrEqual(data.edges.length * 32); expect(result.available).toBe(true);
	});
});
