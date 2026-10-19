import type { DescribeContext, NativeNode } from "@oh-my-pi/pi-tui";
import { getGraphColorMode, lookupGraphColor, type GraphColor } from "./graph-colors";
import type { GraphData } from "./graph";
import type { GraphAppearance } from "./graph-appearance";
import { relations } from "./graph-data/layout.js";
import { conceptClass } from "./native-concepts";
import { nativeGraphLayout, type NativeGraphLayout, type NativeGraphRoute } from "./graph-native-layout";
import { positionClasses, routeStrokeClasses, type NativeGraphRouteClasses } from "./graph-native-styles";
import { nativeElement, supportsElements, type NativeElement } from "./native-elements";
import { indexingText } from "./graph-indexing";

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
const plain = (value: string) => value.replace(/[\u0000-\u0008\u000b-\u001f\u007f-\u009f]/g, "");
const routeGeometry = new WeakMap<NativeGraphRoute, NativeGraphRouteClasses>();

export function graphNodeTitle(node: GraphData["nodes"][number], color: GraphColor | undefined, active = false): string {
	const identity = [...new Set([node.label, node.hit?.qualifiedName ?? node.hit?.symbol, node.hit?.path ?? node.path].filter(Boolean))];
	return plain([...identity, indexingText(node),
		color ? `${color.source === "concept" ? "Inferred concept" : "Code structure"}: ${color.label}` : undefined,
		active ? "Active request target" : undefined].filter(Boolean).join("\n"));
}

function routeTitle(data: GraphData, route: NativeGraphRoute, names: Map<string, string>, grouped: boolean, observed: boolean): string {
	const { link } = route;
	const evidence = [...new Set(link.indices.map(index => {
		const edge = data.edges[index];
		return `${edge.label}${edge.symbol ? ` · ${edge.symbol}` : ""}${edge.moniker ? ` · ${edge.moniker}` : ""}`;
	}))];
	const preview = evidence.slice(0, 3), more = evidence.length - preview.length;
	return plain([`${names.get(link.from)} → ${names.get(link.to)}`,
		...(grouped ? [] : [`${relations[link.kind].label} · ${link.count} directed relationship${link.count === 1 ? "" : "s"}`]),
		...preview, ...(more ? [`+${more} more evidence entries · inspect node details`] : []),
		...(observed ? ["Active request context"] : [])].join("\n"));
}

function routeNode(route: NativeGraphRoute, layout: NativeGraphLayout, title?: string, related = false, observed = false): NativeNode {
	const { link } = route;
	const cached = routeGeometry.get(route), geometry = cached ?? routeStrokeClasses(route.points, layout.width, layout.height);
	if (!cached) routeGeometry.set(route, geometry);
	const { segments } = geometry;
	return nativeElement({ k: "el", key: `route:${JSON.stringify([link.from, link.to, link.kind])}`, p: {
		tag: "div", class: `sg-route sg-${link.kind} ${geometry.total}${related ? " sg-related" : ""}${observed ? " sg-observed" : ""}`, title, aria: title,
		attrs: title ? { role: "group", "data-from": link.from, "data-to": link.to, "data-kind": link.kind, "data-count": link.count, "data-active-context": observed } : { "aria-hidden": true },
	}, c: segments.map((classes, index) => nativeElement({ k: "el", key: `segment:${index}`, p: {
		tag: "div", class: `sg-segment ${classes}`,
	}, c: [nativeElement({ k: "el", p: { tag: "span", class: "sg-stroke" },
		c: [nativeElement({ k: "el", p: { tag: "span", class: "sg-ink" } })] }),
		nativeElement({ k: "el", p: { tag: "span", class: "sg-hit" } }),
		...(index === segments.length - 1 ? [nativeElement({ k: "el", p: { tag: "span", class: "sg-arrow" } })] : [])],
	})) });
}

