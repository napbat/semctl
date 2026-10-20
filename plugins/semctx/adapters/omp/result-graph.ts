import type { GraphData } from "./graph";
import type { Display, Hit } from "./inspector";
import { bindGraphGroupColors } from "./graph-colors";

/** Organize returned occurrences by their supplied file identity, not inferred dependencies. */
export function resultGraph(display: Display): GraphData | undefined {
	if (display.kind !== "search" && display.kind !== "batch") return undefined;
	const hits = display.kind === "search" ? display.hits : display.symbols.flatMap(group => group.hits);
	if (hits.length < 2) return undefined;
	const located = hits.filter(hit => hit.path);
	const locationKey = (hit: Hit) => JSON.stringify([hit.codebaseId ?? display.identity.codebaseId, hit.path, hit.startByte, hit.startLine, hit.qualifiedName ?? hit.symbol]);
	if (new Set(located.map(locationKey)).size < 2) return undefined;
	const nodes: GraphData["nodes"] = [], edges: GraphData["edges"] = [], roots: string[] = [];
	const files = new Map<string, { node: GraphData["nodes"][number]; members: GraphData["nodes"] }>();
	for (const [index, hit] of hits.entries()) {
		const id = `match:${index}`;
		const location = hit.path ? `${hit.path}${hit.startLine ? `:${hit.startLine}` : ""}` : "Location not supplied";
		const match = { id, label: hit.symbol ?? hit.qualifiedName ?? location, detail: location, hit };
		nodes.push(match);
		if (!hit.path) { roots.push(id); continue; }
		const codebase = hit.codebaseId ?? display.identity.codebaseId;
		const fileKey = JSON.stringify([codebase, hit.path]);
		let file = files.get(fileKey);
		if (!file) {
			const node: GraphData["nodes"][number] = { id: `file:${files.size}`, label: hit.path.replace(/\\/g, "/").split("/").at(-1) || hit.path,
				detail: `${hit.path}${codebase ? `\nCodebase: ${codebase}` : ""}`, hit: null };
			file = { node, members: [] }; files.set(fileKey, file); roots.push(node.id); nodes.push(node);
		}
		file.members.push(match);
		edges.push({ from: file.node.id, to: id, label: "contains returned match", kind: "containment" });
	}
	for (const file of files.values()) bindGraphGroupColors(file.node, display.identity.codebaseId, file.members);
	return {
		kind: "graph", title: "Matches by file", query: display.kind === "search" ? display.query : null,
		nodes, edges, roots, truncated: false,
		caveats: [],
	};
}
