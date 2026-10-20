import { describe, expect, test } from "bun:test";
import { bindConceptFamily, conceptFamily, docConcepts, DOC_SOURCE_LIMITS, isDocPath } from "./doc-concepts.js";
const file = path => ({ id: path, label: path, detail: null, hit: null, category: "file", path });
const directory = path => ({ id: `dir:${path}`, label: path || "Indexed files", detail: null, hit: null, category: "directory", path });
const reference = (from, to, symbol = "shared_value") => ({ from, to, label: "references", kind: "reference", symbol, moniker: `${to}#${symbol}` });
const graph = (paths, edges = [], extra = []) => ({ nodes: [...paths.map(file), ...extra], edges });
const conceptOf = (result, source) => result.concepts.find(concept => concept.source === source);
const via = (result, id) => result.assignments.get(id)?.via;
const keyOf = (result, id) => result.assignments.get(id)?.key;
const spans = paths => paths.map(path => `\`${path}\``).join(", ");
// One doc that names 3 files of its own area; the minimal fixture that forms a concept.
const topic = (doc, title, area) => ({ doc, title, files: [0, 1, 2].map(index => `${area}/f${index}.ts`) });
function topics(specs) {
	return { data: graph(specs.flatMap(spec => [spec.doc, ...spec.files])), docs: specs.map(spec => ({ path: spec.doc, text: `# ${spec.title}\n\n${spans(spec.files)}\n` })) };
}
const snapshot = result => ({ concepts: result.concepts.map(concept => ({ ...concept, terms: [...concept.terms] })), assignments: [...result.assignments], stats: result.stats });

