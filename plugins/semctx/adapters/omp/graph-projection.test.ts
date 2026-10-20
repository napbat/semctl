import { afterEach, describe, expect, test } from "bun:test";
import { resetConcepts, setConceptSnapshot } from "./concept-colors";
import { projectSummary, type GraphGroup } from "./graph-projection";
import { conceptFamily, docConcepts, type DocSource } from "./graph-data/doc-concepts.js";
import { centroid, graphEmbedding } from "./graph-data/embedding.js";
import { readDisplay, type Display } from "./inspector";

type Overview = Extract<Display, { kind: "graph" }>;
const DOCS: readonly DocSource[] = [
	{ path: "docs/architecture/dispatch.md", text: "# Dispatch\n\nQueue in `src/dispatch/queue.ts`; workers live in `src/dispatch/`.\n" },
	{ path: "docs/architecture/storage.md", text: "# Storage engine\n\nBlocks in `src/storage/blocks.ts`; the store lives in `src/storage/`.\n" },
];
/** Two docs in one folder name two code areas; the `lib` pair stays outside every doc concept. */
function overview(codebaseId: string): Overview {
	const entries = [["docs/architecture/dispatch.md", "file"], ["docs/architecture/storage.md", "file"],
		["src/dispatch", "directory"], ["src/dispatch/queue.ts", "file"], ["src/dispatch/worker.ts", "file"], ["src/dispatch/pool.ts", "file"],
		["src/storage", "directory"], ["src/storage/blocks.ts", "file"], ["src/storage/journal.ts", "file"], ["src/storage/cache.ts", "file"],
		["lib/other.ts", "file"], ["lib/peer.ts", "file"]] as const;
	return { version: 1, kind: "graph", identity: { codebaseId, name: null, localRoot: "D:/repo", copy: "checkout" },
		title: "Project overview", query: null, roots: ["0"], caveats: [], truncated: true,
		nodes: entries.map(([path, category], index) => ({ id: String(index), label: path.slice(path.lastIndexOf("/") + 1), detail: null, hit: null, category, path })),
		edges: [{ from: "3", to: "4", kind: "reference", label: "references", symbol: "Job", moniker: "dispatch::Job" },
			{ from: "3", to: "7", label: "imports", kind: "import" }, { from: "10", to: "11", label: "imports", kind: "import" }] };
}
function snapshot(value: Overview): void {
	const display = readDisplay(value, "overview", true);
	if (!display) throw new Error("Invalid projection fixture");
	setConceptSnapshot(display, DOCS);
}
const shape = (groups: readonly GraphGroup[]) => groups.map(group => [group.id, group.label, group.path, group.source, group.family, group.members.map(node => node.id)]);

