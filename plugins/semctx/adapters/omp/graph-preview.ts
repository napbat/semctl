import type { DescribeContext, NativeNode } from "@oh-my-pi/pi-tui";
import { getGraphColorMode, lookupGraphColor, type GraphColor } from "./graph-colors";
import type { GraphData } from "./graph";
import type { GraphAppearance } from "./graph-appearance";
import { drawingWeight, relations } from "./graph-data/layout.js";
import { conceptClass } from "./native-concepts";
import { nativeGraphLayout, type NativeGraphLayout, type NativeGraphRoute } from "./graph-native-layout";
import { positionClasses, routeStrokeClasses, type NativeGraphRouteClasses } from "./graph-native-styles";
import { nativeElement, supportsElements, type NativeElement } from "./native-elements";
import { boundIndexingCounts, indexingText } from "./graph-indexing";
import { compactTooltip, TOOLTIP_LINE_CHARS } from "./tooltip-text";

export type GraphPreviewSize = "inline" | "compact" | "overview";
export interface GraphFeedback { id: number; outcome: "returned" | "failed" }
export interface GraphPreviewOptions {
	codebaseId?: string | null;
	activeIds?: ReadonlySet<string>;
	feedback?: ReadonlyMap<string, GraphFeedback>;
	selected?: string;
	size?: GraphPreviewSize;
	appearance?: Readonly<GraphAppearance>;
	nodeAction?: (id: string) => string | undefined;
}
/** Short hover facts beside the node identity. `hint` shows only when no status line needs the space. */
export interface GraphNodeFacts {
	active?: boolean;
	recent?: GraphFeedback["outcome"];
	hint?: string;
}
const plain = (value: string) => value.replace(/[\u0000-\u0008\u000b-\u001f\u007f-\u009f]/g, "");
const routeGeometry = new WeakMap<NativeGraphRoute, NativeGraphRouteClasses>();
const layoutIdentities = new WeakMap<NativeGraphLayout, number>();
let nextLayoutIdentity = 0;

/** One status phrase for a dot or row; ready nodes need no hover status. */
function indexingFact(node: GraphData["nodes"][number]): string | undefined {
	const counts = boundIndexingCounts(node), total = counts ? Object.values(counts).reduce((sum, count) => sum + count, 0) : 0;
	const error = node.indexingError?.split(/\r?\n/).find(line => line.trim());
	const group = node.category === "group";
	const state = node.indexing === "failed" && counts?.failed && total ? `Failed · ${counts.failed} of ${total} members`
		: node.indexing === "failed" ? group ? "Members failed" : "Embedding failed"
		: node.indexing === "pending" ? group ? "Members pending (not processing)" : "Embedding pending (not processing)"
		: node.indexing === "processing" ? group ? "Members processing" : "Embedding processing"
		: node.indexing === "unknown" ? group ? "Member status unknown" : "Embedding status unknown" : undefined;
	return state && error ? `${state}: ${error.trim()}` : state;
}

export function graphNodeTitle(node: GraphData["nodes"][number], color: GraphColor | undefined, facts: GraphNodeFacts = {}): string {
	const documented = color?.concept?.kind === "doc" ? color.concept : undefined;
	const group = node.category === "group", weight = group ? drawingWeight(node) : undefined;
	const second = group && documented?.source ? `From ${documented.source}`
		: [node.hit?.path ?? node.path, node.hit?.qualifiedName ?? node.hit?.symbol].find(value => value && value !== node.label);
	const concept = group || color?.source !== "concept" ? undefined
		: documented ? `Concept: ${documented.title ?? color.label}` : `Inferred concept: ${color.label}`;
	const status = [weight ? `${weight} nodes` : undefined, indexingFact(node), concept,
		facts.active ? "Active request target" : facts.recent ? `Recent request: ${facts.recent}` : undefined].filter(Boolean).join(" · ");
	return compactTooltip(node.label, second, status, facts.hint);
}

/** Appends `suffix` and shortens `value` first, so the suffix survives the tooltip line cap. */
function withSuffix(value: string, suffix: string): string {
	const line = plain(value).replace(/\s+/g, " ").trim(), room = TOOLTIP_LINE_CHARS - suffix.length;
	return `${line.length > room ? `${line.slice(0, room - 1).trimEnd()}…` : line}${suffix}`;
}