describe("doc concept parsing", () => {
	test("takes the first H1 outside fenced code, H2/H3 headings and code, link and bare path mentions", () => {
		const text = [
			"---", "title: Front title", "---",
			"Intro names `src/a/one.ts` and [two](../src/a/two.ts \"Two\") and [three](src/a/three.ts).",
			"```text", "# Fenced heading", "`lib/fenced.ts` lib/fenced2.ts", "```",
			"# Storage engine — blocks and journals", "",
			"## Block layout", "### Journal `replay`", "#### Deep detail",
			"Ignored: https://lib/url.ts, [site](https://lib/link.ts), ![image](../lib/image.ts) and #hashtag.",
			"~~~", "src/a/one.ts", "~~~",
		].join("\n");
		const paths = ["docs/storage.md", "src/a/one.ts", "src/a/two.ts", "src/a/three.ts", "lib/fenced.ts", "lib/fenced2.ts", "lib/url.ts", "lib/link.ts", "lib/image.ts"];
		const result = docConcepts(graph(paths), [{ path: "docs/storage.md", text }]), concept = conceptOf(result, "docs/storage.md");
		expect(concept.title).toBe("Storage engine — blocks and journals"); expect(concept.label).toBe("Storage engine"); expect(concept.key).toBe("doc:docs/storage.md");
		expect(concept.members).toEqual(["docs/storage.md", "src/a/one.ts", "src/a/three.ts", "src/a/two.ts"]);
		for (const path of ["src/a/one.ts", "src/a/two.ts", "src/a/three.ts"]) expect(via(result, path)).toBe("mention");
		expect(via(result, "docs/storage.md")).toBe("doc");
		for (const path of paths.filter(path => path.startsWith("lib/"))) expect(result.assignments.has(path)).toBe(false);
		for (const term of ["storage", "engine", "block", "layout", "journal", "replay", "docs"]) expect(concept.terms.has(term)).toBe(true);
		for (const term of ["fenced", "heading", "deep", "detail", "front"]) expect(concept.terms.has(term)).toBe(false);
	});
	test("excludes indented code and fences inside quotes and list items", () => {
		const hidden = ["hidden/alpha.ts", "hidden/beta.ts", "hidden/gamma.ts"], live = ["live/a.ts", "live/b.ts", "live/c.ts"], data = graph(["docs/example.md", ...hidden, ...live]);
		const lines = ["# Hidden heading", "## Hidden terms", ...hidden];
		const examples = [
			lines.map(line => `    ${line}`).join("\n"),
			lines.map(line => `\t${line}`).join("\n"),
			hidden.map(path => `    - ${path}`).join("\n"),
			["> ```md", ...lines.map(line => `> ${line}`), "> ```"].join("\n"),
			["> > ~~~md", ...lines.map(line => `> > ${line}`), "> > ~~~"].join("\n"),
			["- ```md", ...lines.map(line => `  ${line}`), "  ```"].join("\n"),
			["1. Example", "", "    ```md", ...lines.map(line => `    ${line}`), "    ```"].join("\n"),
			["- Outer", "  - Inner", "", "    ~~~md", ...lines.map(line => `    ${line}`), "    ~~~"].join("\n"),
			["> - ```md", ...lines.map(line => `>   ${line}`), ">   ```"].join("\n"),
			["- Example", "", ...lines.map(line => `      ${line}`)].join("\n"),
			["> ```md", ...lines.map(line => `> ${line}`)].join("\n"),
			["```md", "> ```", ...lines, "```"].join("\n"),
		];
		for (const code of examples) {
			expect(docConcepts(data, [{ path: "docs/example.md", text: `# CLI examples\n\n${code}\n` }]).concepts).toEqual([]);
			const result = docConcepts(data, [{ path: "docs/example.md", text: `${code}\n\n# Real topic\n\n${spans(live)}\n` }]), concept = conceptOf(result, "docs/example.md");
			expect(concept.title).toBe("Real topic"); expect(concept.terms.has("hidden")).toBe(false);
			for (const path of hidden) expect(result.assignments.has(path)).toBe(false);
			for (const path of live) expect(via(result, path)).toBe("mention");
		}
	});
	test("falls back to the front-matter title, then the humanized stem or README folder", () => {
		const area = name => [0, 1, 2].map(index => `${name}/f${index}.ts`);
		const docs = [
			{ path: "docs/front.md", text: `---\ntitle: "Front matter title"\n---\n${spans(area("a"))}\n` },
			{ path: "docs/hypercall-channel.md", text: `Plain text with ${spans(area("b"))}.\n` },
			{ path: "docs/runtime/README.md", text: `## Only a heading\n${spans(area("c"))}\n` },
		];
		const result = docConcepts(graph([...docs.map(doc => doc.path), ...area("a"), ...area("b"), ...area("c")]), docs);
		expect(docs.map(doc => conceptOf(result, doc.path).title)).toEqual(["Front matter title", "Hypercall channel", "Runtime"]);
	});
	test("ignores mentions after the per-document bound", () => {
		const paths = ["docs/late.md", "src/a.ts", "src/b.ts", "src/c.ts"], text = `${"`nothing` ".repeat(4000)}\n${spans(paths.slice(1))}\n`;
		expect(docConcepts(graph(paths), [{ path: "docs/late.md", text }]).concepts).toEqual([]);
		expect(docConcepts(graph(paths), [{ path: "docs/late.md", text: spans(paths.slice(1)) }]).concepts).toHaveLength(1);
	});
});

