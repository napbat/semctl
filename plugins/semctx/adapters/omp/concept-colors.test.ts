import { afterEach, describe, expect, test } from "bun:test";
import { readDisplay, type Display } from "./inspector";
import { conceptRevision, groupConcept, lookupConcept, resetConcepts, setConceptSnapshot, subscribeConcepts } from "./concept-colors";
import { inferConcepts } from "./graph-data/concepts.js";
import { docConcepts, type DocSource } from "./graph-data/doc-concepts.js";
import { conceptPalette } from "./graph-data/palette.js";

type Overview = Extract<Display, { kind: "graph" }>;

function overview(codebaseId = "concept-local", prefix = "src"): Overview {
	return { version: 1, kind: "graph", identity: { codebaseId, name: null, localRoot: "D:/repo", copy: "checkout" },
		title: "Project overview", query: null, roots: [], caveats: [], truncated: false,
		nodes: ["a", "b", "unsupported"].map(id => ({ id, label: id, detail: null, hit: null, category: "file" as const, path: `${prefix}/${id}.ts` })),
		edges: [{ from: "a", to: "b", kind: "reference", label: "references", symbol: "ResolveSymbol", moniker: "actual::ResolveSymbol" }] };
}
function validated(value: Overview = overview()): Display {
	const display = readDisplay(value, "overview", true);
	if (!display) throw new Error("Invalid overview fixture");
	return display;
}
const DOCS: readonly DocSource[] = [{ path: "docs/dispatch.md", text: "# Dispatch\n\nQueue in `src/dispatch/queue.ts`; workers live in `src/dispatch/`.\n" }];
/** The doc names the `src/dispatch` files and folder. The `lib` pair shares only a reference. */
function docOverview(codebaseId: string, extra: { truncated?: boolean; loading?: boolean } = {}): Overview {
	const entries = [["docs/dispatch.md", "file"], ["src/dispatch", "directory"], ["src/dispatch/queue.ts", "file"], ["src/dispatch/worker.ts", "file"],
		["src/dispatch/pool.ts", "file"], ["lib/other.ts", "file"], ["lib/peer.ts", "file"]] as const;
	return { version: 1, kind: "graph", identity: { codebaseId, name: null, localRoot: "D:/repo", copy: "checkout" },
		title: "Project overview", query: null, roots: [], caveats: [], truncated: true, ...extra,
		nodes: entries.map(([path, category], index) => ({ id: String(index), label: path, detail: null, hit: null, category, path })),
		edges: [{ from: "2", to: "3", kind: "reference", label: "references", symbol: "Job", moniker: "dispatch::Job" },
			{ from: "5", to: "6", kind: "reference", label: "references", symbol: "Peer", moniker: "lib::Peer" }] };
}

