import { describe, expect, test } from "bun:test";
import { bindTermVector, centroid, cosine, graphEmbedding, identifierTerms, semanticFamilies, semanticMap, semanticNeighbours, similarityMap } from "./embedding.js";
const file = (id, path, symbol = null) => ({ id, label: id, detail: null, path, category: "file", project: "", hit: symbol ? { path, symbol, qualifiedName: null } : null });
const planted = () => ({ nodes: [file("a1", "graph/query_parser.rs", "QueryParser"), file("a2", "graph/query_planner.rs", "QueryPlanner"), file("a3", "graph/query_cache.rs", "QueryCache"),
	file("b1", "store/vector_writer.rs", "VectorWriter"), file("b2", "store/vector_reader.rs", "VectorReader"), file("b3", "store/vector_loader.rs", "VectorLoader")], edges: [], roots: [] });
function structured() {
	const data = planted(), directory = (id, path) => ({ id, label: path, detail: null, category: "directory", path, hit: null });
	data.nodes.push(directory("dir:graph", "graph"), directory("dir:store", "store"), { id: "project", label: "Workspace", detail: null, category: "project", hit: null });
	for (const node of data.nodes.filter(node => node.category === "file")) data.edges.push({ from: `dir:${node.path.split("/")[0]}`, to: node.id, label: "contains", kind: "containment" }, { from: "project", to: node.id, label: "within project root", kind: "association" });
	data.edges.push({ from: "a1", to: "b1", label: "references", kind: "reference", symbol: "VectorWriter", moniker: "store::VectorWriter" }, { from: "a1", to: "a2", label: "imports", kind: "import" });
	return data;
}
function snapshot(data, reverse = false) {
	const { vectors, informative } = graphEmbedding(data), ids = [...vectors.keys()];
	if (reverse) ids.reverse();
	return { informative, vectors: [...vectors].map(([id, vector]) => [id, [...vector]]), map: [...semanticMap(ids, vectors)], families: semanticFamilies(ids, vectors), neighbours: semanticNeighbours(ids, vectors) };
}