function routeTitle(data: GraphData, route: NativeGraphRoute, names: Map<string, string>, grouped: boolean, observed: boolean): string {
	const { link } = route;
	const evidence = [...new Set(link.indices.map(index => {
		const edge = data.edges[index];
		return edge.symbol ? [edge.symbol, edge.moniker].filter(Boolean).join(" · ") : edge.label;
	}))];
	const head = `${names.get(link.from)} → ${names.get(link.to)}`;
	const shown = evidence.slice(0, observed ? 1 : 2), more = evidence.length - shown.length;
	if (more && shown.length) shown[shown.length - 1] = withSuffix(shown[shown.length - 1], ` · +${more} more`);
	return compactTooltip(grouped ? head : `${head} · ${link.count} ${relations[link.kind].label.toLowerCase()} link${link.count === 1 ? "" : "s"}`,
		observed ? "Active request context" : undefined, ...shown);
}

function routeNode(route: NativeGraphRoute, layout: NativeGraphLayout, title?: string, related = false, observed = false): NativeNode {
	const { link } = route;
	const cached = routeGeometry.get(route), geometry = cached ?? routeStrokeClasses(route.points, layout.width, layout.height);
	if (!cached) routeGeometry.set(route, geometry);
	const { segments } = geometry;
	return nativeElement({ k: "el", key: `route:${JSON.stringify([link.from, link.to, link.kind])}`, p: {
		tag: "div", class: `sg-route sg-${link.kind} ${geometry.total}${route.tier ? ` sg-w${route.tier}` : ""}${related ? " sg-related" : ""}${observed ? " sg-observed" : ""}`, title, aria: title,
		attrs: title ? { role: "group", "data-from": link.from, "data-to": link.to, "data-kind": link.kind, "data-count": link.count, "data-active-context": observed } : { "aria-hidden": true },
	}, c: segments.map((classes, index) => nativeElement({ k: "el", key: `segment:${index}`, p: {
		tag: "div", class: `sg-segment ${classes}`,
	}, c: [nativeElement({ k: "el", p: { tag: "span", class: "sg-stroke" },
		c: [nativeElement({ k: "el", p: { tag: "span", class: "sg-ink" } })] }),
		nativeElement({ k: "el", p: { tag: "span", class: "sg-hit" } }),
		...(index === segments.length - 1 ? [nativeElement({ k: "el", p: { tag: "span", class: "sg-arrow" } })] : [])],
	})) });
}

interface GraphLabelBox { left: number; top: number; right: number; bottom: number }

function territoryLabels(layout: NativeGraphLayout, size: GraphPreviewSize): Map<string, { below: boolean; box: GraphLabelBox }> {
	const labels = new Map<string, { below: boolean; box: GraphLabelBox }>();
	const boxes: GraphLabelBox[] = [];
	if (size === "compact") return labels;
	for (const territory of [...layout.territories].sort((a, b) => a.key < b.key ? -1 : a.key > b.key ? 1 : 0)) {
		const width = Array.from(plain(territory.label)).length * 6 + 12;
		for (const below of [false, true]) {
			const center = territory.y + (below ? territory.ry : -territory.ry);
			const box = { left: territory.x - width / 2, top: center - 7, right: territory.x + width / 2, bottom: center + 7 };
			if (box.left < 2 || box.right > layout.width - 2 || box.top < 2 || box.bottom > layout.height - 2 ||
				layout.dots.some(dot => dot.x + dot.radius > box.left - 2 && dot.x - dot.radius < box.right + 2 && dot.y + dot.radius > box.top - 2 && dot.y - dot.radius < box.bottom + 2) ||
				boxes.some(other => box.left < other.right + 2 && box.right + 2 > other.left && box.top < other.bottom + 2 && box.bottom + 2 > other.top)) continue;
			labels.set(territory.key, { below, box });
			boxes.push(box);
			break;
		}
	}
	return labels;
}