describe("shared evidence-backed concept colors", () => {
	afterEach(resetConcepts);
	test("scopes exact normalized relative paths to their codebase and shares the map palette", () => {
		const display = overview(); setConceptSnapshot(validated(display));
		const color = lookupConcept("concept-local", "src/a.ts");
		expect(color).toBeDefined(); expect(lookupConcept("concept-local", ".\\src\\a.ts")).toEqual(color);
		expect(lookupConcept("concept-local", "src/b.ts")).toEqual(color);
		const inferred = inferConcepts(display).assignments.get("a")!;
		expect(color).toMatchObject({ key: inferred.key, kind: "reference", ...conceptPalette(inferred.key) });
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
		for (const value of [{ ...overview(), truncated: true }, { ...overview(), loading: true },
			{ ...overview(), identity: { ...overview().identity, copy: "canonical" as const, localRoot: null } }]) {
			setConceptSnapshot(validated(value)); expect(lookupConcept("concept-local", "src/a.ts")).toBeUndefined();
		}
		const imports = overview(); imports.edges = [{ from: "a", to: "b", kind: "import", label: "imports" }];
		setConceptSnapshot(validated(imports)); expect(lookupConcept("concept-local", "src/a.ts")).toBeUndefined();
	});
	test("an incomplete refresh preserves the last complete codebase palette", () => {
		setConceptSnapshot(validated());
		const color = lookupConcept("concept-local", "src/a.ts"), revision = conceptRevision();
		setConceptSnapshot(validated({ ...overview("concept-local", "replacement"), loading: true }));
		expect(lookupConcept("concept-local", "src/a.ts")).toBe(color);
		expect(lookupConcept("concept-local", "replacement/a.ts")).toBeUndefined();
		expect(conceptRevision()).toBe(revision);
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
	test("doc concepts color truncated displays, including directories, but never loading displays", () => {
		const value = docOverview("doc-local"), result = docConcepts(value, DOCS), concept = result.concepts.find(entry => entry.key === "doc:docs/dispatch.md");
		expect(concept).toBeDefined();
		setConceptSnapshot(validated(value), DOCS);
		const doc = lookupConcept("doc-local", "docs/dispatch.md");
		expect(doc).toMatchObject({ kind: "doc", key: concept!.key, label: concept!.label, title: concept!.title, source: "docs/dispatch.md", via: "doc",
			terms: concept!.terms, ...conceptPalette(concept!.key) });
		expect(lookupConcept("doc-local", "src/dispatch/queue.ts")).toMatchObject({ key: concept!.key, via: "mention" });
		expect(lookupConcept("doc-local", "src/dispatch")).toMatchObject({ key: concept!.key, via: "folder" });
		const paths = new Map(value.nodes.map(node => [node.id, node.path]));
		for (const [id, { key, via }] of result.assignments) expect(lookupConcept("doc-local", paths.get(id))).toMatchObject({ key, via });
		// A truncated display has no reference communities.
		expect(lookupConcept("doc-local", "lib/other.ts")).toBeUndefined();
		const revision = conceptRevision();
		setConceptSnapshot(validated(docOverview("doc-local")));
		setConceptSnapshot(validated(docOverview("doc-local", { loading: true })), DOCS);
		expect(lookupConcept("doc-local", "docs/dispatch.md")).toBe(doc!);
		expect(conceptRevision()).toBe(revision);
		setConceptSnapshot(validated(docOverview("doc-loading", { loading: true, truncated: false })), DOCS);
		expect(lookupConcept("doc-loading", "docs/dispatch.md")).toBeUndefined();
		setConceptSnapshot(docOverview("doc-raw"), DOCS);
		expect(lookupConcept("doc-raw", "docs/dispatch.md")).toBeUndefined();
		const canonical = docOverview("doc-canonical");
		setConceptSnapshot(validated({ ...canonical, identity: { ...canonical.identity, copy: "canonical", localRoot: null } }), DOCS);
		expect(lookupConcept("doc-canonical", "docs/dispatch.md")).toBeUndefined();
	});
	test("doc concepts take precedence; reference communities fill only the remaining files of complete displays", () => {
		const value = docOverview("doc-precedence", { truncated: false }), display = validated(value);
		const docs = docConcepts(value, DOCS), references = inferConcepts(value);
		expect(docs.assignments.has("2") && references.assignments.has("2") && references.assignments.has("5")).toBe(true);
		setConceptSnapshot(display, DOCS);
		for (const node of value.nodes) {
			const expected = docs.assignments.get(node.id)?.key ?? (node.category === "file" ? references.assignments.get(node.id)?.key : undefined);
			expect(lookupConcept("doc-precedence", node.path)?.key).toBe(expected);
		}
		expect(lookupConcept("doc-precedence", "src/dispatch/queue.ts")?.kind).toBe("doc");
		expect(lookupConcept("doc-precedence", "lib/other.ts")).toMatchObject({ kind: "reference", key: references.assignments.get("5")!.key });
		setConceptSnapshot(display);
		expect(lookupConcept("doc-precedence", "src/dispatch/queue.ts")).toMatchObject({ kind: "reference", key: references.assignments.get("2")!.key });
		expect(lookupConcept("doc-precedence", "docs/dispatch.md")).toBeUndefined();
	});
	test("each file keeps its own route while every group shares one route-free concept color", () => {
		setConceptSnapshot(validated(docOverview("doc-routes", { truncated: false })), DOCS);
		const doc = lookupConcept("doc-routes", "docs/dispatch.md")!, mention = lookupConcept("doc-routes", "src/dispatch/queue.ts")!;
		const folder = lookupConcept("doc-routes", "src/dispatch")!, reference = lookupConcept("doc-routes", "lib/other.ts")!;
		expect([doc.via, mention.via, folder.via]).toEqual(["doc", "mention", "folder"]);
		expect(lookupConcept("doc-routes", "src/dispatch/worker.ts")).toBe(mention);
		const shared = groupConcept(doc);
		expect("via" in shared).toBe(false);
		expect(shared).toMatchObject({ kind: "doc", key: doc.key, label: doc.label, title: doc.title, source: doc.source });
		expect(groupConcept(mention)).toBe(shared); expect(groupConcept(folder)).toBe(shared);
		expect(Object.isFrozen(doc) && Object.isFrozen(shared)).toBe(true);
		expect(groupConcept(reference)).toBe(reference);
	});
	test("doc evidence stays scoped to its codebase and a new snapshot replaces it", () => {
		setConceptSnapshot(validated(docOverview("doc-a")), DOCS);
		setConceptSnapshot(validated(docOverview("doc-b")), DOCS);
		for (const id of ["doc-a", "doc-b"]) expect(lookupConcept(id, "src/dispatch/queue.ts")?.kind).toBe("doc");
		expect(lookupConcept("doc-c", "src/dispatch/queue.ts")).toBeUndefined();
		const revision = conceptRevision();
		// A doc counts only as a catalog file, so this snapshot has no concept.
		setConceptSnapshot(validated(docOverview("doc-a")), [{ ...DOCS[0], path: "docs/absent.md" }]);
		expect(conceptRevision()).toBeGreaterThan(revision);
		expect(lookupConcept("doc-a", "src/dispatch/queue.ts")).toBeUndefined();
		expect(lookupConcept("doc-a", "src/dispatch")).toBeUndefined();
		expect(lookupConcept("doc-b", "src/dispatch/queue.ts")?.kind).toBe("doc");
	});
	test("new overview acceptance keeps committed colors but blocks older snapshots per codebase", () => {
		const initial = validated(docOverview("ordered-docs"));
		setConceptSnapshot(initial, DOCS);
		const color = lookupConcept("ordered-docs", "src/dispatch/queue.ts"), revision = conceptRevision();
		const old = validated(docOverview("ordered-docs")), latest = validated(docOverview("ordered-docs"));
		const foreign = validated(docOverview("ordered-foreign"));
		// New acceptance alone does not remove colors while its docs are still being read.
		expect(lookupConcept("ordered-docs", "src/dispatch/queue.ts")).toBe(color!);
		expect(conceptRevision()).toBe(revision);
		setConceptSnapshot(old, DOCS);
		setConceptSnapshot(initial, DOCS);
		expect(conceptRevision()).toBe(revision);
		expect(lookupConcept("ordered-docs", "src/dispatch/queue.ts")).toBe(color!);
		const updated = [{ ...DOCS[0], text: DOCS[0].text.replace("# Dispatch", "# Updated dispatch") }];
		setConceptSnapshot(latest, updated);
		expect(lookupConcept("ordered-docs", "src/dispatch/queue.ts")?.title).toBe("Updated dispatch");
		// Another codebase's acceptance does not block this codebase's latest snapshot.
		setConceptSnapshot(foreign, DOCS);
		expect(lookupConcept("ordered-foreign", "src/dispatch/queue.ts")?.title).toBe("Dispatch");
	});
	test("reset invalidates every accepted generation, including numbers reused by a fresh codebase", () => {
		const old = validated(docOverview("reset-order"));
		resetConcepts();
		const latest = validated(docOverview("reset-order")), revision = conceptRevision();
		setConceptSnapshot(old, DOCS);
		expect(conceptRevision()).toBe(revision);
		expect(lookupConcept("reset-order", "src/dispatch/queue.ts")).toBeUndefined();
		setConceptSnapshot(latest, DOCS);
		expect(lookupConcept("reset-order", "src/dispatch/queue.ts")?.kind).toBe("doc");
	});
});