function graphLabels(layout: NativeGraphLayout, options: Pick<GraphPreviewOptions, "appearance" | "selected" | "size">): Map<string, { text: string; classes: string }> {
	const mode = options.appearance?.labels;
	const keyLimit = options.size === "compact" ? 3 : options.size === "overview" ? 10 : layout.dots.length <= 8 ? 8 : 6;
	const limit = mode === "hover" ? Number(options.selected !== undefined) : mode === "more" ? keyLimit * 2 : keyLimit;
	// Only deliberate selection may displace the topology's settled labels.
	const priority = [...layout.dots].sort((a, b) => Number(b.node.id === options.selected) - Number(a.node.id === options.selected) ||
		Number(b.root) - Number(a.root) || b.degree - a.degree);
	const boxes: { left: number; top: number; right: number; bottom: number }[] = [];
	const labels = new Map<string, { text: string; classes: string }>();
	for (const [index, dot] of priority.entries()) {
		const path = dot.node.hit?.path ?? dot.node.path;
		let name = dot.node.hit?.symbol ?? (dot.node.category === "file" && path ? path.split("/").at(-1)! : dot.node.label);
		if (path && dot.node.category === "file" && layout.dots.some(other => other !== dot && (other.node.hit?.path ?? other.node.path)?.split("/").at(-1) === name)) name = path.split("/").slice(-2).join("/");
		if (options.size === "compact" && dot.node.category === "group") name = name.split(" · ", 1)[0];
		const characters = Array.from(plain(name).replace(/\s+/g, " ")), maximum = options.size === "compact" ? 18 : 30;
		const text = characters.length > maximum ? `${characters.slice(0, maximum - 1).join("")}…` : characters.join("");
		const width = Array.from(text).length * 6;
		if ((options.appearance ? boxes.length : index) < limit && (mode !== "hover" || dot.node.id === options.selected)) for (const [leftward, below] of [[false, false], [false, true], [true, false], [true, true]] as const) {
			const left = dot.x + (leftward ? -9 - width : 9), top = dot.y + (below ? 7 : -16);
			const box = { left, top, right: left + width, bottom: top + 14 };
			if (box.left < 4 || box.right > layout.width - 4 || box.top < 2 || box.bottom > layout.height - 2 ||
				boxes.some(other => box.left < other.right + 6 && box.right + 6 > other.left && box.top < other.bottom + 4 && box.bottom + 4 > other.top) ||
				layout.dots.some(other => other.x + other.radius > box.left - 2 && other.x - other.radius < box.right + 2 && other.y + other.radius > box.top - 2 && other.y - other.radius < box.bottom + 2)) continue;
			boxes.push(box);
			labels.set(dot.node.id, { text, classes: `${leftward ? " sg-label-left" : ""}${below ? " sg-label-below" : ""}` });
			break;
		}
		if (!labels.has(dot.node.id)) labels.set(dot.node.id, { text,
			classes: ` sg-label-transient${dot.x + width + 9 > layout.width - 4 ? " sg-label-left" : ""}${dot.y < 20 ? " sg-label-below" : ""}`,
		});
	}
	return labels;
}