function graphLabels(layout: NativeGraphLayout, options: Pick<GraphPreviewOptions, "appearance" | "selected" | "size" | "codebaseId">, obstacles: readonly GraphLabelBox[]): Map<string, { text: string; classes: string }> {
	const mode = options.appearance?.labels;
	const keyLimit = options.size === "compact" ? 3 : options.size === "overview" ? 10 : layout.dots.length <= 8 ? 8 : 6;
	const limit = mode === "hover" ? Number(options.selected !== undefined) : mode === "more" ? keyLimit * 2 : keyLimit;
	// Documented concepts explain the map better than incidental structural hubs. Selection still wins.
	const documented = new Set(layout.dots.filter(dot => lookupGraphColor(options.codebaseId, dot.node, "concepts")?.concept?.kind === "doc").map(dot => dot.node.id));
	const priority = [...layout.dots].sort((a, b) => Number(b.node.id === options.selected) - Number(a.node.id === options.selected) ||
		Number(documented.has(b.node.id)) - Number(documented.has(a.node.id)) ||
		Number(b.root) - Number(a.root) || (drawingWeight(b.node) ?? 0) - (drawingWeight(a.node) ?? 0) ||
		b.degree - a.degree || (a.node.id < b.node.id ? -1 : a.node.id > b.node.id ? 1 : 0));
	const boxes: GraphLabelBox[] = [];
	const labels = new Map<string, { text: string; classes: string }>();
	for (const [index, dot] of priority.entries()) {
		const path = dot.node.hit?.path ?? dot.node.path;
		let name = dot.node.hit?.symbol ?? (dot.node.category === "file" && path ? path.split("/").at(-1)! : dot.node.label);
		if (path && dot.node.category === "file" && layout.dots.some(other => other !== dot && (other.node.hit?.path ?? other.node.path)?.split("/").at(-1) === name)) name = path.split("/").slice(-2).join("/");
		const characters = Array.from(plain(name).replace(/\s+/g, " ")), maximum = options.size === "compact" ? 18 : 30;
		const text = characters.length > maximum ? `${characters.slice(0, maximum - 1).join("")}…` : characters.join("");
		const width = Array.from(text).length * (options.size === "compact" ? 6.6 : 6.8) + 4, gap = 9 + (dot.radius - 3.5);
		const preferred = `${dot.x + width + gap > layout.width - 4 ? " sg-label-left" : ""}${dot.y < gap + 11 ? " sg-label-below" : ""}`;
		let transient: string | undefined;
		for (const [leftward, below] of [[false, false], [false, true], [true, false], [true, true]] as const) {
			const left = dot.x + (leftward ? -gap - width : gap), top = dot.y + (below ? gap - 2 : -gap - 7);
			const box = { left, top, right: left + width, bottom: top + 14 };
			if (obstacles.some(other => box.left < other.right + 6 && box.right + 6 > other.left && box.top < other.bottom + 4 && box.bottom + 4 > other.top)) continue;
			const classes = `${leftward ? " sg-label-left" : ""}${below ? " sg-label-below" : ""}`;
			const inBounds = box.left >= 4 && box.right <= layout.width - 4 && box.top >= 2 && box.bottom <= layout.height - 2;
			if (transient === undefined || classes === preferred) transient = classes;
			if (!inBounds || (options.appearance ? boxes.length : index) >= limit || mode === "hover" && dot.node.id !== options.selected ||
				boxes.some(other => box.left < other.right + 6 && box.right + 6 > other.left && box.top < other.bottom + 4 && box.bottom + 4 > other.top) ||
				layout.dots.some(other => other.x + other.radius > box.left - 2 && other.x - other.radius < box.right + 2 && other.y + other.radius > box.top - 2 && other.y - other.radius < box.bottom + 2)) continue;
			boxes.push(box);
			labels.set(dot.node.id, { text, classes });
			break;
		}
		if (!labels.has(dot.node.id)) labels.set(dot.node.id, { text,
			classes: ` sg-label-transient${transient ?? preferred}`,
		});
	}
	return labels;
}

