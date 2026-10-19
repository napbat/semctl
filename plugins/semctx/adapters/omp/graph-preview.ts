import type { DescribeContext, NativeNode } from "@oh-my-pi/pi-tui";
import { lookupConcept } from "./concept-colors";
import type { GraphData } from "./graph";
import { relations } from "./graph-data/layout.js";
import { conceptClass, CONCEPT_STYLES } from "./native-concepts";
import { nativeGraphLayout, type NativeGraphLayout, type NativeGraphRoute } from "./graph-native-layout";
import { GRAPH_STYLES, positionClasses, segmentClasses } from "./graph-native-styles";

type NativeGraphElement = Extract<NativeNode, { k: "el" }>;
export type GraphPreviewSize = "inline" | "compact" | "overview";
export interface GraphFeedback { id: number; outcome: "returned" | "failed" }
export interface GraphPreviewOptions {
	codebaseId?: string | null;
	activeIds?: ReadonlySet<string>;
	feedback?: ReadonlyMap<string, GraphFeedback>;
	selected?: string;
	size?: GraphPreviewSize;
	nodeAction?: (id: string) => string | undefined;
}
const plain = (value: string) => value.replace(/[\u0000-\u0008\u000b-\u001f\u007f-\u009f]/g, "");

function routeTitle(data: GraphData, route: NativeGraphRoute, names: Map<string, string>, grouped: boolean): string {
	const { link } = route;
	const evidence = link.indices.map(index => {
		const edge = data.edges[index];
		return `${edge.label}${edge.symbol ? ` · ${edge.symbol}` : ""}${edge.moniker ? ` · ${edge.moniker}` : ""}`;
	}).join("\n");
	return plain(`${names.get(link.from)} → ${names.get(link.to)}\n${relations[link.kind].label}${grouped ? " · grouped route" : ` · ${link.count} directed relationship${link.count === 1 ? "" : "s"}`}\n${evidence}`);
}

function routeNode(route: NativeGraphRoute, layout: NativeGraphLayout, title?: string, related = false): NativeNode {
	const { link, points } = route;
	return { k: "el", key: `route:${JSON.stringify([link.from, link.to, link.kind])}`, p: {
		tag: "div", class: `sg-route sg-${link.kind}${related ? " sg-related" : ""}`, title, aria: title,
		attrs: title ? { role: "group", "data-from": link.from, "data-to": link.to, "data-kind": link.kind, "data-count": link.count } : { "aria-hidden": true },
	}, c: points.slice(1).map((end, index): NativeNode => ({ k: "el", key: `segment:${index}`, p: {
		tag: "div", class: `sg-segment ${segmentClasses(points[index], end, layout.width, layout.height)}`,
	}, c: [{ k: "el", p: { tag: "span", class: "sg-stroke" } },
		...(index === points.length - 2 ? [{ k: "el" as const, p: { tag: "span" as const, class: "sg-arrow" } }] : [])],
	})) };
}

function graphLabels(layout: NativeGraphLayout, options: GraphPreviewOptions): Map<string, { text: string; classes: string }> {
	const limit = options.size === "compact" ? 3 : options.size === "overview" ? 10 : layout.dots.length <= 8 ? 8 : 6;
	const priority = [...layout.dots].sort((a, b) => Number(b.node.id === options.selected) - Number(a.node.id === options.selected) ||
		Number(options.activeIds?.has(b.node.id) ?? false) - Number(options.activeIds?.has(a.node.id) ?? false) ||
		Number(options.feedback?.has(b.node.id) ?? false) - Number(options.feedback?.has(a.node.id) ?? false) ||
		Number(b.root) - Number(a.root) || b.degree - a.degree).slice(0, limit);
	const boxes: { left: number; top: number; right: number; bottom: number }[] = [];
	const labels = new Map<string, { text: string; classes: string }>();
	for (const dot of priority) {
		const path = dot.node.hit?.path ?? dot.node.path;
		let name = dot.node.hit?.symbol ?? (dot.node.category === "file" && path ? path.split("/").at(-1)! : dot.node.label);
		if (path && dot.node.category === "file" && layout.dots.some(other => other !== dot && (other.node.hit?.path ?? other.node.path)?.split("/").at(-1) === name)) name = path.split("/").slice(-2).join("/");
		if (options.size === "compact" && dot.node.category === "group") name = name.split(" · ", 1)[0];
		const characters = Array.from(plain(name).replace(/\s+/g, " ")), maximum = options.size === "compact" ? 18 : 30;
		const text = characters.length > maximum ? `${characters.slice(0, maximum - 1).join("")}…` : characters.join("");
		const width = Array.from(text).length * 6;
		for (const [leftward, below] of [[false, false], [false, true], [true, false], [true, true]] as const) {
			const left = dot.x + (leftward ? -9 - width : 9), top = dot.y + (below ? 7 : -16);
			const box = { left, top, right: left + width, bottom: top + 14 };
			if (box.left < 4 || box.right > layout.width - 4 || box.top < 2 || box.bottom > layout.height - 2 ||
				boxes.some(other => box.left < other.right + 6 && box.right + 6 > other.left && box.top < other.bottom + 4 && box.bottom + 4 > other.top) ||
				layout.dots.some(other => other.x + other.radius > box.left - 2 && other.x - other.radius < box.right + 2 && other.y + other.radius > box.top - 2 && other.y - other.radius < box.bottom + 2)) continue;
			boxes.push(box);
			labels.set(dot.node.id, { text, classes: `${leftward ? " sg-label-left" : ""}${below ? " sg-label-below" : ""}` });
			break;
		}
	}
	return labels;
}