describe("lexical identifier vectors", () => {
	test("splits identifiers on case, acronym, separator and digit boundaries", () => {
		expect(identifierTerms("GraphQueryBuilder")).toEqual(["graph", "query", "builder"]); expect(identifierTerms("HTTPServer")).toEqual(["http", "server"]);
		expect(identifierTerms("parseXMLHttpRequest")).toEqual(["parse", "xml", "http", "request"]);
		expect(identifierTerms("vector_store-cache.loader/path::Codec")).toEqual(["vector", "store", "cache", "loader", "path", "codec"]);
		expect(identifierTerms("utf8Decoder2024 v2")).toEqual(["utf", "decoder"]); expect(identifierTerms("src/lib/mod.rs index.tsx main.go the_x_of_y")).toEqual([]);
		expect(identifierTerms("GraphNodes class tests")).toEqual(["graph", "node", "class", "test"]); expect(identifierTerms("graph_graph Graph")).toEqual(["graph"]);
		const terms = identifierTerms("GraphQuery"); terms.push("mutated"); expect(identifierTerms("GraphQuery")).toEqual(["graph", "query"]);
	});
	test("files in one directory with related symbols are nearer than unrelated files", () => {
		const data = { nodes: [file("parser", "query/parser.rs", "QueryParser"), file("reader", "query/reader.rs", "QueryReader"), file("lexer", "query/lexer.rs", "TokenLexer"), file("vector", "storage/vector.rs", "VectorCodec")], edges: [], roots: [] };
		const { vectors, informative } = graphEmbedding(data), similarity = (a, b) => cosine(vectors.get(a), vectors.get(b));
		expect(informative).toBe(true); expect(similarity("parser", "reader")).toBeGreaterThan(similarity("parser", "lexer"));
		expect(similarity("parser", "lexer")).toBeGreaterThan(0); expect(similarity("parser", "vector")).toBe(0);
	});
	test("resolved references raise definer and user similarity; the definer carries the symbol more strongly", () => {
		const nodes = [file("x", "alpha/one.rs"), file("y", "beta/two.rs"), file("z", "gamma/three.rs")], plain = graphEmbedding({ nodes, edges: [] });
		const linked = graphEmbedding({ nodes, edges: [{ from: "x", to: "y", label: "references", kind: "reference", symbol: "TokenStream", moniker: "beta::TokenStream" }] });
		expect(cosine(plain.vectors.get("x"), plain.vectors.get("y"))).toBe(0); expect(plain.informative).toBe(false);
		expect(cosine(linked.vectors.get("x"), linked.vectors.get("y"))).toBeGreaterThan(.3); expect(linked.informative).toBe(true);
		expect(linked.vectors.get("y").get("token")).toBeGreaterThan(linked.vectors.get("x").get("token"));
		expect(cosine(linked.vectors.get("x"), linked.vectors.get("z"))).toBe(0);
	});
	test("diffuses one step over structural edges but not over project associations", () => {
		const data = planted(), blank = id => ({ id, label: "", detail: null, hit: null });
		data.nodes.push(blank("empty"), blank("lonely"));
		data.edges.push({ from: "empty", to: "a1", label: "imports", kind: "import" }, { from: "lonely", to: "a1", label: "project", kind: "association" }, { from: "lonely", to: "a2", label: "within project root" });
		const { vectors } = graphEmbedding(data);
		expect(cosine(vectors.get("empty"), vectors.get("a1"))).toBeGreaterThan(.99); expect(vectors.get("lonely").size).toBe(0);
	});
	test("vectors are normalized, capped and ordered by weight, then term", () => {
		const { vectors } = graphEmbedding(structured());
		for (const vector of vectors.values()) {
			const entries = [...vector]; expect(entries.length).toBeLessThanOrEqual(48);
			if (entries.length) expect(Math.hypot(...vector.values())).toBeCloseTo(1, 10);
			for (let index = 1; index < entries.length; index++) expect(entries[index - 1][1] > entries[index][1] || (entries[index - 1][1] === entries[index][1] && entries[index - 1][0] < entries[index][0])).toBe(true);
		}
	});
	test("is independent of node, edge and ID order", () => {
		const data = structured(), reordered = { ...data, nodes: [...data.nodes].reverse().map(node => ({ ...node })), edges: [...data.edges].reverse().map(edge => ({ ...edge })) };
		const before = snapshot(data); expect(before.informative).toBe(true); expect(snapshot(reordered, true)).toEqual(before);
	});
	test("uses bound aggregate vectors verbatim and keeps them out of diffusion", () => {
		const vector = new Map([["custom", 1]]), group = { id: "group", label: "Query group", detail: null, category: "group", path: "graph", hit: null };
		bindTermVector(group, vector);
		const data = { nodes: [group, file("a1", "graph/query_parser.rs", "QueryParser")], edges: [{ from: "group", to: "a1", label: "contains", kind: "containment" }], roots: [] }, result = graphEmbedding(data);
		expect(result.vectors.get("group")).toBe(vector); expect(result.vectors.get("a1").has("custom")).toBe(false); expect(graphEmbedding(data)).toBe(result);
		const copy = graphEmbedding({ ...data, nodes: [{ ...group }, data.nodes[1]] });
		expect(copy.vectors.get("group").has("custom")).toBe(false); expect(copy.vectors.get("group").has("graph")).toBe(true);
	});
	test("reports vectors without shared or distinct terms as uninformative", () => {
		const numbered = Array.from({ length: 3 }, (_, index) => ({ id: `n${index}`, label: `Node ${index}`, detail: null, hit: null }));
		expect(graphEmbedding({ nodes: numbered, edges: [] }).informative).toBe(false);
		expect(graphEmbedding({ nodes: numbered.map(node => ({ ...node, category: "file", path: `src/${node.id}.ts` })), edges: [] }).informative).toBe(false);
		expect(graphEmbedding({ nodes: [file("only", "query/parser.rs")], edges: [] }).informative).toBe(false);
	});
	test("cosine and centroid treat missing vectors as empty", () => {
		const a = new Map([["graph", .6], ["query", .8]]), b = new Map([["query", 1]]);
		expect(cosine(a, undefined)).toBe(0); expect(cosine(new Map(), b)).toBe(0); expect(cosine(a, b)).toBeCloseTo(.8, 12); expect(cosine(b, a)).toBeCloseTo(.8, 12);
		expect(centroid([undefined, new Map()]).size).toBe(0);
		const mean = centroid([a, undefined, b]); expect([...mean.keys()]).toEqual(["query", "graph"]); expect(Math.hypot(...mean.values())).toBeCloseTo(1, 12);
	});
});

