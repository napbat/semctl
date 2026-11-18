import type { GraphData } from "./graph";
import type { GraphAppearance } from "./graph-appearance";
import { affinityWeight, requestAffinity, type AffinityStatus } from "./graph-affinity";
import { bindConceptFamily } from "./graph-data/doc-concepts.js";
import { bindTermVector, graphEmbedding } from "./graph-data/embedding.js";
import { bindAffinity, relationKind } from "./graph-data/layout.js";
import { normalizeConceptPath } from "./graph-data/palette.js";
import { homeFolder } from "./graph-data/regions.js";
import { partitionSubconcepts } from "./graph-data/subconcepts.js";
import { indexingCounts } from "./graph-indexing";
import { memberFolder, summarize, type GraphGroup, type ProjectSummary } from "./graph-projection";

/** A group of at most this many nodes draws its members; a larger group draws subconcept dots that open the next level. */
export const SCOPE_NODE_LIMIT = 24;
const partitionLimits = { calm: 6, balanced: 8, detailed: 12 } as const;
const compare = (a: string, b: string) => a < b ? -1 : a > b ? 1 : 0;

/** Semantic affinity state of the scope's files. "none": no request, for example more than 64 files or no runner. */
export interface ScopeSemantic { status: "none" | AffinityStatus; files: number; indexed: number; pairs: number; error?: string }
export interface ScopeView {
	group: GraphGroup;
	/** Subconcept dots, or the member nodes inside subconcept territories. */
	drawing: GraphData;
	/** Present when the drawing shows subconcept dots. */
	summary?: ProjectSummary;
	/** One child per subconcept, largest first. A child opens as the next scope level. */
	children: GraphGroup[];
	semantic: ScopeSemantic;
}

/**
 * The organized content of one group. Members split into subconcepts: child folders first, then content clusters of
 * heading and identifier terms, recorded links and semctx semantic affinity. The result is deterministic for one input.
 * Affinity is requested for 2 to 64 member files; until it settles, the view uses terms and links only.
 */
export function scopeView(data: GraphData, group: GraphGroup, codebaseId: string | null, detail: GraphAppearance["detail"]): ScopeView {
	const { vectors } = graphEmbedding(data), ids = new Set(group.members.map(node => node.id));
	const files = new Map<string, string>();
	for (const node of group.members) {
		const path = node.category === "file" ? normalizeConceptPath(node.hit?.path ?? node.path) : undefined;
		if (path && !files.has(path)) files.set(path, node.id);
	}
	const state = requestAffinity(codebaseId, [...files.keys()]), affinity = state?.affinity;
	const raw = (affinity?.pairs ?? []).flatMap(pair => {
		const a = files.get(pair.a), b = files.get(pair.b), weight = affinityWeight(pair);
		return a && b && weight > 0 ? [{ a, b, weight }] : [];
	});
	// Hub files appear in most search profiles, so most pairs share some results. The median pair, absent pairs included, is the baseline.
	const total = files.size * (files.size - 1) / 2, sorted = raw.map(pair => pair.weight).sort((a, b) => a - b);
	const base = sorted[Math.floor(total / 2) - (total - sorted.length)] ?? 0, top = sorted.at(-1) ?? 0;
	const pairs = top > base ? raw.flatMap(pair => pair.weight > base ? [{ ...pair, weight: (pair.weight - base) / (top - base) }] : []) : [];
	const weights = new Map(pairs.map(pair => [pair.a < pair.b ? `${pair.a}\u0000${pair.b}` : `${pair.b}\u0000${pair.a}`, pair.weight]));
	const semanticOf = (a: string, b: string) => weights.get(a < b ? `${a}\u0000${b}` : `${b}\u0000${a}`) ?? 0;
	const edges = data.edges.filter(edge => ids.has(edge.from) && ids.has(edge.to));
	const counts = new Map<string, { a: string; b: string; weight: number }>();
	for (const edge of edges) {
		const kind = relationKind(edge);
		if (edge.from === edge.to || (kind !== "import" && kind !== "reference")) continue;
		const [a, b] = edge.from < edge.to ? [edge.from, edge.to] : [edge.to, edge.from], key = `${a}\u0000${b}`;
		const link = counts.get(key) ?? { a, b, weight: 0 };
		link.weight++; counts.set(key, link);
	}
	const parts = partitionSubconcepts(group.members.map(node => ({ id: node.id, name: normalizeConceptPath(node.hit?.path ?? node.path) ?? node.id,
		folder: memberFolder(node) ?? "", vector: vectors.get(node.id) })), { affinity: pairs, links: [...counts.values()], limit: partitionLimits[detail] });
	const byId = new Map(group.members.map(node => [node.id, node]));
	const children = parts.map((part): GraphGroup => {
		const members = part.members.map(id => byId.get(id)!).sort((a, b) => compare(a.path ?? a.label, b.path ?? b.label) || compare(a.id, b.id));
		const home = homeFolder(members.map(memberFolder));
		return { id: JSON.stringify([group.id, part.key]), label: part.label, members, internal: 0, indexing: indexingCounts(members), terms: part.terms, home,
			...(part.kind === "folder" ? { folder: part.folder ?? home } : {}) };
	});
	const semantic: ScopeSemantic = { status: state?.status ?? "none", files: files.size, pairs: pairs.length,
		indexed: affinity ? [...affinity.files.values()].filter(file => file.indexed).length : 0, ...(state?.error ? { error: state.error } : {}) };
	if (group.members.length <= SCOPE_NODE_LIMIT) {
		// Copies carry the drawing-only bindings, so other views of the same nodes keep their own families.
		const kinds = new Map(parts.filter(part => part.members.length > 1).flatMap(part => part.members.map(id => [id, part] as const)));
		const nodes = group.members.map(node => {
			const copy = { ...node }, part = kinds.get(node.id), vector = vectors.get(node.id);
			if (vector) bindTermVector(copy, vector);
			if (part) bindConceptFamily(copy, { key: part.key, label: part.label, kind: part.kind === "folder" ? "path" : "content" });
			return copy;
		});
		const drawing: GraphData = { ...data, title: group.label, nodes, edges, roots: data.roots.filter(id => ids.has(id)) };
		if (pairs.length) bindAffinity(drawing, semanticOf);
		return { group, drawing, children, semantic };
	}
	const scoped: GraphData = { ...data, title: group.label, nodes: group.members, edges, roots: data.roots.filter(id => ids.has(id)) };
	const summary = summarize(scoped, children, { codebaseId, detail, title: group.label, regions: false, vectors,
		own: child => child.folder === undefined ? { key: child.id, label: child.label } : undefined });
	if (pairs.length) {
		const members = new Map(children.map(child => [child.id, child.members.map(member => member.id)]));
		bindAffinity(summary.graph, (a, b) => {
			const left = members.get(a) ?? [], right = members.get(b) ?? [];
			let sum = 0; for (const x of left) for (const y of right) sum += semanticOf(x, y);
			return left.length && right.length ? sum / (left.length * right.length) : 0;
		});
	}
	return { group, drawing: summary.graph, summary, children, semantic };
}
