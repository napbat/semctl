import { groupConcept, lookupConcept, type ConceptColor } from "./concept-colors";
import type { GraphData } from "./graph";
import type { GraphAppearance } from "./graph-appearance";
import { bindGraphGroupColors, type OwnColor } from "./graph-colors";
import { bindConceptFamily, type DocConceptFamily } from "./graph-data/doc-concepts.js";
import { bindKeyTerms, bindTermVector, centroid, distinctiveTerms, graphEmbedding, type TermVector } from "./graph-data/embedding.js";
import { bindDrawingWeight, bindRemainder, category, relationKind, relations, type GraphRelationKind } from "./graph-data/layout.js";
import { homeFolder, mapRegions, type MapRegionKind } from "./graph-data/regions.js";
import { normalizeConceptPath } from "./graph-data/palette.js";
import { bindIndexingCounts, groupIndexing, indexingCounts, indexingSummary, type GraphIndexingCounts } from "./graph-indexing";

const compare = (a: string, b: string) => a < b ? -1 : a > b ? 1 : 0;
const summaryLimits = { calm: { nodes: 12, routes: 24 }, balanced: { nodes: 16, routes: 48 }, detailed: { nodes: 24, routes: 72 } } as const;
/** The ID of the folded remainder group. */
export const REMAINDER_GROUP = "other-groups";
/** A drawn set of groups. It organizes the map and the group list; it does not prove a dependency. */
export interface GraphRegion { key: string; label: string; kind: MapRegionKind; folder?: string; groups: string[] }
/**
 * `source` and `family` describe a doc concept group. `terms` are distinctive identifier terms of the members, not a summary.
 * `home` is the deepest folder that holds at least 60% of the member folders. `folder` is set only for source-structure groups.
 */