describe("doc concept projection", () => {
	afterEach(resetConcepts);
	test("doc concept groups carry the concept label, doc source, family and blended terms", () => {
		const value = overview("projection-docs");
		snapshot(value);
		const result = docConcepts(value, DOCS), summary = projectSummary(value, "projection-docs");
		const members = graphEmbedding(value).vectors, projected = graphEmbedding(summary.graph).vectors;
		expect(result.concepts).toHaveLength(2);
		for (const concept of result.concepts) {
			expect(concept.family).toEqual({ key: "family:docs/architecture", label: "Architecture" });
			const group = summary.allGroups.find(entry => entry.id === JSON.stringify(["projection-docs", concept.key]));
			expect(group).toMatchObject({ label: concept.label, source: concept.source, family: concept.family });
			expect(group!.members.map(node => node.id).sort()).toEqual([...concept.members].sort());
			const node = summary.graph.nodes.find(entry => entry.id === group!.id)!;
			expect(node.label).toBe(concept.label);
			expect(node.detail).toContain(`From ${concept.source}`);
			expect(conceptFamily(node)).toEqual(concept.family);
			// Equal weights: the doc terms and the member centroid, then L2-normalized.
			const vector = projected.get(node.id)!, expected = centroid([concept.terms, centroid(group!.members.map(member => members.get(member.id)))]);
			expect([...vector.keys()]).toEqual([...expected.keys()]);
			for (const [term, weight] of expected) expect(vector.get(term)).toBeCloseTo(weight, 12);
			expect([...vector.values()].reduce((sum, weight) => sum + weight * weight, 0)).toBeCloseTo(1, 12);
		}
		const rest = summary.allGroups.find(group => group.members.some(node => node.path === "lib/other.ts"))!;
		const restNode = summary.graph.nodes.find(node => node.id === rest.id)!;
		expect(rest.source).toBeUndefined();
		expect(rest.family).toBeUndefined();
		expect(conceptFamily(restNode)).toBeUndefined();
		expect(restNode.detail).not.toContain("From ");
	});
	test("doc concept projection does not depend on transport order", () => {
		const value = overview("projection-order");
		snapshot(value);
		const summary = projectSummary(value, "projection-order");
		const again = projectSummary({ ...value, nodes: [...value.nodes].reverse(), edges: [...value.edges].reverse() }, "projection-order");
		expect(again.graph).toEqual(summary.graph);
		expect(shape(again.allGroups)).toEqual(shape(summary.allGroups));
		const vectors = graphEmbedding(summary.graph).vectors, reordered = graphEmbedding(again.graph).vectors;
		for (const [index, node] of summary.graph.nodes.entries()) {
			expect(reordered.get(node.id)).toEqual(vectors.get(node.id)!);
			expect(conceptFamily(again.graph.nodes[index])).toEqual(conceptFamily(node));
		}
	});
	test("qualifies colliding structure labels across all groups without changing doc concept names", () => {
		const value = overview("projection-labels");
		const file = (id: string, project: string, branch: string): Overview["nodes"][number] =>
			({ id, label: id, detail: null, hit: null, category: "file", path: `${project}/${branch}/file.ts`, project });
		value.nodes.push(file("hv-test", "hv", "test"), file("detection-test", "detection", "test"), file("driver-test", "detection/driver", "test"),
			file("uefi-test", "uefi-boot", "test"), file("alpha-test", "packages/alpha", "test"), file("beta-test", "packages/beta", "test"),
			file("hv-include", "hv", "include"), file("client-include", "client", "include"), file("unique", "hv", "src/solo"),
			file("structure-dispatch", "hv", "Dispatch"),
			{ id: "hv-directory", label: "hv", path: "hv", category: "directory", detail: null, hit: null },
			{ id: "hv-project", label: "hv", path: "hv", project: "hv", category: "project", detail: null, hit: null },
			{ id: "hv-files", label: "CMakeLists.txt", path: "hv/CMakeLists.txt", project: "hv", category: "file", detail: null, hit: null });
		// Enough singleton groups to fold some of the collisions out of the calm drawing.
		for (let index = 0; index < 16; index++) value.nodes.push(file(`extra-${index}`, `extra-${index}`, "distinct"));
		snapshot(value);
		const summary = projectSummary(value, "projection-labels", "calm");
		const label = (id: string) => summary.allGroups.find(group => group.members.some(node => node.id === id))?.label;
		expect(label("hv-test")).toBe("hv · test");
		expect(label("detection-test")).toBe("detection · test");
		expect(label("driver-test")).toBe("detection/driver · test");
		expect(label("uefi-test")).toBe("uefi-boot · test");
		expect(label("alpha-test")).toBe("packages/alpha · test");
		expect(label("beta-test")).toBe("packages/beta · test");
		expect(label("hv-include")).toBe("hv · include");
		expect(label("client-include")).toBe("client · include");
		expect(label("unique")).toBe("src/solo");
		expect(label("structure-dispatch")).toBe("hv · Dispatch");
		expect(label("hv-directory")).toBe("Directory · hv");
		expect(label("hv-project")).toBe("Project · hv");
		expect(label("hv-files")).toBe("Files · hv");
		expect(summary.allGroups.find(group => group.source === DOCS[0].path)?.label).toBe("Dispatch");
		expect(new Set(summary.allGroups.map(group => group.label)).size).toBe(summary.allGroups.length);
		expect(new Set(summary.graph.nodes.map(node => node.label)).size).toBe(summary.graph.nodes.length);
		expect(summary.groups.some(group => group.id === "other-groups")).toBe(true);
		const again = projectSummary({ ...value, nodes: [...value.nodes].reverse(), edges: [...value.edges].reverse() }, "projection-labels", "calm");
		expect(again.graph).toEqual(summary.graph);
		expect(shape(again.allGroups)).toEqual(shape(summary.allGroups));
	});
});