describe("doc concept evidence", () => {
	test("resolves exact files, directories, globs, suffixes, stems and symbols within the ambiguity bounds", () => {
		const paths = ["docs/topic.md", "core/exact.rs", "vendor/core/exact.rs", "pkg/p0.rs", "pkg/p1.rs", "globbed/exit_a.rs", "globbed/exit_b.rs", "globbed/other.rs",
			"one/unique_name.rs", "two_a/twice.rs", "two_b/twice.rs", "t1/thrice.rs", "t2/thrice.rs", "t3/thrice.rs", "stem/ParserState.ts", "store/vector.rs", "s1/x.rs", "s2/x.rs", "s3/x.rs", "s4/x.rs", "tools/run.rs", "caller/use.rs"];
		const edges = [reference("caller/use.rs", "store/vector.rs", "vector_store"), ...["s1", "s2", "s3", "s4"].map(folder => reference("caller/use.rs", `${folder}/x.rs`, "Shared_symbol"))];
		const text = `${spans(["core/exact.rs", "pkg/", "globbed/exit_*", "unique_name.rs", "twice.rs", "thrice.rs", "ParserState", "vector_store", "Shared_symbol", "tools", "short"])}\n`;
		const result = docConcepts(graph(paths, edges), [{ path: "docs/topic.md", text }]), mentioned = [...result.assignments].filter(([, item]) => item.via === "mention").map(([id]) => id);
		expect(mentioned).toEqual(["core/exact.rs", "globbed/exit_a.rs", "globbed/exit_b.rs", "one/unique_name.rs", "pkg/p0.rs", "pkg/p1.rs", "stem/ParserState.ts", "store/vector.rs", "two_a/twice.rs", "two_b/twice.rs"]);
	});
	test("rejects links that escape the checkout before any suffix or basename fallback", () => {
		const files = ["src/a.ts", "src/b.ts", "src/c.ts"], data = graph(["docs/external.md", ...files]);
		for (const prefix of ["../../", "part/../../../"]) {
			const text = files.map((path, index) => `[file ${index}](${prefix}${path})`).join("\n");
			expect(docConcepts(data, [{ path: "docs/external.md", text }]).concepts).toEqual([]);
		}
		const definitions = files.map((path, index) => `[file${index}]: ../../${path}`).join("\n");
		expect(docConcepts(data, [{ path: "docs/external.md", text: definitions }]).concepts).toEqual([]);
	});
	test("retains nonescaping relative links and prefers the doc folder before the checkout root", () => {
		const chosen = ["first/a.ts", "second/b.ts", "docs/guide/src/c.ts"], data = graph(["docs/guide/topic.md", ...chosen, "src/c.ts"]);
		const text = "[a](../../first/a.ts)\n[b](../../second/b.ts)\n[c](src/c.ts)\n";
		const result = docConcepts(data, [{ path: "docs/guide/topic.md", text }]);
		for (const path of chosen) expect(via(result, path)).toBe("mention");
		expect(via(result, "src/c.ts")).not.toBe("mention");
	});
	test("weights an exact path above a basename and a file stem above a symbol", () => {
		const a = ["a/a0.ts", "a/a1.ts", "a/a2.ts"], b = ["b/b0.ts", "b/b1.ts", "b/b2.ts"], edges = [reference("a/a0.ts", "x/state_table.ts", "symbol_only")];
		const data = graph(["docs/a.md", "docs/b.md", ...a, ...b, "x/contested.ts", "x/state_table.ts"], edges);
		const result = docConcepts(data, [{ path: "docs/a.md", text: spans([...a, "x/contested.ts", "symbol_only"]) }, { path: "docs/b.md", text: spans([...b, "contested.ts", "state_table"]) }]);
		expect(keyOf(result, "x/contested.ts")).toBe("doc:docs/a.md"); expect(keyOf(result, "x/state_table.ts")).toBe("doc:docs/b.md");
	});
	test("ignores self mentions and doc-to-doc mentions as membership evidence", () => {
		const paths = ["docs/index.md", "docs/a.md", "docs/b.md", "docs/c.md"], text = spans(["docs/index.md", "docs/a.md", "b.md", "./c.md"]);
		const result = docConcepts(graph(paths), [{ path: "docs/index.md", text }]);
		expect(result.concepts).toEqual([]); expect(result.assignments.size).toBe(0);
	});
	test("one example path does not name a concept", () => {
		const paths = ["docs/tool.md", ...[0, 1, 2, 3, 4].map(index => `src/auth/f${index}.rs`)];
		expect(docConcepts(graph(paths), [{ path: "docs/tool.md", text: "Filter example: `path: \"src/auth\"`.\n" }]).concepts).toEqual([]);
	});
	test("excludes hub docs that point at too much code", () => {
		const many = Array.from({ length: 70 }, (_, index) => `mod/f${index}.ts`), paths = ["docs/index.md", "docs/focus.md", ...many, "extra/one.ts"];
		const result = docConcepts(graph(paths), [{ path: "docs/index.md", text: spans(["mod/", "extra/one.ts"]) }, { path: "docs/focus.md", text: spans(many.slice(0, 3)) }]);
		expect(result.stats.hubs).toEqual(["docs/index.md"]); expect(result.concepts.map(concept => concept.source)).toEqual(["docs/focus.md"]);
		expect(conceptOf(result, "docs/focus.md").members).toEqual(["docs/focus.md", "mod/f0.ts", "mod/f1.ts", "mod/f2.ts"]);
		expect(result.assignments.has("docs/index.md")).toBe(false); expect(result.assignments.has("extra/one.ts")).toBe(false);
	});
	test("dissolves a candidate that keeps fewer than 3 files and reassigns them to the next candidate", () => {
		const big = ["big/b0.ts", "big/b1.ts", "big/b2.ts", "big/b3.ts"], paths = ["docs/big.md", "docs/small.md", ...big, "small/s1.ts", "small/s2.ts", "small/s3.ts"];
		const edges = [reference("big/b0.ts", "small/s3.ts", "vector_store")];
		const docs = [{ path: "docs/big.md", text: spans([...big, "small/s3.ts", "small/s3.ts", "small/s3.ts", "s1.ts"]) }, { path: "docs/small.md", text: spans(["small/s1.ts", "small/s2.ts", "vector_store"]) }];
		const result = docConcepts(graph(paths, edges), docs);
		expect(result.concepts.map(concept => concept.source)).toEqual(["docs/big.md"]);
		expect([via(result, "small/s1.ts"), keyOf(result, "small/s1.ts")]).toEqual(["mention", "doc:docs/big.md"]);
		expect([via(result, "small/s3.ts"), keyOf(result, "small/s3.ts")]).toEqual(["mention", "doc:docs/big.md"]);
		expect(via(result, "small/s2.ts")).toBe("folder"); expect(result.assignments.has("docs/small.md")).toBe(false);
	});
});