export interface GraphGroup {
	id: string; label: string; path?: string; source?: string; family?: DocConceptFamily; members: GraphData["nodes"]; internal: number; indexing: GraphIndexingCounts;
	terms: string[]; home: string; folder?: string; region?: GraphRegion;
}
export interface ProjectSummary {
	graph: GraphData;
	groups: GraphGroup[];
	allGroups: GraphGroup[];
	/** Drawn regions, largest first. Each holds at least 2 drawn groups; the remainder never belongs to a region. */
	regions: GraphRegion[];
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

/** A file contributes its folder; a directory or project node contributes itself. Other nodes have no folder. */
export function memberFolder(node: GraphData["nodes"][number]): string | undefined {
	const source = node.hit?.path ?? node.path;
	if ((node.category === "directory" || node.category === "project") && source === "") return "";
	const path = normalizeConceptPath(source);
	if (path === undefined) return undefined;
	return node.category === "directory" || node.category === "project" ? path : path.slice(0, Math.max(0, path.lastIndexOf("/")));
}

/** Every source node remains reachable, including groups folded into the overview's remainder. */
export function projectSummary(data: GraphData, codebaseId: string | null = null, detail: GraphAppearance["detail"] = "balanced"): ProjectSummary {
	const { vectors } = graphEmbedding(data);
	const byGroup = new Map<string, GraphGroup>(), concepts = new Map<string, ConceptColor>();
	for (const node of data.nodes) {
		const codebase = node.hit?.codebaseId ?? codebaseId;
		const assigned = lookupConcept(codebase, node.hit?.path ?? node.path), structure = category(node);
		const concept = assigned && groupConcept(assigned);
		const id = JSON.stringify([codebase, concept?.key ?? `structure:${structure.key}`]);
		let group = byGroup.get(id);
		if (!group) {
			const doc = concept?.kind === "doc" ? { source: concept.source, ...(concept.family ? { family: concept.family } : {}) } : {};
			group = { id, label: concept?.label ?? structure.label, ...doc, members: [], internal: 0, indexing: indexingCounts([]), terms: [], home: "" };
			byGroup.set(id, group);
			if (concept) concepts.set(id, concept);
		}
		group.members.push(node);
	}
	const allGroups = [...byGroup.values()].sort((a, b) => b.members.length - a.members.length || compare(a.id, b.id));
	for (const group of allGroups) {
		group.members.sort((a, b) => compare(a.path ?? a.label, b.path ?? b.label) || compare(a.id, b.id));
		if (concepts.has(group.id)) group.path = group.members[0].hit?.path ?? group.members[0].path;
		group.indexing = indexingCounts(group.members);
		group.home = homeFolder(group.members.map(memberFolder));
		if (!concepts.has(group.id)) group.folder = group.home;
	}
	qualifyStructureLabels(allGroups, concepts);
	// Rarity is measured against every group, so the folding budget never changes a group's key terms.
	const terms = distinctiveTerms(allGroups.map(group => ({ id: group.id, vectors: group.members.map(member => vectors.get(member.id)), exclude: [group.label] })));
	for (const group of allGroups) group.terms = terms.get(group.id) ?? [];
	return summarize(data, allGroups, { codebaseId, detail, title: "Project overview", regions: true,
		// Equal weights: the doc's own terms and the terms of the code that it names.
		vector: (group, members) => { const concept = concepts.get(group.id); return concept?.kind === "doc" && concept.terms ? centroid([concept.terms, members]) : members; } });
}

interface SummaryOptions {
	codebaseId: string | null;
	detail: GraphAppearance["detail"];
	title: string;
	/** Map regions organize the project overview. A drill-down already shows one organized group. */
	regions: boolean;
	/** The bound term vector of a group dot. The default is the centroid of its member vectors. */
	vector?: (group: GraphGroup, members: TermVector) => TermVector;
	/** Its own palette color, for a content subconcept whose members share one folder color. */
	own?: (group: GraphGroup) => OwnColor | undefined;
	/** Member vectors from a larger corpus, for example the whole overview of a drill-down. The default embeds `data`. */
	vectors?: ReadonlyMap<string, TermVector | undefined>;
}

/**
 * One drawn dot per group, in the given size order. Groups beyond the detail budget fold into one remainder.
 * Every edge between two members counts once: internal to one group, drawn, or omitted.
 */
export function summarize(data: GraphData, allGroups: GraphGroup[], options: SummaryOptions): ProjectSummary {
	const limits = summaryLimits[options.detail], vectors = options.vectors ?? graphEmbedding(data).vectors, { codebaseId } = options;
	const originalOf = new Map<string, GraphGroup>();
	for (const group of allGroups) for (const node of group.members) originalOf.set(node.id, group);
	const rest = allGroups.slice(limits.nodes - 1), foldedMembers = allGroups.length > limits.nodes ? rest.flatMap(group => group.members) : [];
	let remainderLabel = `${rest.length} other groups`;
	while (allGroups.some(group => group.label === remainderLabel)) remainderLabel += " · remainder";
	const groups: GraphGroup[] = allGroups.length <= limits.nodes ? allGroups : [...allGroups.slice(0, limits.nodes - 1), {
		id: REMAINDER_GROUP, label: remainderLabel, members: foldedMembers, internal: 0, indexing: indexingCounts(foldedMembers),
		terms: [], home: homeFolder(foldedMembers.map(memberFolder)),
	}];
	const groupOf = new Map<string, string>(), retained = new Map(groups.map(group => [group.id, group]));
	for (const group of groups) for (const node of group.members) groupOf.set(node.id, group.id);
	const links = new Map<string, { from: string; to: string; kind: GraphRelationKind; count: number }>();
	for (const edge of data.edges) {
		const original = originalOf.get(edge.from), from = groupOf.get(edge.from), to = groupOf.get(edge.to);
		if (!original || from === undefined || to === undefined) continue;
		if (original === originalOf.get(edge.to)) original.internal++;
		if (from === to) { if (from === REMAINDER_GROUP) retained.get(from)!.internal++; continue; }
		const kind = relationKind(edge), key = JSON.stringify([from, to, kind]);
		const link = links.get(key);
		if (link) link.count++; else links.set(key, { from, to, kind, count: 1 });
	}
	// The remainder joins unrelated small groups, so its routes would only converge on one arbitrary dot.
	const ranked = [...links.values()].sort((a, b) => b.count - a.count || compare(a.from, b.from) || compare(a.to, b.to) || compare(a.kind, b.kind));
	const drawable = ranked.filter(link => link.from !== REMAINDER_GROUP && link.to !== REMAINDER_GROUP);
	const visible = drawable.slice(0, limits.routes), drawnLinks = visible.reduce((sum, link) => sum + link.count, 0);
	const omitted = ranked.reduce((sum, link) => sum + link.count, 0) - drawnLinks;
	const groupVectors = new Map(groups.map(group => [group.id, centroid(group.members.map(member => vectors.get(member.id)))]));
	let regions: GraphRegion[] = [];
	if (options.regions) {
		// Imports and resolved references show how groups work together. Containment and project membership repeat the folder evidence.
		const evidence = [...links.values()].filter(link => link.kind === "import" || link.kind === "reference").map(link => ({ a: link.from, b: link.to, weight: link.count }));
		const mapped = mapRegions(groups.filter(group => group.id !== REMAINDER_GROUP).map(group => ({ id: group.id, home: group.home, vector: groupVectors.get(group.id),
			...(group.family ? { family: group.family } : {}), weight: group.members.length })), evidence);
		regions = mapped.regions.map((region): GraphRegion => ({ key: region.key, label: region.label, kind: region.kind,
			...(region.folder !== undefined ? { folder: region.folder } : {}), groups: region.members }));
		for (const region of regions) for (const id of region.groups) retained.get(id)!.region = region;
	}
	const graph: GraphData = {
		kind: "graph", title: options.title, query: null,
		nodes: groups.map(group => {
			const members = groupVectors.get(group.id)!;
			const node: GraphData["nodes"][number] = { id: group.id, label: group.label, category: "group", path: group.path, hit: null,
				indexing: groupIndexing(group.indexing),
				detail: [`${group.members.length} indexed nodes · ${group.internal} internal links`, group.source ? `From ${group.source}` : undefined,
					indexingSummary(group.indexing)].filter(Boolean).join("\n") };
			bindGraphGroupColors(node, codebaseId, group.members, group.folder, options.own?.(group));
			bindTermVector(node, options.vector?.(group, members) ?? members);
			if (group.region) bindConceptFamily(node, { key: group.region.key, label: group.region.label, kind: group.region.kind });
			bindKeyTerms(node, group.terms);
			if (group.id === REMAINDER_GROUP) bindRemainder(node);
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
		roots: [...new Set(data.roots.flatMap(id => groupOf.get(id) ?? []))].filter(id => id !== REMAINDER_GROUP),
		caveats: [...data.caveats, ...(data.truncated ? ["The indexed overview is incomplete."] : []),
			...(omitted ? [`${omitted} cross-group links are not drawn. Select a group or node to inspect exact relationships.`] : [])],
		truncated: data.truncated || omitted > 0,
		...(data.loading !== undefined ? { loading: data.loading } : {}),
	};
	return { graph, groups, allGroups, regions, groupOf, files: data.nodes.filter(node => node.category === "file").length, drawnLinks, omitted };
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
