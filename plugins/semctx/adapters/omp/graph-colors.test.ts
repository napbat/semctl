import { afterEach, describe, expect, test } from "bun:test";
import { conceptRevision, lookupConcept, resetConcepts, setConceptSnapshot, subscribeConcepts } from "./concept-colors";
import { bindGraphGroupColors, getGraphColorMode, graphColorRevision, graphGroupColor, lookupGraphColor, setGraphColorMode } from "./graph-colors";
import type { GraphData } from "./graph";
import { readDisplay, type Display, type Hit } from "./inspector";
import { conceptPalette } from "./graph-data/palette.js";

type GraphNode = GraphData["nodes"][number];
function file(path: string, extra: Partial<GraphNode> = {}): GraphNode {
	return { id: `query-local:${path}`, label: "Query-local label", detail: null, hit: null, category: "file", path, ...extra };
}
function hit(path: string | null, codebaseId: string | null = null): Hit {
	return { path, codebaseId, symbol: "Resolve", qualifiedName: "scope.Resolve", language: "typescript", kind: "function", score: null,
		startLine: 1, endLine: 1, startByte: null, endByte: null, snippet: null, snippetStartLine: null, snippetEndLine: null,
		snippetStartByte: null, snippetEndByte: null, focusLine: null, focusByte: null,
		binding: { state: "unverified", reason: null } };
}
function overview(codebaseId: string): Extract<Display, { kind: "graph" }> {
	return { version: 1, kind: "graph", identity: { codebaseId, name: null, localRoot: "D:/repo", copy: "checkout" },
		title: "Project overview", query: null, roots: [], caveats: [], truncated: false,
		nodes: [file("src/a.ts", { id: "a" }), file("src/b.ts", { id: "b" }), file("src/unsupported.ts", { id: "unsupported" })],
		edges: [{ from: "a", to: "b", kind: "reference", label: "references", symbol: "ResolveSymbol", moniker: "actual::ResolveSymbol" }] };
}
function snapshot(value: Extract<Display, { kind: "graph" }>): void {
	const display = readDisplay(value, "overview", true);
	if (!display) throw new Error("Invalid graph color fixture");
	setConceptSnapshot(display);
}

