// Functional communities inferred only from resolved file references and weak import support.
// Sparse modularity moves are bounded by passes, not by all-pairs file comparisons.
import { normalizeConceptPath } from "./palette.js";
const compare = (a, b) => a < b ? -1 : a > b ? 1 : 0;
const file = node => node.category === "file";
const displayName = symbol => symbol.replace(/([a-z\d])([A-Z])/g, "$1 $2").replace(/[_-]+/g, " ").trim();
export function inferConcepts(data) {
	const files = data.nodes.filter(file), nodesById = new Map(files.map(node => [node.id, node])), paths = new Map();
	for (const node of files) {
		const path = normalizeConceptPath(node.path);
		if (path) paths.set(path, (paths.get(path) ?? 0) + 1);
	}
	const ids = new Set(files.filter(node => {
		const path = normalizeConceptPath(node.path);
		return path && paths.get(path) === 1 && (!node.hit?.codebaseId || node.hit.codebaseId === data.identity?.codebaseId);
	}).map(node => node.id));
	// Transport IDs can be ordinal. File identity, not transport order, owns ties and keys.
	const identity = id => normalizeConceptPath(nodesById.get(id)?.path) ?? id;
	const compareId = (a, b) => compare(identity(a), identity(b)) || compare(a, b);
	files.sort((a, b) => compareId(a.id, b.id));
	const references = data.edges.map((edge, index) => ({ edge, index })).filter(({ edge }) => edge.kind === "reference" && typeof edge.symbol === "string" && edge.symbol.trim() && typeof edge.moniker === "string" && edge.moniker.trim() && ids.has(edge.from) && ids.has(edge.to) && edge.from !== edge.to)
		.sort((a, b) => compareId(a.edge.from, b.edge.from) || compareId(a.edge.to, b.edge.to) || compare(a.edge.moniker ?? a.edge.symbol, b.edge.moniker ?? b.edge.symbol) || compare(a.edge.symbol, b.edge.symbol));
	const empty = reason => ({ available: false, reason, groups: [], assignments: new Map(), unassigned: files.map(node => node.id), stats: { passes: 0, visits: 0 } });
	if (!references.length) return empty("Concepts need resolved references between indexed files with real symbol identities. This result has no such evidence; Structure remains available.");
	const identityFiles = new Map(), nameFiles = new Map(), supported = new Set();
	for (const { edge } of references) {
		const identity = edge.moniker ?? edge.symbol;
		if (!identityFiles.has(identity)) identityFiles.set(identity, new Set());
		if (!nameFiles.has(edge.symbol)) nameFiles.set(edge.symbol, new Set());
		for (const id of [edge.from, edge.to]) { identityFiles.get(identity).add(id); nameFiles.get(edge.symbol).add(id); supported.add(id); }
	}
	const idf = count => Math.log1p(files.length / (1 + count));
	const weighted = references.map(item => ({ ...item, weight: idf(identityFiles.get(item.edge.moniker ?? item.edge.symbol).size) * idf(nameFiles.get(item.edge.symbol).size) }));
	const adjacency = new Map([...supported].sort(compareId).map(id => [id, new Map()]));
	function add(a, b, weight) { const neighbours = adjacency.get(a); neighbours.set(b, (neighbours.get(b) ?? 0) + weight); }
	for (const { edge, weight } of weighted) { add(edge.from, edge.to, weight); add(edge.to, edge.from, weight); }
	for (const edge of [...data.edges].sort((a, b) => compareId(a.from, b.from) || compareId(a.to, b.to))) if ((edge.kind === "import" || (!edge.kind && edge.label === "imports")) && edge.from !== edge.to && supported.has(edge.from) && supported.has(edge.to)) { add(edge.from, edge.to, .12); add(edge.to, edge.from, .12); }
	const degree = new Map(), totals = new Map(), communities = new Map(); let mass = 0;
	for (const [id, neighbours] of adjacency) { const value = [...neighbours.values()].reduce((sum, weight) => sum + weight, 0); degree.set(id, value); totals.set(id, value); communities.set(id, id); mass += value; }
	const stats = { passes: 0, visits: 0 };
	for (let pass = 0; pass < 16; pass++) {
		let moved = false; stats.passes++;
		for (const [id, neighbours] of adjacency) {
			const old = communities.get(id), weights = new Map();
			for (const [other, weight] of neighbours) { stats.visits++; const community = communities.get(other); weights.set(community, (weights.get(community) ?? 0) + weight); }
			totals.set(old, totals.get(old) - degree.get(id));
			const score = community => (weights.get(community) ?? 0) - degree.get(id) * (totals.get(community) ?? 0) / mass;
			let best = old, bestScore = score(old);
			for (const community of [...weights.keys()].sort(compareId)) { const value = score(community); if (value > bestScore + 1e-10) { best = community; bestScore = value; } }
			communities.set(id, best); totals.set(best, (totals.get(best) ?? 0) + degree.get(id)); if (best !== old) moved = true;
		}
		if (!moved) break;
	}
	const members = new Map();
	for (const [id, community] of communities) { if (!members.has(community)) members.set(community, []); members.get(community).push(id); }
	const evidence = new Map();
	for (const item of weighted) {
		const community = communities.get(item.edge.from); if (community !== communities.get(item.edge.to)) continue;
		if (!evidence.has(community)) evidence.set(community, { symbols: new Map(), edges: [] });
		const entry = evidence.get(community), identity = item.edge.moniker ?? item.edge.symbol;
		if (!entry.symbols.has(identity)) entry.symbols.set(identity, { symbol: item.edge.symbol, moniker: item.edge.moniker ?? null, count: 0, weight: 0 });
		const symbol = entry.symbols.get(identity); symbol.count++; symbol.weight += item.weight; entry.edges.push({ index: item.index, weight: item.weight, from: item.edge.from, to: item.edge.to, symbol: item.edge.symbol });
	}
	const groups = [], assignments = new Map();
	for (const [community, values] of members) {
		const entry = evidence.get(community); if (values.length < 2 || !entry?.edges.length) continue;
		values.sort(compareId);
		const symbols = [...entry.symbols.values()].sort((a, b) => b.weight - a.weight || compare(a.symbol, b.symbol) || compare(a.moniker ?? "", b.moniker ?? ""));
		entry.edges.sort((a, b) => b.weight - a.weight || compareId(a.from, b.from) || compareId(a.to, b.to) || compare(a.symbol, b.symbol));
		const memberNodes = values.map(id => nodesById.get(id)), projects = [...new Set(memberNodes.map(node => node.project).filter(value => typeof value === "string"))].sort(compare);
		const directories = [...new Set(memberNodes.map(node => (node.path ?? "").replace(/\\/g, "/").split("/").slice(0, -1).join("/") || "."))].sort(compare);
		const group = { key: `concept:${JSON.stringify(values.map(identity))}`, type: "Inferred concept", members: values, symbols, projects, directories, relationCount: entry.edges.length, edges: entry.edges.map(item => item.index) };
		groups.push(group); for (const id of values) assignments.set(id, group);
	}
	groups.sort((a, b) => compare(a.key, b.key));
	const frequency = new Map();
	for (const group of groups) for (const name of new Set(group.symbols.map(item => item.symbol))) frequency.set(name, (frequency.get(name) ?? 0) + 1);
	for (const group of groups) {
		const distinguishing = item => item.weight * Math.log1p(groups.length / (1 + frequency.get(item.symbol)));
		group.symbols.sort((a, b) => distinguishing(b) - distinguishing(a) || compare(a.symbol, b.symbol));
		group.label = [...new Set(group.symbols.map(item => item.symbol))].slice(0, 2).map(displayName).join(" · ");
	}
	const labels = new Map();
	for (const group of groups) labels.set(group.label, (labels.get(group.label) ?? 0) + 1);
	for (const group of groups) if (labels.get(group.label) > 1) {
		// A factual path disambiguates repeated identifiers; it is not a semantic label.
		group.label += ` · ${group.directories.length === 1 ? group.directories[0] : nodesById.get(group.members[0]).path ?? group.members[0]}`;
	}
	if (!groups.length) return { ...empty("Resolved references exist, but no multi-file community has internal symbol evidence. Unsupported files are not assigned a concept."), stats };
	return { available: true, reason: "Inferred from actual resolved-reference/import topology, with shared identifiers downweighted by inverse file frequency. Not embedding similarity or semantic proof.", groups, assignments, unassigned: files.filter(node => !assignments.has(node.id)).map(node => node.id), stats };
}
