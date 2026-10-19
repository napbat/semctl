import type { GraphData } from "./graph";
import { edgeGeometry, layoutGraph, representativeLinks, type GraphRelationKind } from "./graph-data/layout.js";
import type { GraphPreviewSize } from "./graph-preview";

export type NativeGraphPoint = readonly [number, number];
export interface NativeGraphDot {
	node: GraphData["nodes"][number];
	x: number;
	y: number;
	radius: number;
	degree: number;
	root: boolean;
}
export type NativeGraphLink = Omit<GraphData["edges"][number], "kind"> & {
	kind: GraphRelationKind;
	count: number;
	indices: number[];
	index: number;
};
export interface NativeGraphRoute {
	link: NativeGraphLink;
	points: NativeGraphPoint[];
}
export interface NativeGraphLayout {
	width: number;
	height: number;
	dots: NativeGraphDot[];
	routes: NativeGraphRoute[];
}
const layouts = new WeakMap<GraphData, Map<GraphPreviewSize, NativeGraphLayout>>();

/** Settled evidence geometry only; activity, selection, theme and actions never enter this cache. */
export function nativeGraphLayout(data: GraphData, size: GraphPreviewSize): NativeGraphLayout {
	let variants = layouts.get(data);
	const previous = variants?.get(size);
	if (previous) return previous;
	const layout = layoutGraph(data), links = representativeLinks(data);
	const geometries = links.map((_, index) => edgeGeometry(links, index, layout.positions)!);
	let left = layout.bounds.x, top = layout.bounds.y, right = left + layout.width, bottom = top + layout.height;
	for (const edge of geometries) for (let index = 0; index < edge.control.length; index += 2) {
		left = Math.min(left, edge.control[index]); right = Math.max(right, edge.control[index]);
		top = Math.min(top, edge.control[index + 1]); bottom = Math.max(bottom, edge.control[index + 1]);
	}
	const width = size === "compact" ? 280 : size === "overview" ? 760 : 560;
	const height = size === "compact" ? 176 : size === "overview" ? 320 : data.nodes.length <= 8 ? 156 : data.nodes.length <= 24 ? 216 : 264;
	const scale = Math.min(2.4, (width - 64) / Math.max(1, right - left), (height - 48) / Math.max(1, bottom - top));
	const project = (x: number, y: number): NativeGraphPoint => [(x - (left + right) / 2) * scale + width / 2, (y - (top + bottom) / 2) * scale + height / 2];
	const roots = new Set(data.roots);
	const dots = [...layout.positions.values()].map(point => {
		const [x, y] = project(point.x, point.y);
		return { node: point.node, x, y, radius: roots.has(point.node.id) || point.degree >= 4 ? 5 : 3.5, degree: point.degree, root: roots.has(point.node.id) };
	});
	const byId = new Map(dots.map(dot => [dot.node.id, dot]));
	const routes = geometries.map((edge, index): NativeGraphRoute => {
		const from = project(edge.from[0], edge.from[1]), to = project(edge.to[0], edge.to[1]);
		const controls: NativeGraphPoint[] = [from];
		for (let at = 0; at < edge.control.length; at += 2) controls.push(project(edge.control[at], edge.control[at + 1]));
		controls.push(to);
		if (!edge.self) {
			// Clip to the rendered dots, not the simulation's differently scaled radii.
			for (const [at, id, adjacent] of [[0, links[index].from, 1], [controls.length - 1, links[index].to, controls.length - 2]] as const) {
				const dot = byId.get(id)!, tangent = controls[adjacent], dx = tangent[0] - dot.x, dy = tangent[1] - dot.y;
				const distance = Math.hypot(dx, dy) || 1;
				controls[at] = [dot.x + dx / distance * (dot.radius + 1), dot.y + dy / distance * (dot.radius + 1)];
			}
		}
		const points: NativeGraphPoint[] = [controls[0]];
		// De Casteljau subdivision: <=.35px curve error and <=48px chords.
		// Quantization adds <2.3px at 760px, still below the smallest 3.5px dot.
		function subdivide(curve: NativeGraphPoint[], depth: number): void {
			const start = curve[0], end = curve.at(-1)!, dx = end[0] - start[0], dy = end[1] - start[1];
			const length = Math.hypot(dx, dy);
			const deviation = Math.max(...curve.slice(1, -1).map(point => length ? Math.abs(dy * (point[0] - start[0]) - dx * (point[1] - start[1])) / length : Math.hypot(point[0] - start[0], point[1] - start[1])));
			if (depth === 8 || (length <= 48 && deviation <= .35)) { points.push(end); return; }
			const first: NativeGraphPoint[] = [start], last: NativeGraphPoint[] = [end];
			let level = curve;
			while (level.length > 1) {
				const next: NativeGraphPoint[] = [];
				for (let at = 1; at < level.length; at++) next.push([(level[at - 1][0] + level[at][0]) / 2, (level[at - 1][1] + level[at][1]) / 2]);
				first.push(next[0]); last.unshift(next.at(-1)!); level = next;
			}
			subdivide(first, depth + 1); subdivide(last, depth + 1);
		}
		subdivide(controls, 0);
		return { link: links[index], points };
	});
	const prepared = { width, height, dots, routes };
	if (!variants) { variants = new Map(); layouts.set(data, variants); }
	variants.set(size, prepared);
	return prepared;
}