/** Native elements only. Owners keep their tree navigator when these capabilities are absent. */
export function graphPreview(data: GraphData, cx: DescribeContext, options: GraphPreviewOptions = {}): NativeGraphElement | undefined {
	if (!cx.supports("el") || cx.stylesheets !== true || !cx.feature("styles") || data.nodes.length < 1 || data.nodes.length > 64 || data.edges.length > 128) return undefined;
	const size = options.size ?? "inline", layout = nativeGraphLayout(data, size);
	const selected = layout.dots.some(dot => dot.node.id === options.selected) ? options.selected : undefined;
	const names = new Map(data.nodes.map(node => [node.id, node.label]));
	const grouped = data.nodes.every(node => node.category === "group");
	const titles = new Map(layout.routes.map(route => [route, routeTitle(data, route, names, grouped)]));
	const connected = new Set(selected ? [selected] : []);
	for (const route of layout.routes) if (route.link.from === selected || route.link.to === selected) {
		connected.add(route.link.from); connected.add(route.link.to);
	}
	const labels = graphLabels(layout, { ...options, selected });
	const dots = layout.dots.map((dot): NativeNode => {
		const id = dot.node.id, active = options.activeIds?.has(id) ?? false, feedback = options.feedback?.get(id);
		const incident = layout.routes.filter(route => route.link.from === id || route.link.to === id);
		const concept = lookupConcept(dot.node.hit?.codebaseId ?? options.codebaseId ?? null, dot.node.hit?.path ?? dot.node.path);
		const action = options.nodeAction?.(id), label = labels.get(id);
		const title = plain([dot.node.label, dot.node.hit?.qualifiedName, dot.node.hit?.symbol, dot.node.hit?.path ?? dot.node.path, dot.node.detail,
			...(active ? ["Active request target."] : []),
			...(feedback ? [feedback.outcome === "returned" ? "Recently returned source." : "A targeted request failed."] : []),
			...(concept ? [`${concept.label} · ${concept.reason}`] : []), ...incident.map(route => titles.get(route))].filter(Boolean).join("\n"));
		return { k: "el", key: `node:${id}`, p: { tag: "div", class: "sg-node" }, c: [
			{ k: "el", key: "target", p: {
				tag: "button", class: `sg-target ${positionClasses(dot.x, dot.y, layout.width, layout.height)}${concept ? ` ${conceptClass(concept, cx.dark)}` : ""}${dot.root ? " sg-root" : ""}${dot.radius === 5 ? " sg-large" : ""}${active ? " sg-active" : ""}${id === selected ? " sg-selected" : ""}${selected && !connected.has(id) && !active && !feedback ? " sg-muted" : ""}${action ? " sg-action" : ""}`,
				title, aria: `${plain(dot.node.label)}${dot.root ? ", graph root" : ""}${active ? ", active" : ""}${feedback ? `, recently ${feedback.outcome}` : ""}${id === selected ? ", selected" : ""}`,
				attrs: { "data-node": id, "aria-pressed": id === selected }, actions: action ? { click: action } : undefined,
			}, c: [
				{ k: "el", key: "halo", p: { tag: "span", class: "sg-halo", attrs: { "aria-hidden": true } } },
				...(feedback ? [{ k: "el" as const, key: `feedback:${feedback.id}`, p: { tag: "span" as const, class: `sg-feedback sg-${feedback.outcome}`, attrs: { "aria-hidden": true } } }] : []),
				{ k: "el", key: "dot", p: { tag: "span", class: `sg-dot${dot.radius === 5 ? " sg-large" : ""}`, attrs: { "aria-hidden": true } } },
				...(label ? [{ k: "el" as const, key: "label", p: { tag: "span" as const, class: `sg-label${label.classes}`, text: label.text, attrs: { "aria-hidden": true } } }] : []),
			] },
			{ k: "el", key: "hover", p: { tag: "div", class: "sg-hover-links", attrs: { "aria-hidden": true } }, c: incident.map(route => routeNode(route, layout)) },
		] };
	});
	return { k: "el", key: "graph-preview", styles: [...GRAPH_STYLES, ...CONCEPT_STYLES], p: {
		tag: "section", class: `semctl-graph sg-${size}${cx.dark ? " sg-dark" : ""}${selected ? " sg-focused" : ""}${data.nodes.length <= 8 ? " sg-small" : data.nodes.length <= 24 ? " sg-medium" : ""}`,
		aria: `${data.nodes.length} nodes and ${data.edges.length} directed relationships${selected ? "; selected node connections highlighted" : ""}`,
		title: "Select a dot to inspect its relationships. Solid links are ordinary relationships; dashed links are imports; dotted links are project associations. Hover dots or links for exact directed evidence.",
		attrs: { "data-motion": cx.reduceMotion === false ? "full" : "reduced" },
	}, c: [{ k: "el", key: "ratio", p: { tag: "div", class: "sg-ratio", attrs: { "aria-hidden": true } } },
		{ k: "el", key: "geometry", p: { tag: "div", class: "sg-plane" }, c: [
			...layout.routes.map(route => routeNode(route, layout, titles.get(route), route.link.from === selected || route.link.to === selected)), ...dots,
		] }],
	};
}
