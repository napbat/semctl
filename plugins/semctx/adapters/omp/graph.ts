import type { Component, DescribeContext, NativeNode, NativeUiEvent } from "@oh-my-pi/pi-tui";
import { nativeGraphColorCue } from "./native-concepts";
import type { Hit } from "./inspector";
import { graphNodeTitle, graphPreview } from "./graph-preview";
import { conceptRevision, subscribeConcepts } from "./concept-colors";
import { graphColorRevision, graphGroupColor, lookupGraphColor } from "./graph-colors";
import { strengths } from "./graph-data/matches.js";
import { matchScoreIndicator, matchVerificationIcon } from "./match-indicators";
import { graphWindow, projectSummary, type GraphGroup, type ProjectSummary } from "./graph-projection";
import { supportsElements } from "./native-elements";
import { indexingSummary, indexingText, type GraphIndexing } from "./graph-indexing";
import type { GraphAppearance } from "./graph-appearance";

export interface GraphData {
	kind: "graph";
	title: string;
	query: string | null;
	nodes: { id: string; label: string; detail: string | null; hit: Hit | null; category?: "file" | "directory" | "project" | "group"; path?: string; project?: string; indexing?: GraphIndexing; indexingError?: string }[];
	edges: { from: string; to: string; label: string; kind?: "containment" | "association" | "import" | "reference"; symbol?: string; moniker?: string }[];
	roots: string[];
	caveats: string[];
	truncated: boolean;
	loading?: boolean;
}
const object = (v: unknown): v is Record<string, unknown> => v !== null && typeof v === "object" && !Array.isArray(v);
const clean = (s: string) => s.replace(/[\x00-\x08\x0b-\x1f\x7f-\x9f]/g, "");
const text = (s: string, p: Record<string, unknown> = {}): NativeNode => ({ k: "text", p: { text: clean(s), wrap: "word", ...p } });

/** Validate topology, never repair endpoints or guess missing roots/relations. Envelope owns the byte bound. */
export function readGraph(value: Record<string, unknown>, validateHit: (v: unknown) => boolean, localOverview = false): value is Record<string, unknown> & GraphData {
	const maxNodes = localOverview ? 12_000 : 64, maxEdges = localOverview ? 48_000 : 128;
	if (value.kind !== "graph" || typeof value.title !== "string" || !(value.query === null || typeof value.query === "string") ||
		!Array.isArray(value.nodes) || value.nodes.length > maxNodes || !Array.isArray(value.edges) || value.edges.length > maxEdges ||
		!Array.isArray(value.roots) || value.roots.length > maxNodes || !Array.isArray(value.caveats) ||
		!value.caveats.every(v => typeof v === "string") || typeof value.truncated !== "boolean") return false;
	if (value.loading !== undefined && typeof value.loading !== "boolean") return false;
	const ids = new Set<string>();
	const files = new Set<string>();
	for (const n of value.nodes) {
		if (!object(n) || typeof n.id !== "string" || n.id.length === 0 || ids.has(n.id) || typeof n.label !== "string" ||
			!(n.detail === null || typeof n.detail === "string") || !(n.hit === null || validateHit(n.hit))) return false;
		if (n.category !== undefined && (typeof n.category !== "string" || !["file", "directory", "project", "group"].includes(n.category))) return false;
		if (n.path !== undefined && (typeof n.path !== "string" || n.path.length > 4096 || /[\\:\u0000-\u001f\u007f-\u009f]/.test(n.path) ||
			(n.path !== "" && n.path.split("/").some(part => part === "" || part === "." || part === "..")))) return false;
		if (n.project !== undefined && (typeof n.project !== "string" || n.project.length > 4096 || /[\\:\u0000-\u001f\u007f-\u009f]/.test(n.project) ||
			(n.project !== "" && n.project.split("/").some(part => part === "" || part === "." || part === "..")))) return false;
		if (n.indexing !== undefined && (typeof n.indexing !== "string" || !["pending", "processing", "ready", "failed", "unknown"].includes(n.indexing))) return false;
		if (n.indexingError !== undefined && (n.indexing !== "failed" || typeof n.indexingError !== "string" || !n.indexingError.trim() ||
			n.indexingError.length > 4096 || /[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f-\u009f]/.test(n.indexingError))) return false;
		ids.add(n.id);
		if (n.category === "file" && typeof n.path === "string" && n.path.length > 0) files.add(n.id);
	}
	return new Set(value.roots).size === value.roots.length && value.roots.every(id => typeof id === "string" && ids.has(id)) &&
		value.edges.every(e => {
			if (!object(e) || typeof e.from !== "string" || !ids.has(e.from) || typeof e.to !== "string" || !ids.has(e.to) || typeof e.label !== "string") return false;
			if (e.kind !== undefined && (typeof e.kind !== "string" || !["containment", "association", "import", "reference"].includes(e.kind))) return false;
			for (const evidence of [e.symbol, e.moniker]) {
				if (evidence !== undefined && (typeof evidence !== "string" || !evidence.length || evidence.length > 4096 || /[\u0000-\u001f\u007f-\u009f]/.test(evidence))) return false;
			}
			if (e.kind === "reference" && (typeof e.symbol !== "string" || typeof e.moniker !== "string" || !files.has(e.from) || !files.has(e.to))) return false;
			return (e.symbol === undefined && e.moniker === undefined) || e.kind === "reference";
		});
}