/** Native elements only. Owners keep their tree navigator when these capabilities are absent. */
export function graphPreview(data: GraphData, cx: DescribeContext, options: GraphPreviewOptions = {}): NativeElement | undefined {
	if (!supportsElements(cx) || !cx.feature("styles") || data.nodes.length < 1 || data.nodes.length > 64 || data.edges.length > 128) return undefined;
	const size = options.size ?? "inline", appearance = options.appearance, layout = nativeGraphLayout(data, size, appearance?.layout);
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
	const labels = graphLabels(layout, { ...options, selected });
	const dots = layout.dots.map((dot): NativeNode => {
		const id = dot.node.id, active = options.activeIds?.has(id) ?? false;
		const feedback = active ? undefined : options.feedback?.get(id);
		const indexing = dot.node.indexing, stateText = indexingText(dot.node);
		const affected = indexing === "pending" || indexing === "processing" || indexing === "failed";
		const incident = layout.routes.filter(route => route.link.from === id || route.link.to === id);
		const color = lookupGraphColor(options.codebaseId, dot.node, colorMode);
		const action = options.nodeAction?.(id), label = labels.get(id);
		const title = [graphNodeTitle(dot.node, color, active),
			feedback ? `Recent request: ${feedback.outcome}` : undefined,
			action ? dot.node.category === "group" ? "Select to explore group" : "Select for details" : undefined].filter(Boolean).join("\n");
		return nativeElement({ k: "el", key: `node:${id}`, p: { tag: "div", class: "sg-node" }, c: [
			nativeElement({ k: "el", key: "target", p: {
				tag: "button", class: `sg-target ${positionClasses(dot.x, dot.y, layout.width, layout.height)}${color ? ` ${conceptClass(color, cx.dark)}` : ""}${dot.root ? " sg-root" : ""}${dot.radius === 5 ? " sg-large" : ""}${active ? " sg-active" : ""}${affected ? ` sg-indexing-${indexing}` : ""}${id === selected ? " sg-selected" : ""}${selected && !connected.has(id) && !active && !feedback && !affected ? " sg-muted" : ""}${action ? " sg-action" : ""}`,
				title, aria: `${plain(dot.node.label)}${dot.root ? ", graph root" : ""}${active ? ", active request target" : ""}${feedback ? `, recently ${feedback.outcome}` : ""}${stateText ? `, ${plain(stateText)}` : ""}${dot.node.category === "group" && dot.node.detail ? `, ${plain(dot.node.detail)}` : ""}${id === selected ? ", selected" : ""}`,
				attrs: { "data-node": id, "aria-pressed": id === selected, ...(indexing ? { "data-indexing": indexing } : {}) }, actions: action ? { click: action } : undefined,
			}, c: [
				nativeElement({ k: "el", key: "halo", p: { tag: "span", class: "sg-halo", attrs: { "aria-hidden": true } } }),
				...(affected ? [nativeElement({ k: "el", key: `indexing:${indexing}`, p: { tag: "span", class: "sg-indexing-state", attrs: { "aria-hidden": true } } })] : []),
				...(feedback ? [nativeElement({ k: "el", key: `feedback:${feedback.id}`, p: { tag: "span", class: `sg-feedback sg-${feedback.outcome}`, attrs: { "aria-hidden": true } } })] : []),
				nativeElement({ k: "el", key: "dot", p: { tag: "span", class: `sg-dot${dot.radius === 5 ? " sg-large" : ""}`, attrs: { "aria-hidden": true } } }),
				...(label ? [nativeElement({ k: "el", key: "label", p: { tag: "span", class: `sg-label${label.classes}`, text: label.text, attrs: { "aria-hidden": true } } })] : []),
			] }),
			nativeElement({ k: "el", key: "hover", p: { tag: "div", class: "sg-hover-links", attrs: { "aria-hidden": true } }, c: incident.map(route => routeNode(route, layout, undefined, false, activeRoutes.has(route))) }),
		] });
	});
	return nativeElement({ k: "el", key: "graph-preview", p: {
		tag: "section", class: `semctl-graph sg-${size}${cx.dark ? " sg-dark" : ""}${selected ? " sg-focused" : ""}${data.nodes.length <= 8 ? " sg-small" : data.nodes.length <= 24 ? " sg-medium" : ""}${appearance ? ` sg-links-${appearance.links}` : ""}${colorMode === "neutral" ? " sg-neutral" : ""}`,
		aria: `${data.nodes.length} nodes and ${data.edges.length} directed relationships${selected ? "; selected node connections highlighted" : ""}`,
		title: ["Graph · select a dot for details",
			"Links: solid relationships · dashed imports · dotted project associations.",
			"Activity: request context, not traversal or success.",
			"Rings: pending (not processing), processing, or failed embeddings.",
			...(colorMode !== "neutral" ? ["Colors: categories, not similarity or confidence."] : [])].join("\n"),
		attrs: { "data-motion": appearance?.motion !== "off" && cx.reduceMotion === false ? "full" : "reduced", ...(appearance ? { "data-layout": appearance.layout, "data-labels": appearance.labels } : {}) },
	}, c: [nativeElement({ k: "el", key: "ratio", p: { tag: "div", class: "sg-ratio", attrs: { "aria-hidden": true } } }),
		nativeElement({ k: "el", key: `geometry:${appearance?.layout ?? "force"}`, p: { tag: "div", class: "sg-plane" }, c: [
			...layout.routes.map(route => routeNode(route, layout, titles.get(route), route.link.from === selected || route.link.to === selected, activeRoutes.has(route))), ...dots,
		] })],
	});
}