describe("doc concept propagation", () => {
	test("propagates over references and same-stem siblings only with a majority that covers the neighbourhood", () => {
		const core = ["core/m1.ts", "core/m2.ts", "core/m3.ts"], noise = ["noise/u1.ts", "noise/u2.ts", "noise/u3.ts", "noise/u4.ts"];
		const paths = ["docs/core.md", ...core, "near/p.ts", "far/r.ts", "weak/q.ts", "include/m1.h", ...noise];
		const edges = [reference("near/p.ts", "core/m1.ts"), reference("core/m2.ts", "near/p.ts"), reference("far/r.ts", "near/p.ts"), reference("weak/q.ts", "core/m1.ts"), ...noise.map(path => reference("weak/q.ts", path))];
		const result = docConcepts(graph(paths, edges), [{ path: "docs/core.md", text: spans(core) }]);
		expect(via(result, "near/p.ts")).toBe("reference"); expect(via(result, "far/r.ts")).toBe("reference"); expect(via(result, "include/m1.h")).toBe("reference");
		expect(result.assignments.has("weak/q.ts")).toBe(false); for (const path of noise) expect(result.assignments.has(path)).toBe(false);
	});
	test("leaves a file with tied neighbour concepts unassigned", () => {
		const { data, docs } = topics([topic("docs/a.md", "Alpha", "a"), topic("docs/b.md", "Beta", "b")]);
		data.nodes.push(file("between/t.ts")); data.edges.push(reference("between/t.ts", "a/f0.ts"), reference("between/t.ts", "b/f0.ts"));
		expect(docConcepts(data, docs).assignments.has("between/t.ts")).toBe(false);
	});
	test("inherits the nearest folder plurality only when it covers a fifth of the folder", () => {
		const big = Array.from({ length: 20 }, (_, index) => `big/x${index}.ts`);
		const paths = ["docs/topic.md", "src/f/a.ts", "src/f/b.ts", "src/f/c.ts", "src/f/d.ts", "src/f/deep/g.ts", "src/f/NOTES.md", ...big];
		const extra = [directory(""), directory("src"), directory("src/f"), directory("src/f/deep"), directory("big"), { id: "project", label: "Workspace", detail: null, hit: null, category: "project", path: "" }];
		const result = docConcepts(graph(paths, [], extra), [{ path: "docs/topic.md", text: spans(["src/f/a.ts", "src/f/b.ts", "src/f/c.ts", "big/x0.ts", "big/x1.ts"]) }]);
		for (const path of ["src/f/d.ts", "src/f/deep/g.ts", "src/f/NOTES.md"]) expect(via(result, path)).toBe("folder");
		expect(big.filter(path => result.assignments.has(path))).toEqual(["big/x0.ts", "big/x1.ts"]);
		for (const id of ["dir:src", "dir:src/f", "dir:src/f/deep"]) expect([via(result, id), keyOf(result, id)]).toEqual(["folder", "doc:docs/topic.md"]);
		for (const id of ["dir:", "dir:big", "project"]) expect(result.assignments.has(id)).toBe(false);
		expect(result.stats).toEqual({ docs: 1, hubs: [], files: paths.length, assigned: 9 });
	});
});

