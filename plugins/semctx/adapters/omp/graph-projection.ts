import { groupConcept, lookupConcept, type ConceptColor } from "./concept-colors";
import type { GraphData } from "./graph";
import type { GraphAppearance } from "./graph-appearance";
import { bindGraphGroupColors } from "./graph-colors";
import { bindConceptFamily, type DocConceptFamily } from "./graph-data/doc-concepts.js";
import { bindTermVector, centroid, graphEmbedding } from "./graph-data/embedding.js";
import { bindDrawingWeight, category, relationKind, relations, type GraphRelationKind } from "./graph-data/layout.js";
import { bindIndexingCounts, groupIndexing, indexingCounts, indexingSummary, type GraphIndexingCounts } from "./graph-indexing";

const compare = (a: string, b: string) => a < b ? -1 : a > b ? 1 : 0;
const summaryLimits = { calm: { nodes: 12, routes: 24 }, balanced: { nodes: 16, routes: 48 }, detailed: { nodes: 24, routes: 72 } } as const;
/** `source` and `family` describe a doc concept group. */
export interface GraphGroup { id: string; label: string; path?: string; source?: string; family?: DocConceptFamily; members: GraphData["nodes"]; internal: number; indexing: GraphIndexingCounts }
export interface ProjectSummary {
	graph: GraphData;
	groups: GraphGroup[];
	allGroups: GraphGroup[];
	groupOf: Map<string, string>;
	files: number;
	drawnLinks: number;
	omitted: number;
}

/** Keep concept names intact. Repeated structure names need the shortest distinguishing source prefix. */
function qualifyStructureLabels(groups: readonly GraphGroup[], concepts: ReadonlyMap<string, ConceptColor>): void {
	const counts = new Map<string, number>();
	for (const group of groups) counts.set(group.label, (counts.get(group.label) ?? 0) + 1);
	const ambiguous = groups.filter(group => counts.get(group.label)! > 1 && !concepts.has(group.id)).sort((a, b) => compare(a.id, b.id));
	const renamed = new Set(ambiguous), taken = new Set(groups.filter(group => !renamed.has(group)).map(group => group.label));
	const prefixes = new Map<GraphGroup, string[]>(), labels = new Map(ambiguous.map(group => [group, group.label]));
	for (const group of ambiguous) {
		const member = group.members[0], project = (member.project ?? member.hit?.project ?? "").replace(/\\/g, "/").replace(/^\.\/|\/+$/g, "");
		let prefix = project !== group.label ? project : "";
		if (!prefix) {
			const path = (member.hit?.path ?? member.path ?? "").replace(/\\/g, "/").split("/");
			if (member.category !== "directory" && member.category !== "project") path.pop();
			const branch = group.label.split("/");
			const start = path.findIndex((_, index) => branch.every((part, offset) => path[index + offset] === part));
			if (start > 0) prefix = path.slice(0, start).join("/");
		}
		const kind = group.members.some(node => node.category === "file") ? "Files"
			: member.category === "directory" ? "Directory" : member.category === "project" ? "Project" : category(member).type || "Structure";
		prefixes.set(group, prefix ? prefix.split("/") : [kind]);
	}
	for (const group of ambiguous) {
		const parts = prefixes.get(group)!, label = labels.get(group)!;
		let qualified: string | undefined;
		for (let depth = 1; depth <= parts.length; depth++) {
			const prefix = parts.slice(0, depth).join("/"), candidate = `${prefix} · ${label}`;
			const shared = ambiguous.some(other => {
				if (other === group || labels.get(other) !== label) return false;
				const peer = prefixes.get(other)!;
				// A complete shorter prefix keeps its name; a longer one must show its next segment.
				return peer.length >= depth && peer.slice(0, depth).join("/") === prefix && (depth < parts.length || peer.length === parts.length);
			});
			if (!shared && !taken.has(candidate)) { qualified = candidate; break; }
		}
		const base = `${parts.join("/")} · ${label}`;
		let ordinal = 1;
		qualified ??= base;
		while (taken.has(qualified)) qualified = `${base} (${++ordinal})`;
		group.label = qualified;
		taken.add(qualified);
	}
}

