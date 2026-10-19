import { afterEach, describe, expect, test } from "bun:test";
import { readDisplay, type Display } from "./inspector";
import { conceptRevision, lookupConcept, resetConcepts, setConceptSnapshot, subscribeConcepts } from "./concept-colors";
import { inferConcepts } from "./graph-data/concepts.js";
import { conceptPalette } from "./graph-data/palette.js";

function overview(codebaseId = "concept-local", prefix = "src"): Extract<Display, { kind: "graph" }> {
	return { version: 1, kind: "graph", identity: { codebaseId, name: null, localRoot: "D:/repo", copy: "checkout" },
		title: "Project overview", query: null, roots: [], caveats: [], truncated: false,
		nodes: ["a", "b", "unsupported"].map(id => ({ id, label: id, detail: null, hit: null, category: "file" as const, path: `${prefix}/${id}.ts` })),
		edges: [{ from: "a", to: "b", kind: "reference", label: "references", symbol: "ResolveSymbol", moniker: "actual::ResolveSymbol" }] };
}
function validated(value = overview()): Display {
	const display = readDisplay(value, "overview", true);
	if (!display) throw new Error("Invalid overview fixture");
	return display;
}

describe("shared evidence-backed concept colors", () => {
	afterEach(resetConcepts);
	test("scopes exact normalized relative paths to their codebase and shares the map palette", () => {
		const display = overview(); setConceptSnapshot(validated(display));
		const color = lookupConcept("concept-local", "src/a.ts");
		expect(color).toBeDefined(); expect(lookupConcept("concept-local", ".\\src\\a.ts")).toEqual(color);
		expect(lookupConcept("concept-local", "src/b.ts")).toEqual(color);
		const inferred = inferConcepts(display).assignments.get("a")!;
		expect(color).toMatchObject({ key: inferred.key, ...conceptPalette(inferred.key) });
		for (const path of ["src/unsupported.ts", "src/A.ts", "src/a.ts/extra", "../src/a.ts", "/src/a.ts", "D:/repo/src/a.ts", "file://src/a.ts"]) expect(lookupConcept("concept-local", path)).toBeUndefined();
		for (const id of ["foreign", null, undefined]) expect(lookupConcept(id, "src/a.ts")).toBeUndefined();
		setConceptSnapshot(validated(overview("foreign", "other")));
		expect(lookupConcept("concept-local", "src/a.ts")).toEqual(color);
		expect(lookupConcept("foreign", "src/a.ts")).toBeUndefined();
	});
	test("replaces assignments, clears unsupported evidence, and resets every codebase", () => {
		setConceptSnapshot(validated()); const revision = conceptRevision();
		setConceptSnapshot(validated(overview("concept-local", "replacement")));
		expect(conceptRevision()).toBeGreaterThan(revision);
		expect(lookupConcept("concept-local", "src/a.ts")).toBeUndefined();
		expect(lookupConcept("concept-local", "replacement/a.ts")).toBeDefined();
		setConceptSnapshot(validated({ ...overview("concept-local", "replacement"), edges: [] }));
		expect(lookupConcept("concept-local", "replacement/a.ts")).toBeUndefined();
		setConceptSnapshot(validated()); setConceptSnapshot(validated(overview("foreign"))); resetConcepts();
		expect(lookupConcept("concept-local", "src/a.ts")).toBeUndefined();
		expect(lookupConcept("foreign", "src/a.ts")).toBeUndefined();
	});
	test("requires validated full local overview evidence, not arbitrary graph carriers", () => {
		const raw = overview();
		setConceptSnapshot(raw); expect(lookupConcept("concept-local", "src/a.ts")).toBeUndefined();
		const ordinary = readDisplay(raw, "imports")!;
		setConceptSnapshot(ordinary); expect(lookupConcept("concept-local", "src/a.ts")).toBeUndefined();
		for (const value of [{ ...overview(), truncated: true }, { ...overview(), identity: { ...overview().identity, copy: "canonical" as const, localRoot: null } }]) {
			setConceptSnapshot(validated(value)); expect(lookupConcept("concept-local", "src/a.ts")).toBeUndefined();
		}
		const imports = overview(); imports.edges = [{ from: "a", to: "b", kind: "import", label: "imports" }];
		setConceptSnapshot(validated(imports)); expect(lookupConcept("concept-local", "src/a.ts")).toBeUndefined();
	});
	test("keeps keys and palette stable across transport order and unrelated ordinal shifts", () => {
		const original = overview(); setConceptSnapshot(validated(original));
		const before = lookupConcept("concept-local", "src/a.ts");
		const shifted = overview(); shifted.nodes = shifted.nodes.reverse().map((node, index) => ({ ...node, id: `shifted-${index}` }));
		shifted.nodes.unshift({ id: "context", label: "Context", detail: null, hit: null, category: "directory", path: "unrelated" });
		shifted.edges = original.edges.map(edge => ({ ...edge, from: shifted.nodes.find(node => node.path === "src/a.ts")!.id, to: shifted.nodes.find(node => node.path === "src/b.ts")!.id })).reverse();
		setConceptSnapshot(validated(shifted)); expect(lookupConcept("concept-local", "src/a.ts")).toEqual(before);
	});
	test("repaints all reachable subscribers without a historical-card eviction cap", () => {
		const owners = Array.from({ length: 300 }, () => ({ paints: 0 }));
		const stops = owners.map(owner => subscribeConcepts(owner, value => { value.paints++; }));
		setConceptSnapshot(validated()); expect(owners.every(owner => owner.paints === 1)).toBe(true);
		for (const stop of stops) stop(); resetConcepts();
		expect(owners.every(owner => owner.paints === 1)).toBe(true);
	});
});