/** Native elements only. Owners keep their tree navigator when these capabilities are absent. */
export function graphPreview(data: GraphData, cx: DescribeContext, options: GraphPreviewOptions = {}): NativeElement | undefined {
	if (!supportsElements(cx) || !cx.feature("styles") || data.nodes.length < 1 || data.nodes.length > 64 || data.edges.length > 128) return undefined;
	const size = options.size ?? "inline", appearance = options.appearance, layout = nativeGraphLayout(data, size, appearance?.layout);
	const layoutId = layoutIdentities.get(layout) ?? ++nextLayoutIdentity;
	layoutIdentities.set(layout, layoutId);
	const colorMode = appearance?.color ?? getGraphColorMode();
	const selected = layout.dots.some(dot => dot.node.id === options.selected) ? options.selected : undefined;
	const names = new Map(data.nodes.map(node => [node.id, node.label]));
	const grouped = data.nodes.every(node => node.category === "group");
	const activeRoutes = new Set(layout.routes.filter(route => options.activeIds?.has(route.link.from) || options.activeIds?.has(route.link.to)));
	const titles = new Map(layout.routes.map(route => [route, routeTitle(data, route, names, grouped, activeRoutes.has(route))]));
	const connected = new Set(selected ? [selected] : []);
	for (const route of layout.routes) if (route.link.from === selected || route.link.to === selected) {
		connected.add(route.link.from); connected.add(route.link.to);
	}
	const chips = territoryLabels(layout, size);
	const labels = graphLabels(layout, { ...options, selected }, [...chips.values()].map(chip => chip.box));
	const arrival = new Map([...layout.dots].sort((a, b) => Number(b.root) - Number(a.root) || b.degree - a.degree ||
		(a.node.id < b.node.id ? -1 : a.node.id > b.node.id ? 1 : 0)).map((dot, rank) => [dot.node.id, Math.min(7, Math.floor(rank / Math.ceil(layout.dots.length / 8)))]));
	const territories = layout.territories.map(territory => {
		const tint = territory.color[cx.dark ? "dark" : "light"].replace("#", "");
		const width = Math.max(0, Math.min(200, Math.round(territory.rx * 400 / layout.width)));
		const height = Math.max(0, Math.min(200, Math.round(territory.ry * 400 / layout.height)));
		const title = compactTooltip(territory.title ?? `${territory.label}\nShared identifier terms`);
		const chip = chips.get(territory.key);
		return nativeElement({ k: "el", key: `territory:${territory.key}`, p: {
			tag: "div", class: `sg-territory sg-territory-${territory.kind} ${positionClasses(territory.x, territory.y, layout.width, layout.height)} sg-tw${width} sg-th${height} sg-tint-${tint}`,
			attrs: chip ? undefined : { "aria-hidden": true },
		}, c: chip ? [nativeElement({ k: "el", key: "territory-label", p: {
			tag: "span", class: `sg-territory-label${chip.below ? " sg-territory-label-bottom" : ""}`, text: plain(territory.label), title,
		} })] : [] });
	});
	const dots = layout.dots.map((dot): NativeNode => {
		const id = dot.node.id, active = options.activeIds?.has(id) ?? false;
		const feedback = active ? undefined : options.feedback?.get(id);
		const indexing = dot.node.indexing, stateText = indexingText(dot.node);
		const affected = indexing === "pending" || indexing === "processing" || indexing === "failed";
		const counts = boundIndexingCounts(dot.node), members = counts ? Object.values(counts).reduce((sum, count) => sum + count, 0) : 0;
		const pip = indexing === "failed" && counts && members > 0
			? counts.failed * 4 < members ? "sg-pip-faint" : counts.failed * 5 <= members * 3 ? "sg-pip-mid" : "sg-pip-strong"
			: undefined;
		const incident = layout.routes.filter(route => route.link.from === id || route.link.to === id);
		const color = lookupGraphColor(options.codebaseId, dot.node, colorMode);
		const action = options.nodeAction?.(id), label = labels.get(id);
		const title = graphNodeTitle(dot.node, color, { active, recent: feedback?.outcome,
			hint: action ? dot.node.category === "group" ? "Select to explore group" : "Select for details" : undefined });
		return nativeElement({ k: "el", key: `node:${id}`, p: { tag: "div", class: "sg-node" }, c: [
			nativeElement({ k: "el", key: "target", p: {
				tag: "button", class: `sg-target ${positionClasses(dot.x, dot.y, layout.width, layout.height)} sg-s${Math.round(dot.radius * 2)} sg-d${arrival.get(id)}${color ? ` ${conceptClass(color, cx.dark)}` : ""}${dot.root ? " sg-root" : ""}${active ? " sg-active" : ""}${affected ? ` sg-indexing-${indexing}` : ""}${pip ? ` ${pip}` : ""}${id === selected ? " sg-selected" : ""}${selected && !connected.has(id) ? " sg-muted" : ""}${action ? " sg-action" : ""}`,
				title, aria: `${plain(dot.node.label)}${dot.root ? ", graph root" : ""}${active ? ", active request target" : ""}${feedback ? `, recently ${feedback.outcome}` : ""}${stateText ? `, ${plain(stateText)}` : ""}${dot.node.category === "group" && dot.node.detail ? `, ${plain(dot.node.detail)}` : ""}${id === selected ? ", selected" : ""}`,
				attrs: { "data-node": id, "aria-pressed": id === selected, ...(indexing ? { "data-indexing": indexing } : {}) }, actions: action ? { click: action } : undefined,
			}, c: [
				nativeElement({ k: "el", key: "halo", p: { tag: "span", class: "sg-halo", attrs: { "aria-hidden": true } } }),
				...(affected ? [nativeElement({ k: "el", key: `indexing:${indexing}`, p: { tag: "span", class: "sg-indexing-state", attrs: { "aria-hidden": true } } })] : []),
				...(feedback ? [nativeElement({ k: "el", key: `feedback:${feedback.id}`, p: { tag: "span", class: `sg-feedback sg-${feedback.outcome}`, attrs: { "aria-hidden": true } } })] : []),
				nativeElement({ k: "el", key: "dot", p: { tag: "span", class: "sg-dot", attrs: { "aria-hidden": true } } }),
				...(label ? [nativeElement({ k: "el", key: "label", p: { tag: "span", class: `sg-label${label.classes}`, text: label.text, attrs: { "aria-hidden": true } } })] : []),
			] }),
			nativeElement({ k: "el", key: "hover", p: { tag: "div", class: "sg-hover-links", attrs: { "aria-hidden": true } }, c: incident.map(route => routeNode(route, layout, undefined, true, activeRoutes.has(route))) }),
		] });
	});
	return nativeElement({ k: "el", key: "graph-preview", p: {
		tag: "section", class: `semctl-graph sg-${size}${cx.dark ? " sg-dark" : ""}${selected ? " sg-focused" : ""}${data.nodes.length <= 8 ? " sg-small" : data.nodes.length <= 24 ? " sg-medium" : ""}${appearance ? ` sg-links-${appearance.links}` : ""}${colorMode === "neutral" ? " sg-neutral" : ""}`,
		aria: `${data.nodes.length} nodes and ${data.edges.length} directed relationships${selected ? "; selected node connections highlighted" : ""}`,
		title: compactTooltip(`${grouped ? "Project map" : "Graph"} · select a dot for details`,
			"Links: solid relationships · dashed imports · dotted project associations"),
		attrs: { "data-motion": appearance?.motion !== "off" && cx.reduceMotion === false ? "full" : "reduced", ...(appearance ? { "data-layout": appearance.layout, "data-labels": appearance.labels } : {}) },
	}, c: [nativeElement({ k: "el", key: "ratio", p: { tag: "div", class: "sg-ratio", attrs: { "aria-hidden": true } } }),
		nativeElement({ k: "el", key: "plane", p: { tag: "div", class: "sg-plane" }, c: [
			nativeElement({ k: "el", key: `territories:${layoutId}`, p: { tag: "div", class: "sg-territories" }, c: territories }),
			nativeElement({ k: "el", key: `routes:${layoutId}`, p: { tag: "div", class: "sg-routes" },
				c: layout.routes.map(route => routeNode(route, layout, titles.get(route), route.link.from === selected || route.link.to === selected, activeRoutes.has(route))) }),
			...dots,
		] })],
	});
}