describe("shared codebase graph colors", () => {
	afterEach(resetConcepts);

	test("file, symbol, directory, project root and query-local representations share normalized source-directory colors", () => {
		const source = file("src/feature/entry.ts");
		const symbol: GraphNode = { id: "symbol-result:73", label: "Different query", detail: null, hit: hit(".\\src\\feature\\entry.ts") };
		const directory = file("src/feature", { id: "directory-result", category: "directory" });
		const project = file("src/feature", { id: "project-result", category: "project" });
		const color = lookupGraphColor("shared-source", source, "concepts");
		expect(color?.source).toBe("structure");
		for (const node of [symbol, directory, project, file("src/feature/other.ts", { id: "ordinal-shift", label: "Unrelated label", indexing: "failed", indexingError: "reported failure" })]) {
			expect(lookupGraphColor("shared-source", node, "concepts")).toEqual(color);
			expect(lookupGraphColor("shared-source", node, "structure")).toEqual(color);
		}
		expect(color).toMatchObject({ label: "src/feature", ...conceptPalette(color!.key) });
		expect(lookupGraphColor("shared-source", file("root.ts"), "structure")).toEqual(
			lookupGraphColor("shared-source", file("", { category: "directory" }), "structure"));
		expect(lookupGraphColor("shared-source", file("src/Feature/entry.ts"), "structure")?.key).not.toBe(color?.key);
		expect(lookupGraphColor("other-codebase", source, "structure")?.key).not.toBe(color?.key);
	});

	test("explicit foreign hit identities never borrow the envelope's concept or structure colors", () => {
		snapshot(overview("local-envelope"));
		const local = file("src/a.ts"), foreign = file("local/wrong.ts", { hit: hit("src/a.ts", "foreign-hit") });
		expect(lookupGraphColor("local-envelope", local, "concepts")?.source).toBe("concept");
		const fallback = lookupGraphColor("local-envelope", foreign, "concepts");
		expect(fallback?.source).toBe("structure");
		expect(fallback).toEqual(lookupGraphColor("foreign-hit", local, "structure"));
		expect(fallback).toEqual(lookupGraphColor(null, foreign, "concepts"));
		expect(fallback?.key).not.toBe(lookupGraphColor("local-envelope", local, "structure")?.key);
		snapshot(overview("foreign-hit"));
		expect(lookupGraphColor("local-envelope", foreign, "concepts")).toMatchObject({ ...lookupConcept("foreign-hit", "src/a.ts"), source: "concept" });
		expect(graphGroupColor("local-envelope", [local, foreign], "concepts")).toBeUndefined();
		expect(graphGroupColor("local-envelope", [local, foreign], "structure")).toBeUndefined();
	});

	test("missing, invalid and misleading source identities stay neutral rather than hashing ids or labels", () => {
		const pathless: GraphNode = { id: "src/a.ts", label: "src/a.ts", detail: "Directory: src", hit: null };
		expect(lookupGraphColor("source-boundary", pathless, "concepts")).toBeUndefined();
		for (const path of ["", ".", "..", "../src/a.ts", "/src/a.ts", "src//a.ts", "src/./a.ts", "D:/repo/src/a.ts", "file://src/a.ts", "src/\u0000a.ts", "x".repeat(4097)]) {
			expect(lookupGraphColor("source-boundary", file(path), "concepts")).toBeUndefined();
		}
		for (const codebase of [null, undefined, "", " padded", "padded ", "bad\nidentity", "x".repeat(4097)]) {
			expect(lookupGraphColor(codebase, file("src/a.ts"), "structure")).toBeUndefined();
		}
		for (const foreign of ["", " padded", "invalid\nidentity"]) {
			expect(lookupGraphColor("valid-envelope", file("src/a.ts", { hit: hit("src/a.ts", foreign) }), "concepts")).toBeUndefined();
		}
		expect(lookupGraphColor("valid-envelope", file("src/a.ts", { hit: hit("../invalid.ts") }), "concepts")).toBeUndefined();
		expect(lookupGraphColor("valid-envelope", file("src/a.ts", { category: "group" }), "concepts")).toBeUndefined();
	});

	test("validated complete concept evidence overrides structure, while unverified or unsupported metadata does not", () => {
		const raw = overview("validated-colors");
		setConceptSnapshot(raw);
		expect(lookupGraphColor("validated-colors", raw.nodes[0], "concepts")?.source).toBe("structure");
		snapshot(raw);
		const concept = lookupConcept("validated-colors", "src/a.ts")!;
		expect(concept).toBeDefined();
		expect(lookupGraphColor("validated-colors", raw.nodes[0], "concepts")).toMatchObject({ ...concept, source: "concept" });
		expect(lookupGraphColor("validated-colors", { id: "different-result", label: "Resolve", detail: null, hit: hit("src/a.ts") }, "concepts")).toEqual(
			lookupGraphColor("validated-colors", raw.nodes[0], "concepts"));
		expect(lookupGraphColor("validated-colors", raw.nodes[0], "structure")?.source).toBe("structure");
		expect(lookupGraphColor("validated-colors", raw.nodes[2], "concepts")?.source).toBe("structure");
		for (const value of [
			{ ...overview("partial-colors"), truncated: true },
			{ ...overview("canonical-colors"), identity: { ...overview("canonical-colors").identity, copy: "canonical" as const, localRoot: null } },
		]) {
			snapshot(value);
			expect(lookupGraphColor(value.identity.codebaseId, value.nodes[0], "concepts")?.source).toBe("structure");
		}
		const arbitrary = readDisplay(overview("tool-colors"), "imports");
		if (!arbitrary) throw new Error("Invalid ordinary graph fixture");
		setConceptSnapshot(arbitrary);
		expect(lookupGraphColor("tool-colors", file("src/a.ts"), "concepts")?.source).toBe("structure");
	});

	test("groups are colored only when every source-bearing member has the same complete color identity", () => {
		const a = file("src/a.ts"), b = file("src/b.ts"), other = file("other/c.ts");
		expect(graphGroupColor("aggregate-colors", [a, b], "structure")).toEqual(lookupGraphColor("aggregate-colors", a, "structure"));
		expect(graphGroupColor("aggregate-colors", [a, other], "structure")).toBeUndefined();
		const synthetic: GraphNode = { id: "arbitrary-wrapper", label: "Files", detail: null, hit: null, category: "group" };
		expect(graphGroupColor("aggregate-colors", [synthetic, a, b], "concepts")).toEqual(lookupGraphColor("aggregate-colors", a, "concepts"));
		for (const unknown of [
			{ ...synthetic, category: "file" as const }, { ...synthetic, category: "project" as const },
			{ ...synthetic, category: undefined }, { ...synthetic, project: "unreported-root" }, { ...synthetic, path: "unknown/path" },
		]) expect(graphGroupColor("aggregate-colors", [a, unknown], "concepts")).toBeUndefined();
		expect(graphGroupColor("aggregate-colors", [], "concepts")).toBeUndefined();
		expect(graphGroupColor("aggregate-colors", [synthetic], "concepts")).toBeUndefined();
		snapshot(overview("aggregate-colors"));
		expect(graphGroupColor("aggregate-colors", [a, b], "concepts")?.source).toBe("concept");
		expect(graphGroupColor("aggregate-colors", [a, file("src/unsupported.ts")], "concepts")).toBeUndefined();
		expect(graphGroupColor("aggregate-colors", [a, file("src", { category: "directory" })], "concepts")).toBeUndefined();
	});

	test("projection bindings preserve both palettes, neutral mixed aggregates and codebase scope without parsing projected metadata", () => {
		const data = overview("projected-colors");
		snapshot(data);
		const members = data.nodes.slice(0, 2);
		const projected = file("misleading/metadata.ts", { id: "arbitrary-query-group:19", category: "group", label: "Other result name" });
		bindGraphGroupColors(projected, "projected-colors", members);
		expect(lookupGraphColor("projected-colors", projected, "concepts")).toEqual(graphGroupColor("projected-colors", members, "concepts"));
		expect(lookupGraphColor("projected-colors", projected, "structure")).toEqual(graphGroupColor("projected-colors", members, "structure"));
		expect(lookupGraphColor("projected-colors", projected, "concepts")?.source).toBe("concept");
		expect(lookupGraphColor("projected-colors", projected, "structure")?.source).toBe("structure");
		expect(lookupGraphColor("projected-colors", projected, "neutral")).toBeUndefined();
		expect(lookupGraphColor("wrong-codebase", projected, "concepts")).toBeUndefined();
		expect(lookupGraphColor(null, projected, "structure")).toBeUndefined();
		expect(lookupGraphColor("projected-colors", { ...projected }, "concepts")).toBeUndefined();
		const mixed = file("src/a.ts", { category: "group" });
		bindGraphGroupColors(mixed, "projected-colors", [...members, file("other/c.ts")]);
		expect(lookupGraphColor("projected-colors", mixed, "concepts")).toBeUndefined();
		expect(lookupGraphColor("projected-colors", mixed, "structure")).toBeUndefined();
		expect(graphGroupColor("projected-colors", [members[0], mixed], "concepts")).toBeUndefined();
		const nested = file("ignored/path.ts", { category: "group" });
		bindGraphGroupColors(nested, "projected-colors", [projected]);
		expect(lookupGraphColor("projected-colors", nested, "concepts")).toEqual(lookupGraphColor("projected-colors", projected, "concepts"));
	});

	test("retained result roots and nested bindings gain and lose validated concepts without rebinding or changing structural colors", () => {
		const codebase = "retained-result-colors";
		const members = [file("src/a.ts"), file("src/b.ts")];
		const root = file("src/a.ts", { id: "persistent-result-root" });
		bindGraphGroupColors(root, codebase, members);
		const nested = file("ignored/path.ts", { category: "group" });
		bindGraphGroupColors(nested, codebase, [root]);
		const structure = lookupGraphColor(codebase, root, "structure");
		const evidence = JSON.stringify([root, nested]);
		expect(lookupGraphColor(codebase, root, "concepts")?.source).toBe("structure");
		expect(lookupGraphColor(codebase, nested, "concepts")?.source).toBe("structure");
		// Later edits to the caller's array cannot silently redefine this retained aggregate.
		members.push(file("other/not-an-original-member.ts"));
		snapshot(overview(codebase));
		expect(lookupGraphColor(codebase, nested, "concepts")?.source).toBe("concept");
		expect(lookupGraphColor(codebase, root, "concepts")).toEqual(lookupGraphColor(codebase, file("src/a.ts"), "concepts"));
		expect(graphGroupColor(codebase, [nested, file("src/a.ts")], "concepts")?.source).toBe("concept");
		expect(lookupGraphColor(codebase, root, "structure")).toBe(structure);
		const unsupported = overview(codebase);
		unsupported.edges = [];
		snapshot(unsupported);
		expect(lookupGraphColor(codebase, root, "concepts")).toBe(structure);
		expect(lookupGraphColor(codebase, nested, "concepts")).toBe(structure);
		expect(lookupGraphColor(codebase, root, "structure")).toBe(structure);
		snapshot(overview(codebase));
		expect(lookupGraphColor(codebase, root, "concepts")?.source).toBe("concept");
		resetConcepts();
		expect(lookupGraphColor(codebase, nested, "concepts")).toBe(structure);
		expect(JSON.stringify([root, nested])).toBe(evidence);
	});

	test("palette changes repaint existing subscribers and bound groups without changing concept revisions or graph evidence", () => {
		const previous = getGraphColorMode();
		const data = overview("mode-colors");
		snapshot(data);
		const node = data.nodes[0], projected = file("ignored/path.ts", { category: "group" });
		bindGraphGroupColors(projected, "mode-colors", data.nodes.slice(0, 2));
		const originalNodes = data.nodes, originalEdges = data.edges, evidence = JSON.stringify(data);
		const owners = [{ paints: 0 }, { paints: 0 }];
		const stops = owners.map(owner => subscribeConcepts(owner, value => { value.paints++; }));
		try {
			setGraphColorMode("concepts");
			owners.forEach(owner => { owner.paints = 0; });
			const start = graphColorRevision(), concepts = conceptRevision();
			const shared = lookupGraphColor("mode-colors", projected);
			expect(shared?.source).toBe("concept");
			setGraphColorMode("structure");
			expect(graphColorRevision()).toBe(start + 1);
			expect(conceptRevision()).toBe(concepts);
			expect(owners.map(owner => owner.paints)).toEqual([1, 1]);
			expect(lookupGraphColor("mode-colors", node)?.source).toBe("structure");
			expect(lookupGraphColor("mode-colors", projected)?.source).toBe("structure");
			expect(lookupGraphColor("mode-colors", projected, "concepts")).toBe(shared);
			setGraphColorMode("structure");
			expect(graphColorRevision()).toBe(start + 1);
			expect(owners.map(owner => owner.paints)).toEqual([1, 1]);
			setGraphColorMode("neutral");
			expect(lookupGraphColor("mode-colors", node)).toBeUndefined();
			expect(lookupGraphColor("mode-colors", projected)).toBeUndefined();
			expect(graphGroupColor("mode-colors", data.nodes)).toBeUndefined();
			setGraphColorMode("concepts");
			expect(lookupGraphColor("mode-colors", projected)).toBe(shared);
			setGraphColorMode("unrecognized" as "neutral");
			expect(getGraphColorMode()).toBe("concepts");
			expect(conceptRevision()).toBe(concepts);
			expect(JSON.stringify(data)).toBe(evidence);
			expect(data.nodes).toBe(originalNodes);
			expect(data.edges).toBe(originalEdges);
		} finally {
			stops.forEach(stop => stop());
			setGraphColorMode(previous);
		}
	});
});