describe("doc concept labels and families", () => {
	test("shortens long titles at separators, caps them at a word boundary and resolves duplicates", () => {
		const { data, docs } = topics([
			topic("docs/storage.md", "Storage engine — blocks and journals", "s"), topic("docs/build.md", "Build and test", "b"),
			topic("docs/uefi.md", "UEFI, PI, and ACPI specification bindings", "u"), topic("docs/long.md", "Extraordinarily comprehensive hypervisor bring-up", "l"),
			topic("docs/amd/bindings.md", "Bindings", "amd"), topic("docs/intel/bindings.md", "Bindings", "intel"),
			topic("docs/a/same.md", "Same", "x"), topic("docs/b/a/same.md", "Same", "y"), topic("docs/source.md", "AMD source and implementation bindings", "binding"),
		]);
		const result = docConcepts(data, docs), labels = new Map(result.concepts.map(concept => [concept.source, concept.label]));
		expect(labels.get("docs/storage.md")).toBe("Storage engine"); expect(labels.get("docs/build.md")).toBe("Build and test");
		expect(labels.get("docs/uefi.md")).toBe("UEFI, PI, and ACPI…"); expect(labels.get("docs/long.md")).toBe("Extraordinarily…");
		expect(labels.get("docs/source.md")).toBe("AMD source and…");
		expect(labels.get("docs/amd/bindings.md")).toBe("Bindings · amd"); expect(labels.get("docs/intel/bindings.md")).toBe("Bindings · intel");
		expect([labels.get("docs/a/same.md"), labels.get("docs/b/a/same.md")]).toEqual(["Same · a", "Same · a 2"]);
		expect(new Set(labels.values()).size).toBe(labels.size); for (const label of labels.values()) expect(label.length).toBeLessThanOrEqual(28);
	});
	test("gives a family only to concepts that share their doc folder", () => {
		const { data, docs } = topics([
			topic("docs/architecture/dispatch.md", "Dispatch", "d"), topic("docs/architecture/storage.md", "Storage engine", "s"), topic("docs/policy/threat.md", "Threat model", "t"),
			topic("docs/specs/amd/bindings.md", "AMD bindings", "ab"), topic("docs/specs/amd/README.md", "AMD guide", "ag"), topic("README.md", "Root", "r"), topic("DESIGN.md", "Design", "e"),
		]);
		const result = docConcepts(data, docs), family = source => conceptOf(result, source).family;
		expect(family("docs/architecture/dispatch.md")).toEqual({ key: "family:docs/architecture", label: "Architecture" }); expect(family("docs/architecture/storage.md")).toBe(family("docs/architecture/dispatch.md"));
		expect(family("docs/specs/amd/README.md")).toEqual({ key: "family:docs/specs/amd", label: "AMD" });
		for (const source of ["docs/policy/threat.md", "README.md", "DESIGN.md"]) expect("family" in conceptOf(result, source)).toBe(false);
	});
	test("builds L2-normalized term vectors from the title and the first 24 headings, capped at 48 terms", () => {
		const word = index => `w${String.fromCharCode(97 + Math.floor(index / 26))}${String.fromCharCode(97 + index % 26)}`;
		const headings = Array.from({ length: 30 }, (_, index) => `## ${word(2 * index)} ${word(2 * index + 1)}`).join("\n");
		const text = `# Vector store\n\n${headings}\n${spans(["v/a.ts", "v/b.ts", "v/c.ts"])}\n`;
		const terms = conceptOf(docConcepts(graph(["docs/vector.md", "v/a.ts", "v/b.ts", "v/c.ts"]), [{ path: "docs/vector.md", text }]), "docs/vector.md").terms;
		expect([...terms.values()].reduce((sum, weight) => sum + weight * weight, 0)).toBeCloseTo(1, 10); expect(terms.size).toBe(48);
		expect(terms.get("vector")).toBe(terms.get("store")); expect(terms.has(word(48))).toBe(false);
		expect(Math.max(...[...terms].filter(([term]) => term !== "vector" && term !== "store").map(([, weight]) => weight))).toBeLessThan(terms.get("vector"));
	});
});

