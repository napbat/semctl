import { describe, expect, test } from "bun:test";
import { clusterCount, partitionSubconcepts } from "./subconcepts.js";

const vector = (...terms) => new Map(terms.map(term => [term, 1 / Math.sqrt(terms.length)]));
const item = (name, ...terms) => ({ id: name, name, folder: name.split("/").slice(0, -1).join("/"), vector: vector(...terms) });
const shape = parts => parts.map(part => [part.label, part.kind, part.members]);

describe("subconcepts", () => {
	test("members in several child folders split by folder, direct files form their own subconcept", () => {
		const parts = partitionSubconcepts([
			item("docs/specifications/intel/a.md", "vmcs"), item("docs/specifications/intel/b.md", "vmcs"),
			item("docs/specifications/amd/c.md", "vmcb"), item("docs/specifications/README.md", "index"),
		]);
		expect(shape(parts)).toEqual([
			["intel", "folder", ["docs/specifications/intel/a.md", "docs/specifications/intel/b.md"]],
			["specifications · files", "folder", ["docs/specifications/README.md"]],
			["amd", "folder", ["docs/specifications/amd/c.md"]],
		]);
		expect(parts.map(part => [part.key, part.folder])).toEqual([
			["folder:docs/specifications/intel", "docs/specifications/intel"],
			["folder:docs/specifications/", "docs/specifications"],
			["folder:docs/specifications/amd", "docs/specifications/amd"],
		]);
	});
	test("folders beyond the limit fold into one last subconcept and every member stays assigned", () => {
		const items = ["a", "b", "c", "d", "e"].flatMap((folder, index) => Array.from({ length: 5 - index }, (_, n) => item(`src/${folder}/f${n}.ts`, folder)));
		const parts = partitionSubconcepts(items, { limit: 3 });
		expect(parts.map(part => [part.label, part.members.length])).toEqual([["a", 5], ["b", 4], ["3 other folders", 6]]);
		expect(parts.at(-1)?.key).toBe("folder:src/*");
		expect(parts.flatMap(part => part.members).sort()).toEqual(items.map(entry => entry.id).sort());
	});
	test("members of one folder split into content clusters named by their distinctive terms", () => {
		const parts = partitionSubconcepts([
			item("extracts/1.md", "vmcs", "field", "guest"), item("extracts/2.md", "vmcs", "field", "host"), item("extracts/3.md", "vmcs", "field", "guest"),
			item("extracts/4.md", "acpi", "table", "madt"), item("extracts/5.md", "acpi", "table", "fadt"), item("extracts/6.md", "acpi", "table", "madt"),
		], { limit: 2 });
		expect(parts.map(part => [part.kind, part.members, part.label])).toEqual([
			["content", ["extracts/1.md", "extracts/2.md", "extracts/3.md"], "guest · field"],
			["content", ["extracts/4.md", "extracts/5.md", "extracts/6.md"], "madt · acpi"],
		]);
		// Key terms add what the label does not already say.
		expect(parts.map(part => part.terms)).toEqual([["vmcs", "host"], ["table", "fadt"]]);
	});
	test("semantic affinity decides the clusters when the terms do not differ", () => {
		const items = ["a", "b", "c", "d"].map(name => item(`specs/${name}.md`, "spec"));
		const pairs = [{ a: "specs/a.md", b: "specs/c.md", weight: .9 }, { a: "specs/b.md", b: "specs/d.md", weight: .8 }, { a: "specs/a.md", b: "specs/b.md", weight: .1 }];
		const parts = partitionSubconcepts(items, { affinity: pairs, limit: 2 });
		expect(parts.map(part => part.members)).toEqual([["specs/a.md", "specs/c.md"], ["specs/b.md", "specs/d.md"]]);
		// No term separates the clusters, so their labels count files and a duplicate takes an ordinal.
		expect(parts.map(part => part.label)).toEqual(["2 files", "2 files 2"]);
	});
	test("the content cut keeps fewer clusters than the bound at a clear similarity drop", () => {
		const items = Array.from({ length: 12 }, (_, n) => item(`x/${String(n).padStart(2, "0")}.md`, n < 6 ? "paging" : "interrupt", n < 6 ? "table" : "vector"));
		expect(partitionSubconcepts(items, { limit: 4 }).map(part => part.members.length)).toEqual([6, 6]);
	});
	test("a dominant child folder splits into content clusters beside its sibling folders", () => {
		const extracts = Array.from({ length: 14 }, (_, n) => item(`intel/extracts/${String(n).padStart(2, "0")}.md`, n < 7 ? "paging" : "interrupt", n < 7 ? "table" : "vector"));
		const parts = partitionSubconcepts([...extracts, item("intel/notes/a.md", "notes"), item("intel/notes/b.md", "notes")], { limit: 4 });
		expect(parts.map(part => [part.label, part.kind, part.members.length])).toEqual([
			["extracts · paging · table", "content", 7],
			["extracts · interrupt · vector", "content", 7],
			["notes", "folder", 2],
		]);
	});
	test("the partition does not depend on the input order", () => {
		const items = ["a", "b", "c", "d", "e", "f"].map((name, index) => item(`x/${name}.md`, index % 2 ? "odd" : "even", name));
		const forward = partitionSubconcepts(items, { limit: 3 }), backward = partitionSubconcepts([...items].reverse(), { limit: 3 });
		expect(backward).toEqual(forward);
	});
	test("more than 160 members bucket under their most common terms, and one member's word is no key term", () => {
		const items = Array.from({ length: 170 }, (_, n) => item(`big/${String(n).padStart(3, "0")}.md`, ["alpha", "beta", "gamma"][n % 3], `unique${n}`));
		const parts = partitionSubconcepts(items, { limit: 4 });
		expect(parts.map(part => [part.label, part.members.length, part.terms])).toEqual([["alpha", 57, []], ["beta", 57, []], ["gamma", 56, []]]);
	});
	test("words that every member shares do not separate the clusters", () => {
		const items = Array.from({ length: 8 }, (_, n) => item(`manual/${n}.md`, "software", "developer", "manual", n < 4 ? "paging" : "interrupt"));
		expect(partitionSubconcepts(items, { limit: 3 }).map(part => part.label).sort()).toEqual(["interrupt", "paging"]);
	});
	test("the cluster count grows with the square root of the member count within its bounds", () => {
		expect([2, 8, 50, 200, 2000].map(n => clusterCount(n, 8))).toEqual([2, 2, 5, 8, 8]);
	});
});
