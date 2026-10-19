import { lookupConcept } from "./concept-colors";
import type { GraphData } from "./graph";
import { category, relationKind, relations, type GraphRelationKind } from "./graph-data/layout.js";

const compare = (a: string, b: string) => a < b ? -1 : a > b ? 1 : 0;
export interface GraphGroup { id: string; label: string; path?: string; members: GraphData["nodes"]; internal: number }
export interface ProjectSummary {
	graph: GraphData;
	groups: GraphGroup[];
	allGroups: GraphGroup[];
	groupOf: Map<string, string>;
	files: number;
	drawnLinks: number;
	omitted: number;
}

/** Every source node remains reachable, including groups folded into the overview's remainder. */
export function projectSummary(data: GraphData, codebaseId: string | null = null): ProjectSummary {
	const byGroup = new Map<string, GraphGroup>(), originalOf = new Map<string, string>();
	for (const node of data.nodes) {
		const codebase = node.hit?.codebaseId ?? codebaseId;
		const concept = lookupConcept(codebase, node.hit?.path ?? node.path), structure = category(node);
		const id = JSON.stringify([codebase, concept?.key ?? `structure:${structure.key}`]);
		let group = byGroup.get(id);
		if (!group) {
			group = { id, label: concept?.label ?? structure.label, path: concept ? node.hit?.path ?? node.path : undefined, members: [], internal: 0 };
			byGroup.set(id, group);
		}
		group.members.push(node); originalOf.set(node.id, id);
	}
	const allGroups = [...byGroup.values()].sort((a, b) => b.members.length - a.members.length || compare(a.id, b.id));
	for (const group of allGroups) group.members.sort((a, b) => compare(a.path ?? a.label, b.path ?? b.label) || compare(a.id, b.id));
	const rest = allGroups.slice(15);
	const groups: GraphGroup[] = allGroups.length <= 16 ? allGroups : [...allGroups.slice(0, 15), {
		id: "other-groups", label: `${rest.length} other groups`, members: rest.flatMap(group => group.members), internal: 0,
	}];
	const groupOf = new Map<string, string>(), retained = new Map(groups.map(group => [group.id, group]));
	for (const group of groups) for (const node of group.members) groupOf.set(node.id, group.id);
	const links = new Map<string, { from: string; to: string; kind: GraphRelationKind; count: number }>();
	for (const edge of data.edges) {
		const original = originalOf.get(edge.from)!;
		if (original === originalOf.get(edge.to)) byGroup.get(original)!.internal++;
		const from = groupOf.get(edge.from)!, to = groupOf.get(edge.to)!;
		if (from === to) { if (from === "other-groups") retained.get(from)!.internal++; continue; }
		const kind = relationKind(edge), key = JSON.stringify([from, to, kind]);
		const link = links.get(key);
		if (link) link.count++; else links.set(key, { from, to, kind, count: 1 });
	}
	const ranked = [...links.values()].sort((a, b) => b.count - a.count || compare(a.from, b.from) || compare(a.to, b.to) || compare(a.kind, b.kind));
	const visible = ranked.slice(0, 48), omitted = ranked.slice(48).reduce((sum, link) => sum + link.count, 0);
	const drawnLinks = visible.reduce((sum, link) => sum + link.count, 0);
	const graph: GraphData = {
		kind: "graph", title: "Project overview", query: null,
		nodes: groups.map(group => ({ id: group.id, label: group.label, category: "group", path: group.path, hit: null,
			detail: `${group.members.length} indexed nodes · ${group.internal} internal links` })),
		edges: visible.map(link => ({ from: link.from, to: link.to, label: `${link.count} ${relations[link.kind].label.toLowerCase()} links`,
			// Aggregate counts are not individual symbol bindings.
			kind: link.kind === "reference" || link.kind === "generic" ? undefined : link.kind })),
		roots: [...new Set(data.roots.map(id => groupOf.get(id)!))],
		caveats: [...data.caveats, ...(data.truncated ? ["The indexed overview is incomplete."] : []),
			...(omitted ? [`${omitted} cross-group links are not drawn. Select a group or node to inspect exact relationships.`] : [])],
		truncated: data.truncated || omitted > 0,
	};
	return { graph, groups, allGroups, groupOf, files: data.nodes.filter(node => node.category === "file").length, drawnLinks, omitted };
}

/** A bounded drawing, not a replacement for the complete native relationship navigator. */
export function graphWindow(data: GraphData, members: readonly GraphData["nodes"][number][], selected?: string): GraphData {
	const nodes = members.slice(0, 48), ids = new Set(nodes.map(node => node.id));
	const candidates = data.edges.filter(edge => ids.has(edge.from) && ids.has(edge.to));
	const edges = candidates.slice(0, 128), omittedNodes = members.length - nodes.length, omittedEdges = candidates.length - edges.length;
	return { ...data, nodes, edges, roots: selected && ids.has(selected) ? [selected] : data.roots.filter(id => ids.has(id)),
		truncated: data.truncated || omittedNodes > 0 || omittedEdges > 0,
		caveats: [...data.caveats, ...(omittedNodes ? [`${omittedNodes} nodes are outside this drawing. Every node remains in the native list.`] : []),
			...(omittedEdges ? [`${omittedEdges} links are outside this drawing. Exact relationships remain in node details.`] : [])] };
}