describe("doc concept contract", () => {
	test("is independent of node, edge and doc order and of ordinal node IDs", () => {
		const { data, docs } = topics([topic("docs/architecture/a.md", "Alpha engine", "a"), topic("docs/architecture/b.md", "Beta engine", "b"), topic("docs/c.md", "Gamma", "c")]);
		data.nodes.push(file("a/extra.ts"), file("link/p.ts"), directory("a")); data.edges.push(reference("link/p.ts", "b/f0.ts"), reference("link/p.ts", "b/f1.ts"));
		const reordered = { nodes: [...data.nodes].reverse(), edges: [...data.edges].reverse() };
		expect(snapshot(docConcepts(reordered, [...docs].reverse()))).toEqual(snapshot(docConcepts(data, docs)));
		const ordinal = new Map(data.nodes.map((node, index) => [node.id, String(index)])), renumbered = { nodes: data.nodes.map(node => ({ ...node, id: ordinal.get(node.id) })), edges: data.edges.map(edge => ({ ...edge, from: ordinal.get(edge.from), to: ordinal.get(edge.to) })) };
		const original = docConcepts(data, docs), shifted = docConcepts(renumbered, docs);
		expect(shifted.concepts.map(concept => [concept.key, concept.label, concept.members.length])).toEqual(original.concepts.map(concept => [concept.key, concept.label, concept.members.length]));
		expect([...shifted.assignments].map(([id, item]) => [data.nodes[Number(id)].id, item]).sort()).toEqual([...original.assignments].sort());
	});
	test("sorts concepts by member count, then key, and ignores docs that are not catalog files", () => {
		const { data, docs } = topics([topic("docs/z.md", "Zeta", "z"), topic("docs/a.md", "Alpha", "a"), topic("docs/m.md", "Mu", "m")]);
		data.nodes.push(file("m/f3.ts")); docs[2].text += "`m/f3.ts`\n";
		const result = docConcepts(data, [...docs, { path: "docs/missing.md", text: docs[0].text }, { path: "../escape.md", text: docs[0].text }]);
		expect(result.concepts.map(concept => concept.key)).toEqual(["doc:docs/m.md", "doc:docs/a.md", "doc:docs/z.md"]); expect(result.stats.docs).toBe(3);
	});
	test("recognizes Markdown paths and exposes frozen source bounds", () => {
		expect(["a/B.MD", "x.markdown", "page.mdx"].map(isDocPath)).toEqual([true, true, true]); expect(["a.md.txt", "md", undefined].map(isDocPath)).toEqual([false, false, false]);
		expect(DOC_SOURCE_LIMITS).toEqual({ files: 256, fileBytes: 1 << 20, totalBytes: 16 << 20 }); expect(Object.isFrozen(DOC_SOURCE_LIMITS)).toBe(true);
	});
	test("binds and clears a concept family on an object", () => {
		const node = {}, family = { key: "family:docs/architecture", label: "Architecture" };
		expect(conceptFamily(node)).toBeUndefined(); bindConceptFamily(node, family); expect(conceptFamily(node)).toBe(family); expect(conceptFamily({})).toBeUndefined();
		bindConceptFamily(node, undefined); expect(conceptFamily(node)).toBeUndefined();
	});
});