describe("semantic placement", () => {
	test("semantic map separates two lexical groups along one axis with a deterministic sign", () => {
		const { vectors } = graphEmbedding(planted()), ids = [...vectors.keys()], map = semanticMap(ids, vectors);
		expect(map.size).toBe(6);
		const group = prefix => ids.filter(id => id.startsWith(prefix)).map(id => map.get(id)), mean = points => points.reduce(([x, y], point) => [x + point[0] / points.length, y + point[1] / points.length], [0, 0]);
		const a = group("a"), b = group("b"), ma = mean(a), mb = mean(b), spread = [...a.map(point => Math.hypot(point[0] - ma[0], point[1] - ma[1])), ...b.map(point => Math.hypot(point[0] - mb[0], point[1] - mb[1]))].reduce((sum, value) => sum + value, 0) / 6;
		expect(Math.hypot(ma[0] - mb[0], ma[1] - mb[1])).toBeGreaterThan(2 * spread);
		expect(a.every(([x]) => x < 0)).toBe(true); expect(b.every(([x]) => x > 0)).toBe(true);
		expect(Math.sqrt([...map.values()].reduce((sum, [x, y]) => sum + x * x + y * y, 0) / map.size)).toBeCloseTo(1, 6);
	});
	test("semantic map rejects uninformative input and places two points on the x axis", () => {
		const graph = new Map([["graph", 1]]), vectors = new Map([["a", graph], ["b", graph], ["c", graph], ["d", new Map()], ["e", new Map([["store", 1]])]]);
		expect(semanticMap(["a"], vectors)).toBeUndefined(); expect(semanticMap(["a", "b", "c"], vectors)).toBeUndefined(); expect(semanticMap(["d", "e", "missing"], vectors)).toBeUndefined();
		const pair = new Map([["a", new Map([["graph", .8], ["query", .6]])], ["b", new Map([["graph", .6], ["store", .8]])]]);
		expect([...semanticMap(["b", "a"], pair)]).toEqual([["a", [-1, 0]], ["b", [1, 0]]]);
	});
	test("semantic map is the similarity map of cosine similarity", () => {
		const { vectors } = graphEmbedding(structured()), ids = [...vectors.keys()].reverse();
		expect(semanticMap(ids, vectors)).toEqual(similarityMap(ids, (a, b) => cosine(vectors.get(a), vectors.get(b))));
	});
	test("similarity map keeps the two largest eigenvalues of an indefinite similarity", () => {
		// A planar Gram matrix (eigenvalues 10 and 6) minus a stronger negative direction: the largest magnitudes would be the wrong axes.
		const points = { a: [-2, 0], b: [-1, 1], c: [0, 0], d: [1, 1], e: [2, 0], f: [0, -2] }, negative = { a: 1, b: 0, c: -2, d: 0, e: 1, f: 0 }, ids = Object.keys(points);
		for (const strength of [0, 20, 60]) {
			const similarity = (p, q) => points[p][0] * points[q][0] + points[p][1] * points[q][1] - strength * negative[p] * negative[q] / 6;
			const map = similarityMap([...ids].reverse(), similarity);
			expect([...map.keys()]).toEqual(ids);
			// Map distances are the planar distances at one scale: RMS radius 1 instead of √(16 / 6).
			for (const p of ids) for (const q of ids) if (p < q) {
				const distance = Math.hypot(map.get(p)[0] - map.get(q)[0], map.get(p)[1] - map.get(q)[1]);
				expect(distance / Math.hypot(points[p][0] - points[q][0], points[p][1] - points[q][1])).toBeCloseTo(Math.sqrt(6 / 16), 5);
			}
			expect(similarityMap(ids, similarity)).toEqual(map);
		}
		expect(similarityMap(["a", "b", "c"], () => 1)).toBeUndefined(); expect(similarityMap(["a", "b", "c"], (p, q) => p === q ? 1 : 0)).toBeUndefined();
	});
	test("semantic families recover planted groups with labels from shared terms", () => {
		const { vectors } = graphEmbedding(planted()), ids = [...vectors.keys()], families = semanticFamilies(ids, vectors);
		expect(families.map(({ key, label, terms, members }) => ({ key, label, terms, members }))).toEqual([
			{ key: "family:query+graph", label: "query · graph", terms: ["query", "graph"], members: ["a1", "a2", "a3"] },
			{ key: "family:vector+store", label: "vector · store", terms: ["vector", "store"], members: ["b1", "b2", "b3"] }]);
		for (const family of families) expect(family.cohesion).toBeGreaterThanOrEqual(.22);
		expect(semanticFamilies(ids, vectors, { threshold: .5 })).toEqual([]); expect(semanticFamilies(ids, vectors, { limit: 1 })).toHaveLength(1);
	});
	test("semantic families keep distinct keys for repeated term labels", () => {
		const rest = Math.sqrt(1 - 2 * .45 ** 2 - .4 ** 2), member = (extra, own) => new Map([["graph", .45], ["query", .45], [extra, .4], [own, rest]]);
		const vectors = new Map([["x1", member("left", "x1")], ["x2", member("left", "x2")], ["y1", member("right", "y1")], ["y2", member("right", "y2")]]);
		expect(semanticFamilies([...vectors.keys()].reverse(), vectors, { threshold: .5 }).map(family => [family.key, family.label, family.members]))
			.toEqual([["family:graph+query", "graph · query", ["x1", "x2"]], ["family:graph+query#2", "graph · query", ["y1", "y2"]]]);
	});
	test("semantic neighbours are symmetric-deduplicated and canonical", () => {
		const { vectors } = graphEmbedding(planted()), ids = [...vectors.keys()].reverse(), pairs = result => result.map(({ a, b }) => [a, b]);
		expect(pairs(semanticNeighbours(ids, vectors))).toEqual([["a1", "a2"], ["a1", "a3"], ["a2", "a3"], ["b1", "b2"], ["b1", "b3"], ["b2", "b3"]]);
		expect(pairs(semanticNeighbours(ids, vectors, { k: 1 }))).toEqual([["a1", "a2"], ["a1", "a3"], ["b1", "b2"], ["b1", "b3"]]);
		expect(semanticNeighbours(ids, vectors, { minimum: .5 })).toEqual([]);
		for (const { similarity } of semanticNeighbours(ids, vectors)) expect(similarity).toBeCloseTo(cosine(vectors.get("a1"), vectors.get("a2")), 12);
	});
});
