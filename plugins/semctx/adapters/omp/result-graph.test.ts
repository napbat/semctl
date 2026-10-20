import { afterEach, expect, test } from "bun:test";
import { resetConcepts, setConceptSnapshot } from "./concept-colors";
import { lookupGraphColor, setGraphColorMode } from "./graph-colors";
import { readDisplay, type Display, type Hit } from "./inspector";
import { resultGraph } from "./result-graph";

afterEach(() => { resetConcepts(); setGraphColorMode("concepts"); });
function sourceHit(codebaseId: string, startLine: number): Hit {
	return { codebaseId, path: "src/shared/file.ts", symbol: "render", qualifiedName: "app.render", language: "typescript", kind: "function",
		score: null, startLine, endLine: startLine + 1, startByte: null, endByte: null, snippet: null,
		snippetStartLine: null, snippetEndLine: null, snippetStartByte: null, snippetEndByte: null, focusLine: null, focusByte: null,
		binding: { state: "not-probed", reason: null } };
}

test("same-path file roots bind to their actual source codebase, never the envelope or parsed query ids", () => {
	const display: Display = { version: 1, kind: "search", query: "render", indexWarning: null,
		identity: { codebaseId: "envelope", name: null, localRoot: null, copy: "canonical" },
		hits: [sourceHit("first", 2), sourceHit("foreign", 2), sourceHit("foreign", 12)] };
	const graph = resultGraph(display);
	if (!graph) throw new Error("Missing result graph");
	expect(graph.roots).toHaveLength(2); expect(graph.edges).toHaveLength(3);
	for (const mode of ["concepts", "structure"] as const) for (const rootId of graph.roots) {
		const root = graph.nodes.find(node => node.id === rootId)!;
		const members = graph.edges.filter(edge => edge.from === rootId).map(edge => graph.nodes.find(node => node.id === edge.to)!);
		const expected = lookupGraphColor("envelope", members[0], mode);
		if (!expected) throw new Error("No source color");
		expect(lookupGraphColor("envelope", root, mode)?.key).toBe(expected.key);
		for (const member of members) expect(lookupGraphColor("envelope", member, mode)?.key).toBe(expected.key);
		// Arbitrary client ids and captions cannot replace the bound source identity.
		root.id = `misleading:${root.id}`; root.label = "envelope/src/shared/file.ts";
		expect(lookupGraphColor("envelope", root, mode)?.key).toBe(expected.key);
		expect(lookupGraphColor("unrelated-context", root, mode)).toBeUndefined();
		root.id = rootId;
	}
	const colors = graph.roots.map(id => lookupGraphColor("envelope", graph.nodes.find(node => node.id === id)!, "structure")?.key);
	expect(colors[0]).not.toBe(colors[1]);
});

test("retained result file roots gain and lose validated concepts with their hits without rebuilding the graph", () => {
	const display: Display = { version: 1, kind: "batch", identity: { codebaseId: "envelope", name: null, localRoot: null, copy: "canonical" },
		symbols: [{ symbol: "render", hits: [sourceHit("foreign", 2), sourceHit("foreign", 12)] }] };
	const graph = resultGraph(display);
	if (!graph) throw new Error("Missing batch result graph");
	const root = graph.nodes.find(node => node.id === graph.roots[0])!, member = graph.nodes.find(node => node.hit)!;
	expect(lookupGraphColor("envelope", root, "concepts")?.source).toBe("structure");
	const overview = readDisplay({ version: 1, kind: "graph", title: "Project", query: null, roots: ["a"], caveats: [], truncated: false,
		identity: { codebaseId: "foreign", name: null, localRoot: "D:/foreign", copy: "checkout" },
		nodes: [{ id: "a", label: "file.ts", category: "file", path: "src/shared/file.ts", hit: null, detail: null },
			{ id: "b", label: "helper.ts", category: "file", path: "src/shared/helper.ts", hit: null, detail: null }],
		edges: [{ from: "a", to: "b", label: "references", kind: "reference", symbol: "render", moniker: "local:render" }],
	}, "overview", true);
	if (!overview) throw new Error("Invalid overview fixture");
	setConceptSnapshot(overview);
	const resolved = lookupGraphColor("envelope", member, "concepts");
	expect(resolved?.source).toBe("concept");
	expect(lookupGraphColor("envelope", root, "concepts")?.key).toBe(resolved?.key);
	resetConcepts();
	expect(lookupGraphColor("envelope", root, "concepts")?.source).toBe("structure");
	expect(lookupGraphColor("envelope", root, "concepts")?.key).toBe(lookupGraphColor("envelope", member, "concepts")?.key);
});