export interface GraphRelation {
	index: number;
	node: GraphData["nodes"][number];
	label: string;
	direction: "outgoing" | "incoming" | "self";
}
const edgeLabel = (edge: GraphData["edges"][number]) => edge.symbol ? `${edge.label} · ${edge.symbol}` : edge.label;
/** Both directions remain explicit. Self-loops appear once, parallel edges are not deduplicated. */
export function graphRelations(data: GraphData, id: string): GraphRelation[] {
	return relationIndex(data, new Map(data.nodes.map(node => [node.id, node]))).get(id) ?? [];
}
function relationIndex(data: GraphData, nodes: Map<string, GraphData["nodes"][number]>): Map<string, GraphRelation[]> {
	const relations = new Map(data.nodes.map(node => [node.id, [] as GraphRelation[]]));
	for (const [index, edge] of data.edges.entries()) {
		const label = edgeLabel(edge);
		relations.get(edge.from)!.push({ index, node: nodes.get(edge.to)!, label,
			direction: edge.from === edge.to ? "self" : "outgoing" });
		if (edge.to !== edge.from) relations.get(edge.to)!.push({ index, node: nodes.get(edge.from)!, label, direction: "incoming" });
	}
	return relations;
}

/** A rooted relationship navigator, not a unique-parent reinterpretation of the graph. */
export class GraphInspector implements Component {
	private selected: string | undefined;
	private relation: GraphRelation | undefined;
	private previewComponent: Component | undefined;
	private selectionRevision = 0;
	private readonly expanded = new Set<string>();
	private readonly nodeById: Map<string, GraphData["nodes"][number]>;
	private readonly ordered: GraphData["nodes"][number][];
	private readonly relations: Map<string, GraphRelation[]>;
	private shownNodes = 32;
	private conceptCacheRevision = -1;
	private colorCacheRevision = -1;
	private conceptCacheDark: boolean | undefined;
	private readonly conceptCues = new Map<string, NativeNode>();
	private activity = new Set<string>();
	private activeIds = new Set<string>();
	private readonly treeCache = new Map<boolean, NativeNode[]>();
	private readonly ranks: Map<Hit, { index: number; strength: number | null }>;
	private indicatorMask = -1;
	private quickCache: NativeNode[] | undefined;
	private rowCache: NativeNode[] | undefined;
	private readonly nodeOrder: Map<string, number>;
	private readonly aggregate: boolean;
	private projection?: ProjectSummary;
	private appearance?: Readonly<GraphAppearance>;
	private projectionDetail?: GraphAppearance["detail"];
	private scope?: GraphGroup;
	private scopeDrawing?: GraphData;
	private focusDrawing?: GraphData;
	private shownGroups = 6;
	private shownRelations = 8;
	private readonly groupCues = new Map<string, NativeNode>();
	private readonly diagramAction = (id: string): string | undefined => {
		if (this.projection && !this.scopeDrawing && !this.focusDrawing) {
			const index = this.projection.groups.findIndex(group => group.id === id);
			return index < 0 ? undefined : `summary-group:${index}`;
		}
		const index = this.nodeOrder.get(id);
		return index === undefined ? undefined : `node:${index}`;
	};
	constructor(readonly data: GraphData, private readonly requestRender: () => void, private readonly preview: (hit: Hit) => Component,
		private readonly codebaseId: string | null = null, comparisonHits?: readonly Hit[], private readonly summary: "header" | "body" = "body",
		private readonly presentation: "inline" | "overview" = "inline") {
		this.nodeById = new Map(data.nodes.map(n => [n.id, n]));
		this.relations = relationIndex(data, this.nodeById);
		const roots = new Set(data.roots);
		this.ordered = [...data.roots.map(id => this.nodeById.get(id)!), ...data.nodes.filter(n => !roots.has(n.id))];
		this.nodeOrder = new Map(this.ordered.map((node, index) => [node.id, index]));
		this.aggregate = data.nodes.length > 64 || data.edges.length > 128 ||
			(presentation === "overview" && data.nodes.filter(node => node.category === "file").length > 24);
		const hits = comparisonHits ?? data.nodes.flatMap(node => node.hit ? [node.hit] : []), scales = strengths(hits);
		this.ranks = new Map(hits.map((hit, index) => [hit, { index, strength: scales[index] }]));
		subscribeConcepts(this, owner => owner.requestRender());
	}
	invalidate(): void { this.quickCache = undefined; this.rowCache = undefined; this.previewComponent?.invalidate?.(); }
	setAppearance(appearance: Readonly<GraphAppearance>): void {
		if (this.appearance === appearance) return;
		this.appearance = appearance;
		this.conceptCues.clear(); this.groupCues.clear(); this.treeCache.clear();
		this.quickCache = undefined; this.rowCache = undefined;
		this.requestRender();
	}
	setActivity(targets: readonly string[]): void {
		const next = new Set(targets.filter(target => target.length > 0));
		if (next.size === this.activity.size && [...next].every(target => this.activity.has(target))) return;
		this.activity = next;
		this.activeIds = new Set(this.data.nodes.filter(node => this.active(node)).map(node => node.id));
		this.treeCache.clear();
		this.quickCache = undefined; this.rowCache = undefined;
		this.requestRender();
	}
	private active(node: GraphData["nodes"][number]): boolean {
		if (node.hit?.codebaseId && node.hit.codebaseId !== this.codebaseId) return false;
		return [node.id, node.label, node.path, node.hit?.symbol, node.hit?.qualifiedName, node.hit?.path]
			.some(value => typeof value === "string" && this.activity.has(value));
	}
	private treeLabel(node: GraphData["nodes"][number], label: string, cx: DescribeContext): string | readonly { t: string; s: string; fx?: "pulse" }[] {
		return this.active(node) ? [{ t: clean(label), s: "success", fx: cx.reduceMotion ? undefined : "pulse" }] : clean(label);
	}
	private visibleNodes(): GraphData["nodes"] { return this.scope?.members ?? this.ordered; }
	private clearSelection(): void {
		this.selected = undefined; this.relation = undefined; this.previewComponent = undefined; this.focusDrawing = undefined;
		this.shownRelations = 8; this.quickCache = undefined; this.rowCache = undefined;
	}
	private chooseGroup(index?: number): void {
		const group = index === undefined ? undefined : this.projection?.allGroups[index];
		if (index !== undefined && !group) return;
		this.enterGroup(group);
	}
	private enterGroup(group?: GraphGroup): void {
		this.scope = group; this.scopeDrawing = group ? graphWindow(this.data, group.members) : undefined;
		this.clearSelection(); this.shownNodes = 32; this.treeCache.clear(); this.requestRender();
	}
	private diagram(cx: DescribeContext): NativeNode | undefined {
		const drawing = this.focusDrawing ?? this.scopeDrawing ?? this.projection?.graph ?? this.data;
		let active = this.activeIds;
		if (drawing === this.projection?.graph) {
			active = new Set<string>();
			for (const id of this.activeIds) { const group = this.projection.groupOf.get(id); if (group) active.add(group); }
		}
		return graphPreview(drawing, cx, { codebaseId: this.codebaseId, activeIds: active,
			selected: this.selected, size: this.presentation === "overview" ? "overview" : "inline", nodeAction: this.diagramAction, appearance: this.appearance });
	}
	private groupControls(cx: DescribeContext): NativeNode[] {
		if (!this.projection) return [];
		if (this.scope) return [{ k: "row", key: "graph-scope", p: { gap: "sm", align: "center" }, c: [
			text("Overview", { tone: "accent", actions: { click: "overview" }, aria: "Return to project overview" }),
			text("›", { tone: "muted" }), text(this.scope.label, { grow: 1, truncate: "end", wrap: "none", title: this.scope.label }),
			text(`${this.scope.members.length} nodes`, { tone: "muted" }),
			...(indexingSummary(this.scope.indexing) ? [text(indexingSummary(this.scope.indexing)!, { tone: "muted" })] : []),
		] }];
		const rows: NativeNode[] = this.projection.allGroups.slice(0, this.shownGroups).map((group, index) => {
			const active = this.activeIds.size > 0 && group.members.some(node => this.activeIds.has(node.id));
			const action = `group:${index}`;
			const preview = group.members.slice(0, 3).map(node => node.hit?.path ?? node.path ?? node.label), more = group.members.length - preview.length;
			let cue = this.groupCues.get(group.id);
			if (!cue && supportsElements(cx) && cx.feature("styles")) {
				const color = graphGroupColor(this.codebaseId, group.members, this.appearance?.color);
				if (color) cue = nativeGraphColorCue(color, cx, undefined, `group-cue-${index}`);
				if (cue) this.groupCues.set(group.id, cue);
			}
			return { k: "row", key: `group-${index}`, p: { gap: "sm", align: "center", actions: { click: action },
				title: clean([group.label, `${group.members.length} indexed nodes · ${group.internal} internal links`, indexingSummary(group.indexing),
					...preview, ...(more ? [`+${more} more nodes`] : []), ...(active ? ["Active request target in group"] : []), "Select to explore group"].filter(Boolean).join("\n")),
				aria: `Explore ${clean(group.label)}${active ? ", active request target" : ""}${indexingSummary(group.indexing) ? `, ${indexingSummary(group.indexing)}` : ""}` }, c: [...(cue ? [cue] : []),
				{ k: "text", p: { grow: 1, wrap: "none", truncate: "end", spans: [{ t: clean(group.label), s: active ? "success" : "",
					fx: active && !cx.reduceMotion ? "pulse" : undefined }] } }, text(`${group.members.length}`, { tone: "muted" }),
				...(indexingSummary(group.indexing) ? [text(indexingSummary(group.indexing)!, { tone: group.indexing.failed ? "error" : "muted", wrap: "none", truncate: "end" })] : []),
			] };
		});
		if (this.shownGroups < this.projection.allGroups.length) rows.push(text(`${this.projection.allGroups.length - this.shownGroups} more groups`, {
			tone: "muted", actions: { click: "more-groups" }, aria: "Show more project groups",
		}));
		return [{ k: "col", key: "project-groups", p: { gap: "sm", max: { w: "100ch" } }, c: rows }];
	}
	private conceptCue(node: GraphData["nodes"][number], cx: DescribeContext): NativeNode | undefined {
		if (!supportsElements(cx) || !cx.feature("styles")) return undefined;
		let cue = this.conceptCues.get(node.id);
		if (!cue) {
			const color = lookupGraphColor(this.codebaseId, node, this.appearance?.color);
			if (!color) return undefined;
			cue = nativeGraphColorCue(color, cx);
			if (!cue) return undefined;
			this.conceptCues.set(node.id, cue);
		}
		return cue;
	}
	private nodeTitle(node: GraphData["nodes"][number]): string {
		const color = lookupGraphColor(this.codebaseId, node, this.appearance?.color);
		return `${graphNodeTitle(node, color, this.active(node))}\n${this.relations.get(node.id)!.length} incident relationships · select for details`;
	}
	private nodeIndicators(node: GraphData["nodes"][number], index: number, cx: DescribeContext): NativeNode[] {
		const cue = this.conceptCue(node, cx), result = cue ? [cue] : [];
		if (node.indexing !== undefined) result.push(text(node.indexing, { tone: node.indexing === "failed" ? "error" : node.indexing === "processing" ? "accent" : "muted",
			title: indexingText(node), aria: indexingText(node), shrink: 0 }));
		if (node.hit) {
			const rank = this.ranks.get(node.hit)!;
			const score = matchScoreIndicator(node.hit, rank.index, rank.strength, cx, `node:${index}`);
			if (score) result.push(score);
			result.push(matchVerificationIcon(node.hit, cx, `node:${index}`));
		}
		return result;
	}
	private initialTrees(cx: DescribeContext): NativeNode[] {
		let trees = this.treeCache.get(cx.reduceMotion);
		const members = this.visibleNodes();
		if (trees && trees.length === Math.min(this.shownNodes, members.length)) return trees;
		// Native tree open state belongs to the terminal. Reuse these exact nodes across selection
		// and preview changes, so selecting a relationship never re-applies the initial open state.
		const rootIds = new Set(this.data.roots);
		const previous = trees;
		trees = members.slice(0, this.shownNodes).map((node, position): NativeNode => {
			if (previous?.[position]) return previous[position];
			const index = this.nodeOrder.get(node.id)!;
			const relations = this.relations.get(node.id)!;
			const visible = this.expanded.has(`neighbors-${index}`) ? relations : relations.slice(0, 4);
			const tree: NativeNode = { k: "tree", key: `node-${index}`, p: { title: this.nodeTitle(node), nodes: [{
				id: `node-${index}`, label: this.treeLabel(node, `${rootIds.has(node.id) ? "◇ " : ""}${node.label}`, cx), open: false,
				children: [...visible.map(relation => ({
					id: `link-${index}-${relation.index}`,
					label: this.treeLabel(relation.node, `${relation.direction === "outgoing" ? "→" : relation.direction === "incoming" ? "←" : "↻"} ${relation.label} · ${relation.node.label}`, cx),
				})), ...(relations.length > visible.length ? [{ id: `more-${index}`, label: `${relations.length - visible.length} more relationships…` }] : [])],
			}] } };
			const indicators = this.nodeIndicators(node, index, cx);
			return indicators.length ? { k: "row", key: `indicators-node-${index}`, p: { gap: "sm", align: "start" }, c: [...indicators, tree] } : tree;
		});
		this.treeCache.set(cx.reduceMotion, trees);
		return trees;
	}
	private select(id: string, relation?: GraphRelation): void {
		this.quickCache = undefined; this.rowCache = undefined;
		if (!this.nodeById.has(id)) return;
		if (this.scope && !this.scope.members.some(node => node.id === id)) {
			this.scope = this.projection?.allGroups.find(group => group.members.some(node => node.id === id));
			this.scopeDrawing = this.scope ? graphWindow(this.data, this.scope.members) : undefined;
			this.shownNodes = 32; this.treeCache.clear();
		}
		this.selected = id;
		this.relation = relation;
		this.shownRelations = 8;
		if (this.aggregate) {
			const nearby = new Map([[id, this.nodeById.get(id)!]]);
			for (const edge of this.relations.get(id)!) nearby.set(edge.node.id, edge.node);
			this.focusDrawing = graphWindow(this.data, [...nearby.values()], id);
		}
		this.selectionRevision++;
		// Creating the injected preview never performs source I/O. Only a deliberate selection opens it.
		const hit = this.nodeById.get(id)!.hit;
		this.previewComponent = hit ? this.preview(hit) : undefined;
		this.requestRender();
	}
	move(delta: number): void {
		const members = this.visibleNodes();
		if (!members.length) return;
		const at = members.findIndex(n => n.id === this.selected);
		this.select(members[Math.max(0, Math.min(members.length - 1, at < 0 ? 0 : at + delta))].id);
	}
	handleNativeEvent(event: NativeUiEvent): void {
		this.quickCache = undefined; this.rowCache = undefined;
		if (event.type === "toggle") {
			const key = event.key.slice(event.key.lastIndexOf("/") + 1);
			if (event.collapsed) this.expanded.delete(key); else this.expanded.add(key);
			this.requestRender();
		} else if (event.type === "select" || event.type === "activate") {
			if (event.item.startsWith("node-")) {
				const n = this.ordered[Number(event.item.slice(5))];
				if (n) this.select(n.id);
			} else if (event.item.startsWith("edge-")) this.selectEdge(Number(event.item.slice(5)));
			else if (event.item.startsWith("link-")) {
				const match = /^link-(\d+)-(\d+)$/.exec(event.item);
				const source = match ? this.ordered[Number(match[1])] : undefined;
				const relation = source ? this.relations.get(source.id)!.find(r => r.index === Number(match![2])) : undefined;
				if (relation) this.select(relation.node.id, relation);
			} else if (event.item.startsWith("more-")) {
				const index = Number(event.item.slice(5));
				if (Number.isSafeInteger(index) && this.ordered[index]) {
					this.expanded.add(`neighbors-${index}`);
					this.treeCache.clear();
					this.requestRender();
				}
			}
		} else if (event.type === "action") {
			if (event.act.startsWith("node:")) {
				const n = this.ordered[Number(event.act.slice(5))]; if (n) this.select(n.id);
			} else if (event.act.startsWith("edge:")) this.selectEdge(Number(event.act.slice(5)));
			else if (event.act === "clear") { this.clearSelection(); this.requestRender(); }
			else if (event.act === "overview") this.chooseGroup();
			else if (event.act.startsWith("group:")) this.chooseGroup(Number(event.act.slice(6)));
			else if (event.act.startsWith("summary-group:")) {
				const group = this.projection?.groups[Number(event.act.slice(14))];
				if (group) this.enterGroup(group);
			}
			else if (event.act === "more-groups") { this.shownGroups += 12; this.requestRender(); }
			else if (event.act === "more-related") { this.shownRelations += 16; this.requestRender(); }
			else if (event.act === "more-nodes") { this.shownNodes = Math.min(this.visibleNodes().length, this.shownNodes + 32); this.requestRender(); }
		}
	}
	private selectEdge(index: number): void {
		if (!this.selected || !Number.isSafeInteger(index)) return;
		const r = this.relations.get(this.selected)!.find(r => r.index === index);
		if (r) this.select(r.node.id, r);
	}
	private section(key: string, title: string, children: NativeNode[], cx: DescribeContext): NativeNode[] {
		return cx.supports("section") ? [{ k: "section", key, p: { head: clean(title), collapsible: true, collapsed: !this.expanded.has(key) }, c: children }] : children;
	}
	describe(cx: DescribeContext): NativeNode {
		if (this.appearance?.motion === "off" && !cx.reduceMotion) cx = { ...cx, reduceMotion: true };
		const d = this.data;
		const revision = conceptRevision(), colorRevision = graphColorRevision(), drawingSupported = supportsElements(cx) && cx.feature("styles"),
			mask = Number(drawingSupported) | (Number(cx.supports("meter")) << 1) | (Number(cx.supports("icon")) << 2) | (Number(cx.reduceMotion) << 3);
		if (this.aggregate && (this.conceptCacheRevision !== revision || this.projectionDetail !== this.appearance?.detail)) {
			this.projection = projectSummary(d, this.codebaseId, this.appearance?.detail);
			this.projectionDetail = this.appearance?.detail;
			// A display-only budget change must not replace the user's drilled-down membership.
			this.scope = this.projection.allGroups.find(group => group.id === this.scope?.id) ??
				this.projection.groups.find(group => group.id === this.scope?.id) ?? this.scope;
			this.scopeDrawing = this.scope ? graphWindow(d, this.scope.members) : undefined;
		}
		if (this.conceptCacheRevision !== revision || this.colorCacheRevision !== colorRevision || this.conceptCacheDark !== cx.dark || this.indicatorMask !== mask) {
			this.conceptCacheRevision = revision; this.colorCacheRevision = colorRevision; this.conceptCacheDark = cx.dark; this.indicatorMask = mask;
			this.conceptCues.clear(); this.groupCues.clear(); this.treeCache.clear(); this.quickCache = undefined; this.rowCache = undefined;
		}
		const rootIds = new Set(d.roots), members = this.visibleNodes();
		const rows = cx.supports("tree") ? this.initialTrees(cx) : this.rowCache ??= members.slice(0, this.shownNodes).map((n): NativeNode => {
			const i = this.nodeOrder.get(n.id)!;
			return { k: "row", key: `node-${i}`, reveal: n.id === this.selected ? { at: "nearest", n: this.selectionRevision } : undefined, p: {
				gap: "sm", mark: n.id === this.selected ? "pick" : undefined, actions: { click: `node:${i}` },
				aria: `Inspect ${clean(n.label)}${rootIds.has(n.id) ? ", graph root" : ""}`, title: this.nodeTitle(n),
			}, c: [...this.nodeIndicators(n, i, cx), text(rootIds.has(n.id) ? "◇" : "·", { tone: "muted", shrink: 0 }),
				this.active(n) ? { k: "text", p: { spans: [{ t: clean(n.label), s: "success", fx: cx.reduceMotion ? undefined : "pulse" }] } } : text(n.label, { grow: 1, truncate: "end", wrap: "none" })] };
		});
		const out: NativeNode[] = [], summary: NativeNode[] = [];
		if (this.summary === "body") summary.push(text(`${d.nodes.length} nodes · ${d.edges.length} relationships`, {
			tone: "muted", title: "Arrows show edge direction. Shared nodes and cycles remain links.",
		}));
		if (d.truncated) summary.push(text("Partial graph", { tone: "warning", title: "The graph is truncated. The original result retains all returned data." }));
		if (d.loading) summary.push(text("Relationship layers loading", { tone: "muted", title: "Available nodes remain usable." }));
		if (d.caveats.length) {
			const title = clean(d.caveats.join("\n")), tone = d.truncated ? "warning" as const : "muted" as const;
			summary.push(cx.supports("icon") ? { k: "icon", key: "graph-notes", p: { name: "info", tone, title, aria: title } }
				: text("Graph notes", { tone, title, aria: title }));
		}
		if (summary.length) out.push({ k: "row", p: { gap: "sm", align: "center" }, c: summary });
		if (!rows.length) out.push(text("No graph nodes in this result.", { tone: "muted" }));
		if (this.scope) out.push(...this.groupControls(cx));
		const diagram = this.diagram(cx);
		if (diagram) out.push(diagram);
		else if (d.nodes.length) out.push(text("Native graph drawing unavailable · use node navigation below.", {
			tone: "muted", title: "Graph drawing requires Tern native elements and surface styles.",
		}));
		const drawing = this.focusDrawing ?? this.scopeDrawing ?? this.projection?.graph;
		if (diagram && drawing) out.push(text(`${drawing.nodes.length} ${drawing === this.projection?.graph ? "group summaries" : "nodes"} · ${drawing.edges.length} routes drawn`, {
			tone: "muted", title: drawing.caveats.join("\n") || "Exact relationships remain in native node details.",
		}));
		if (this.projection && !this.scope && !this.selected) out.push(...this.groupControls(cx));
		const quick = this.quickCache ??= members.slice(0, 3).map((node): NativeNode => {
			const index = this.nodeOrder.get(node.id)!;
			return { k: "row", key: `quick-${index}`, p: {
				gap: "sm", max: { w: "28ch" }, mark: this.selected === node.id ? "pick" : undefined,
				actions: { click: `node:${index}` }, title: this.nodeTitle(node), aria: `Inspect ${clean(node.label)}`,
			}, c: [...this.nodeIndicators(node, index, cx), {
				k: "text", p: { spans: [{ t: clean(node.hit?.symbol ?? node.label), s: this.active(node) ? "success" : this.selected === node.id ? "accent" : "muted",
					fx: this.active(node) && !cx.reduceMotion ? "pulse" : undefined }], truncate: "end", wrap: "none" },
			}] };
		});
		if (quick.length && (!this.projection || this.scope)) out.push({ k: "row", key: "quick-nodes", p: { gap: "md", wrap: true }, c: quick });
		const details = [...rows];
		if (this.shownNodes < members.length) details.push({ ...text(`Show ${Math.min(32, members.length - this.shownNodes)} more`, {
			tone: "accent", actions: { click: "more-nodes" }, aria: "Show more graph nodes",
		}), key: "more-nodes" });
		if (details.length) out.push(...this.section("graph-nodes", this.scope ? "Group nodes" : this.projection ? "All nodes" : d.edges.length ? "Relationships" : "Nodes", details, cx));
		if (this.selected !== undefined) {
			const selected = this.nodeById.get(this.selected)!;
			out.push({ k: "row", key: "selection", reveal: { at: "nearest", n: this.selectionRevision }, p: { gap: "sm", wrap: true }, c: [text(selected.label, { grow: 1, tone: "accent" }),
				{ ...text("Close preview", { tone: "muted", actions: { click: "clear" }, aria: "Close selected node preview" }), key: "clear" }] });
			if (this.relation) {
				const e = d.edges[this.relation.index];
				out.push(text(`${this.nodeById.get(e.from)!.label} → ${this.nodeById.get(e.to)!.label} · ${this.relation.label}`, { tone: "accent", title: e.moniker }));
			}
			if (selected.detail) out.push(text(selected.detail, { tone: "muted" }));
			if (selected.indexing !== undefined) out.push(text(indexingText(selected)!, { tone: selected.indexing === "failed" ? "error" : "muted" }));
			const relations = this.relations.get(selected.id)!, visible = relations.slice(0, this.shownRelations);
			if (cx.supports("tree") && relations.length) out.push({ k: "tree", key: "relationships", p: { nodes: [{ id: "selected", label: `${relations.length} incident relationships`, open: true,
				children: visible.map(r => ({ id: `edge-${r.index}`, label: this.treeLabel(r.node, `${r.direction === "outgoing" ? "→" : r.direction === "incoming" ? "←" : "↻"} ${r.label} · ${r.node.label}`, cx) })) }] } });
			else out.push(...visible.map((r): NativeNode => ({ k: "row", key: `edge-${r.index}`, p: {
				gap: "sm", actions: { click: `edge:${r.index}` }, aria: `${r.direction}: ${clean(r.label)}: ${clean(r.node.label)}`,
			}, c: [text(`${r.direction === "outgoing" ? "→" : r.direction === "incoming" ? "←" : "↻"} ${r.label}`, { tone: "muted" }), text(r.node.label, { tone: "accent", grow: 1 })] })));
			if (visible.length < relations.length) out.push(text(`${relations.length - visible.length} more relationships`, {
				tone: "accent", actions: { click: "more-related" }, aria: "Show more selected node relationships",
			}));
			if (!relations.length) out.push(text("No relationships for this node in this bounded result.", { tone: "muted" }));
			if (this.previewComponent) out.push({ k: "col", key: `preview-${this.nodeOrder.get(selected.id)}`, c: [this.previewComponent] });
		}
		return { k: "col", p: { gap: "xs" }, c: out };
	}
	render(width: number): readonly string[] {
		return [this.data.title, ...this.data.caveats, ...(this.data.truncated ? ["Graph preview truncated"] : []),
			...this.ordered.map(n => `${n.id === this.selected ? ">" : "·"} ${n.label}${n.indexing ? ` · embedding ${n.indexing}` : ""}`),
			...this.data.edges.map(e => `${this.nodeById.get(e.from)!.label} → ${this.nodeById.get(e.to)!.label} · ${edgeLabel(e)}`)]
			.flatMap(s => clean(s).split(/\r?\n/)).map(s => s.slice(0, Math.max(0, width)));
	}
}