/** Every source node remains reachable, including groups folded into the overview's remainder. */
export function projectSummary(data: GraphData, codebaseId: string | null = null, detail: GraphAppearance["detail"] = "balanced"): ProjectSummary {
	const limits = summaryLimits[detail], { vectors } = graphEmbedding(data);
	const byGroup = new Map<string, GraphGroup>(), originalOf = new Map<string, string>(), concepts = new Map<string, ConceptColor>();
	for (const node of data.nodes) {
		const codebase = node.hit?.codebaseId ?? codebaseId;
		const assigned = lookupConcept(codebase, node.hit?.path ?? node.path), structure = category(node);
		const concept = assigned && groupConcept(assigned);
		const id = JSON.stringify([codebase, concept?.key ?? `structure:${structure.key}`]);
		let group = byGroup.get(id);
		if (!group) {
			const doc = concept?.kind === "doc" ? { source: concept.source, ...(concept.family ? { family: concept.family } : {}) } : {};
			group = { id, label: concept?.label ?? structure.label, ...doc, members: [], internal: 0, indexing: indexingCounts([]) };
			byGroup.set(id, group);
			if (concept) concepts.set(id, concept);
		}
		group.members.push(node); originalOf.set(node.id, id);
	}
	const allGroups = [...byGroup.values()].sort((a, b) => b.members.length - a.members.length || compare(a.id, b.id));
	for (const group of allGroups) {
		group.members.sort((a, b) => compare(a.path ?? a.label, b.path ?? b.label) || compare(a.id, b.id));
		if (concepts.has(group.id)) group.path = group.members[0].hit?.path ?? group.members[0].path;
		group.indexing = indexingCounts(group.members);
	}
	qualifyStructureLabels(allGroups, concepts);
	const rest = allGroups.slice(limits.nodes - 1), foldedMembers = allGroups.length > limits.nodes ? rest.flatMap(group => group.members) : [];
	let remainderLabel = `${rest.length} other groups`;
	while (allGroups.some(group => group.label === remainderLabel)) remainderLabel += " · remainder";
	const groups: GraphGroup[] = allGroups.length <= limits.nodes ? allGroups : [...allGroups.slice(0, limits.nodes - 1), {
		id: "other-groups", label: remainderLabel, members: foldedMembers, internal: 0, indexing: indexingCounts(foldedMembers),
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
	const visible = ranked.slice(0, limits.routes), omitted = ranked.slice(limits.routes).reduce((sum, link) => sum + link.count, 0);
	const drawnLinks = visible.reduce((sum, link) => sum + link.count, 0);
	const graph: GraphData = {
		kind: "graph", title: "Project overview", query: null,
		nodes: groups.map(group => {
			const concept = concepts.get(group.id), members = centroid(group.members.map(member => vectors.get(member.id)));
			const node: GraphData["nodes"][number] = { id: group.id, label: group.label, category: "group", path: group.path, hit: null,
				indexing: groupIndexing(group.indexing),
				detail: [`${group.members.length} indexed nodes · ${group.internal} internal links`, group.source ? `From ${group.source}` : undefined,
					indexingSummary(group.indexing)].filter(Boolean).join("\n") };
			bindGraphGroupColors(node, codebaseId, group.members);
			// Equal weights: the doc's own terms and the terms of the code that it names.
			bindTermVector(node, concept?.kind === "doc" && concept.terms ? centroid([concept.terms, members]) : members);
			if (concept?.kind === "doc") bindConceptFamily(node, concept.family);
			bindDrawingWeight(node, group.members.length);
			bindIndexingCounts(node, group.indexing);
			return node;
		}),
		edges: visible.map(link => {
			const edge: GraphData["edges"][number] = { from: link.from, to: link.to, label: `${link.count} ${relations[link.kind].label.toLowerCase()} links`,
				// Aggregate counts are not individual symbol bindings.
				kind: link.kind === "reference" || link.kind === "generic" ? undefined : link.kind };
			bindDrawingWeight(edge, link.count);
			return edge;
		}),
		roots: [...new Set(data.roots.map(id => groupOf.get(id)!))],
		caveats: [...data.caveats, ...(data.truncated ? ["The indexed overview is incomplete."] : []),
			...(omitted ? [`${omitted} cross-group links are not drawn. Select a group or node to inspect exact relationships.`] : [])],
		truncated: data.truncated || omitted > 0,
		...(data.loading !== undefined ? { loading: data.loading } : {}),
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
