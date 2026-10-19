import type { GraphData } from "./graph";
import type { GraphAppearance } from "./graph-appearance";
import { edgeGeometry, layoutGraph, representativeLinks, type GraphPosition, type GraphRelationKind } from "./graph-data/layout.js";
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
interface NativeCurve {
	from: readonly number[];
	to: readonly number[];
	control: number[];
	self: boolean;
}
const layouts = new WeakMap<GraphData, Map<string, NativeGraphLayout>>();
const compare = (a: string, b: string) => a < b ? -1 : a > b ? 1 : 0;

/** Geometry-only ordering: no activity, embedding state, concept color or theme. */
function topology(data: GraphData) {
	const ids = data.nodes.map(node => node.id).sort(compare), roots = new Set(data.roots);
	const outgoing = new Map(ids.map(id => [id, new Set<string>()])), neighbours = new Map(ids.map(id => [id, new Set<string>()]));
	for (const edge of data.edges) {
		outgoing.get(edge.from)!.add(edge.to);
		neighbours.get(edge.from)!.add(edge.to); neighbours.get(edge.to)!.add(edge.from);
	}
	const order = (a: string, b: string) => Number(roots.has(b)) - Number(roots.has(a)) || compare(a, b);
	const unseen = new Set(ids), components: string[][] = [];
	for (const id of [...ids].sort(order)) {
		if (!unseen.delete(id)) continue;
		const component = [id];
		for (let at = 0; at < component.length; at++) for (const next of [...neighbours.get(component[at])!].sort(compare)) {
			if (unseen.delete(next)) component.push(next);
		}
		components.push(component.sort(order));
	}
	return { ids, roots, outgoing, neighbours, components, order };
}

/** Radial rings encode undirected distance, not a fabricated dependency hierarchy. */
function radialPositions(data: GraphData): Map<string, NativeGraphPoint> {
	const { roots, neighbours, components, order } = topology(data), result = new Map<string, NativeGraphPoint>();
	let left = 0, top = 0, rowHeight = 0;
	const blocks = components.map(component => {
		const seeds = component.filter(id => roots.has(id));
		if (!seeds.length) seeds.push([...component].sort((a, b) => neighbours.get(b)!.size - neighbours.get(a)!.size || order(a, b))[0]);
		const distance = new Map(seeds.map(id => [id, 0])), queue = [...seeds];
		for (let at = 0; at < queue.length; at++) for (const next of [...neighbours.get(queue[at])!].sort(order)) {
			if (!distance.has(next)) { distance.set(next, distance.get(queue[at])! + 1); queue.push(next); }
		}
		const rings = new Map<number, string[]>();
		for (const id of component) {
			const depth = distance.get(id)!;
			if (!rings.has(depth)) rings.set(depth, []);
			rings.get(depth)!.push(id);
		}
		let radius = 0;
		const points = new Map<string, NativeGraphPoint>();
		for (const [depth, ring] of [...rings].sort(([a], [b]) => a - b)) {
			ring.sort(order);
			radius = depth === 0 && ring.length === 1 ? 0 : Math.max(radius + 42, ring.length * 30 / (2 * Math.PI));
			ring.forEach((id, index) => {
				const angle = -Math.PI / 2 + index / ring.length * Math.PI * 2;
				points.set(id, [Math.cos(angle) * radius, Math.sin(angle) * radius]);
			});
		}
		return { points, radius: radius + 24, extent: Math.max(48, radius * 2 + 48) };
	});
	const targetWidth = Math.sqrt(blocks.reduce((area, block) => area + block.extent ** 2, 0) * 1.6);
	for (const block of blocks) {
		if (left && left + block.extent > targetWidth) { left = 0; top += rowHeight + 24; rowHeight = 0; }
		for (const [id, point] of block.points) result.set(id, [left + block.radius + point[0], top + block.radius + point[1]]);
		left += block.extent + 24; rowHeight = Math.max(rowHeight, block.extent);
	}
	return result;
}

/** Blocks encode weak connectivity only. Rows within each block have no directed rank. */
function componentPositions(data: GraphData): Map<string, NativeGraphPoint> {
	const { neighbours, components, order } = topology(data), result = new Map<string, NativeGraphPoint>();
	const blocks = components.sort((a, b) => b.length - a.length || order(a[0], b[0])).map(component => {
		const first = [...component].sort((a, b) => neighbours.get(b)!.size - neighbours.get(a)!.size || order(a, b))[0];
		const seen = new Set([first]), queue = [first];
		for (let at = 0; at < queue.length; at++) for (const next of [...neighbours.get(queue[at])!].sort(order)) {
			if (!seen.has(next)) { seen.add(next); queue.push(next); }
		}
		const columns = Math.min(queue.length, Math.ceil(Math.sqrt(queue.length * 1.6))), rows = Math.ceil(queue.length / columns);
		return { queue, columns, width: (columns - 1) * 36 + 48, height: (rows - 1) * 36 + 48 };
	});
	const targetWidth = Math.sqrt(blocks.reduce((area, block) => area + block.width * block.height, 0) * 1.6);
	let left = 0, top = 0, rowHeight = 0;
	for (const block of blocks) {
		if (left && left + block.width > targetWidth) { left = 0; top += rowHeight + 36; rowHeight = 0; }
		block.queue.forEach((id, at) => result.set(id, [left + 24 + at % block.columns * 36, top + 24 + Math.floor(at / block.columns) * 36]));
		left += block.width + 36; rowHeight = Math.max(rowHeight, block.height);
	}
	return result;
}

/** Directed SCC condensation gives cycles one rank and retains every disconnected node. */
function layeredPositions(data: GraphData): Map<string, NativeGraphPoint> {
	const { ids, outgoing, order } = topology(data), indices = new Map<string, number>(), low = new Map<string, number>();
	const stack: string[] = [], stacked = new Set<string>(), components: string[][] = [];
	function visit(id: string): void {
		indices.set(id, indices.size); low.set(id, indices.get(id)!); stack.push(id); stacked.add(id);
		for (const next of [...outgoing.get(id)!].sort(compare)) {
			if (!indices.has(next)) { visit(next); low.set(id, Math.min(low.get(id)!, low.get(next)!)); }
			else if (stacked.has(next)) low.set(id, Math.min(low.get(id)!, indices.get(next)!));
		}
		if (low.get(id) !== indices.get(id)) return;
		const component: string[] = [];
		let next: string;
		do { next = stack.pop()!; stacked.delete(next); component.push(next); } while (next !== id);
		components.push(component.sort(order));
	}
	for (const id of ids) if (!indices.has(id)) visit(id);
	const groupOf = new Map(components.flatMap((group, index) => group.map(id => [id, index] as const)));
	const successors = components.map(() => new Set<number>()), predecessors = components.map(() => new Set<number>());
	const indegree = components.map(() => 0), ranks = components.map(() => 0);
	for (const [id, targets] of outgoing) for (const target of targets) {
		const from = groupOf.get(id)!, to = groupOf.get(target)!;
		if (from !== to && !successors[from].has(to)) { successors[from].add(to); predecessors[to].add(from); indegree[to]++; }
	}
	const groupOrder = (a: number, b: number) => order(components[a][0], components[b][0]);
	const ready = components.map((_, index) => index).filter(index => indegree[index] === 0).sort(groupOrder);
	while (ready.length) {
		const from = ready.shift()!;
		for (const to of [...successors[from]].sort(groupOrder)) {
			ranks[to] = Math.max(ranks[to], ranks[from] + 1);
			if (--indegree[to] === 0) { ready.push(to); ready.sort(groupOrder); }
		}
	}
	const layers = new Map<number, number[]>();
	for (const index of components.keys()) {
		if (!layers.has(ranks[index])) layers.set(ranks[index], []);
		layers.get(ranks[index])!.push(index);
	}
	const orderedLayers = [...layers].sort(([a], [b]) => a - b);
	for (const [, layer] of orderedLayers) layer.sort(groupOrder);
	const centers = new Map<number, number>();
	function center(layer: number[]): void {
		let at = -(layer.reduce((count, group) => count + components[group].length, 0) - 1) / 2;
		for (const group of layer) {
			centers.set(group, at + (components[group].length - 1) / 2);
			at += components[group].length;
		}
	}
	for (const [, layer] of orderedLayers) center(layer);
	// Bounded barycenter sweeps reduce crossings using only condensation edges.
	// Move whole cycle blocks so every member keeps its directed rank and adjacency.
	for (let pass = 0; pass < 4; pass++) for (const forward of [true, false]) {
		const neighbours = forward ? predecessors : successors;
		for (const [, layer] of forward ? orderedLayers : [...orderedLayers].reverse()) {
			const scores = new Map(layer.map(group => {
				const linked = [...neighbours[group]];
				return [group, linked.length ? linked.reduce((sum, next) => sum + centers.get(next)!, 0) / linked.length : centers.get(group)!] as const;
			}));
			layer.sort((a, b) => scores.get(a)! - scores.get(b)! || groupOrder(a, b));
			center(layer);
		}
	}
	const result = new Map<string, NativeGraphPoint>();
	for (const [rank, layer] of orderedLayers) for (const group of layer) {
		components[group].forEach((id, at) => result.set(id, [rank * 72, (centers.get(group)! + at - (components[group].length - 1) / 2) * 36]));
	}
	return result;
}

/** Resolve visual collisions without feeding transient state back into the force simulation. */
function separate(points: GraphPosition[], left: number, top: number, right: number, bottom: number, layered: boolean): void {
	const width = right - left, height = bottom - top;
	const gap = Math.min(26, Math.sqrt(width * height / Math.max(1, points.length) / 1.5));
	const ordered = layered ? [...points].sort((a, b) => a.x - b.x || a.y - b.y || compare(a.node.id, b.node.id)) : undefined;
	const clamp = (point: GraphPosition) => { point.x = Math.max(left, Math.min(right, point.x)); point.y = Math.max(top, Math.min(bottom, point.y)); };
	for (let pass = 0; pass < 160; pass++) {
		let overlap = 0;
		for (let a = 0; a < points.length; a++) for (let b = a + 1; b < points.length; b++) {
			const p = points[a], q = points[b];
			let dx = q.x - p.x, dy = q.y - p.y, distance = Math.hypot(dx, dy);
			if (distance >= gap) continue;
			if (layered) {
				// Preserve directed rank columns while there is enough vertical room.
				const shift = (Math.sqrt(Math.max(0, gap * gap - dx * dx)) - Math.abs(dy)) / 2, sign = dy < 0 ? -1 : 1;
				p.y -= sign * shift; q.y += sign * shift; overlap = Math.max(overlap, shift);
			} else {
				if (distance < .001) { const angle = (a * 31 + b * 17) * 2.399963229728653; dx = Math.cos(angle); dy = Math.sin(angle); distance = 1; }
				const shift = (gap - distance) / 2;
				p.x -= dx / distance * shift; p.y -= dy / distance * shift;
				q.x += dx / distance * shift; q.y += dy / distance * shift; overlap = Math.max(overlap, shift);
			}
			clamp(p); clamp(q);
		}
		if (overlap < .01) return;
	}
	// Dense boundary jams use nearest free slots, still ordered by settled geometry.
	// This is a drawing-space packing fallback, not a new relationship or grouping.
	let jammed = false;
	for (let a = 0; a < points.length && !jammed; a++) for (let b = a + 1; b < points.length; b++) {
		if (Math.hypot(points[a].x - points[b].x, points[a].y - points[b].y) < gap - .2) { jammed = true; break; }
	}
	if (!jammed) return;
	const columns = Math.max(1, Math.floor(width / gap) + 1), rows = Math.max(1, Math.ceil(points.length / columns));
	const slots: NativeGraphPoint[] = [];
	for (let y = 0; y < rows; y++) for (let x = 0; x < columns; x++) slots.push([
		columns === 1 ? (left + right) / 2 : left + x * width / (columns - 1),
		rows === 1 ? (top + bottom) / 2 : top + y * height / (rows - 1),
	]);
	if (ordered) {
		// Overfull ranks wrap through neighbouring columns in directed-rank order.
		slots.sort((a, b) => a[0] - b[0] || a[1] - b[1]);
		ordered.forEach((point, index) => { [point.x, point.y] = slots[index]; });
		return;
	}
	for (const point of [...points].sort((a, b) => a.x - b.x || a.y - b.y || compare(a.node.id, b.node.id))) {
		let best = 0, distance = Infinity;
		for (let at = 0; at < slots.length; at++) {
			const candidate = Math.hypot(point.x - slots[at][0], point.y - slots[at][1]);
			if (candidate < distance) { distance = candidate; best = at; }
		}
		[point.x, point.y] = slots.splice(best, 1)[0];
	}
}

/** Settled evidence geometry only; activity, selection, theme and actions never enter this cache. */
export function nativeGraphLayout(data: GraphData, size: GraphPreviewSize, choice: GraphAppearance["layout"] = "force"): NativeGraphLayout {
	let variants = layouts.get(data);
	const key = `${size}:${choice}`, previous = variants?.get(key);
	if (previous) return previous;
	const links = representativeLinks(data), roots = new Set(data.roots);
	const width = size === "compact" ? 280 : size === "overview" ? 760 : 560;
	const height = size === "compact" ? 176 : size === "overview" ? 320 : data.nodes.length <= 8 ? 156 : data.nodes.length <= 24 ? 216 : 264;
	let positions: Map<string, GraphPosition>, project: (x: number, y: number) => NativeGraphPoint;
	let geometries: NativeCurve[];
	if (choice === "force" && size !== "compact") {
		// Preserve established inline/tool-response geometry when no new mode is requested.
		const layout = layoutGraph(data);
		positions = layout.positions;
		geometries = links.map((_, index) => edgeGeometry(links, index, positions)!);
		let left = layout.bounds.x, top = layout.bounds.y, right = left + layout.width, bottom = top + layout.height;
		for (const edge of geometries) for (let at = 0; at < edge.control.length; at += 2) {
			left = Math.min(left, edge.control[at]); right = Math.max(right, edge.control[at]);
			top = Math.min(top, edge.control[at + 1]); bottom = Math.max(bottom, edge.control[at + 1]);
		}
		const scale = Math.min(2.4, (width - 64) / Math.max(1, right - left), (height - 48) / Math.max(1, bottom - top));
		project = (x, y) => [(x - (left + right) / 2) * scale + width / 2, (y - (top + bottom) / 2) * scale + height / 2];
	} else {
		if (choice === "force") positions = new Map([...layoutGraph(data).positions].map(([id, point]) => [id, { ...point }]));
		else {
			const coordinates = choice === "radial" ? radialPositions(data) : choice === "components" ? componentPositions(data) : layeredPositions(data);
			positions = new Map([...data.nodes].sort((a, b) => compare(a.id, b.id)).map(node => [node.id, {
				node, x: coordinates.get(node.id)![0], y: coordinates.get(node.id)![1], radius: 0, degree: 0, category: "",
			}]));
			for (const edge of data.edges) { positions.get(edge.from)!.degree++; positions.get(edge.to)!.degree++; }
		}
		const points = [...positions.values()];
		for (const point of points) point.radius = roots.has(point.node.id) || point.degree >= 4 ? 5 : 3.5;
		const pairs = new Map<string, number>();
		for (const link of links) { const pair = JSON.stringify([link.from, link.to]); pairs.set(pair, (pairs.get(pair) ?? 0) + 1); }
		let inset = 20, headroom = 20;
		for (const link of links) {
			const count = pairs.get(JSON.stringify([link.from, link.to]))!;
			if (link.from === link.to) {
				const size = positions.get(link.from)!.radius + 13 + (count - 1) * 5;
				inset = Math.max(inset, size + 4); headroom = Math.max(headroom, size * 2 + 4);
			} else {
				const reverse = pairs.has(JSON.stringify([link.to, link.from]));
				inset = Math.max(inset, (count - 1) * 5 + (reverse ? 15 : 0) + 4);
			}
		}
		const left = inset, right = width - inset, top = Math.max(inset, headroom), bottom = height - inset;
		const xs = points.map(point => point.x), ys = points.map(point => point.y);
		const minX = Math.min(...xs), maxX = Math.max(...xs), minY = Math.min(...ys), maxY = Math.max(...ys);
		for (const point of points) {
			point.x = maxX === minX ? width / 2 : left + (point.x - minX) / (maxX - minX) * (right - left);
			point.y = maxY === minY ? (top + bottom) / 2 : top + (point.y - minY) / (maxY - minY) * (bottom - top);
		}
		separate(points, left, top, right, bottom, choice === "layers");
		geometries = links.map((_, index) => edgeGeometry(links, index, positions)!);
		project = (x, y) => [x, y];
	}
	const dots = [...positions.values()].map(point => {
		const [x, y] = project(point.x, point.y);
		return { node: point.node, x, y, radius: roots.has(point.node.id) || point.degree >= 4 ? 5 : 3.5, degree: point.degree, root: roots.has(point.node.id) };
	});
	const byId = new Map(dots.map(dot => [dot.node.id, dot]));
	const routes = geometries.map((edge, index): NativeGraphRoute => {
		const controls: NativeGraphPoint[] = [project(edge.from[0], edge.from[1])];
		for (let at = 0; at < edge.control.length; at += 2) controls.push(project(edge.control[at], edge.control[at + 1]));
		controls.push(project(edge.to[0], edge.to[1]));
		if (!edge.self) {
			// Clip to the rendered dots, not the simulation's differently scaled radii.
			for (const [at, id, adjacent] of [[0, links[index].from, 1], [controls.length - 1, links[index].to, controls.length - 2]] as const) {
				const dot = byId.get(id)!, tangent = controls[adjacent], dx = tangent[0] - dot.x, dy = tangent[1] - dot.y;
				const distance = Math.hypot(dx, dy) || 1;
				controls[at] = [dot.x + dx / distance * (dot.radius + 1), dot.y + dy / distance * (dot.radius + 1)];
			}
		}
		const points: NativeGraphPoint[] = [controls[0]];
		// De Casteljau subdivision: <=.12px curve error and <=48px chords.
		// Native CSS residual classes keep positional rounding below .13px per axis at 760px.
		function subdivide(curve: NativeGraphPoint[], depth: number): void {
			const start = curve[0], end = curve.at(-1)!, dx = end[0] - start[0], dy = end[1] - start[1];
			const length = Math.hypot(dx, dy);
			const deviation = Math.max(...curve.slice(1, -1).map(point => length ? Math.abs(dy * (point[0] - start[0]) - dx * (point[1] - start[1])) / length : Math.hypot(point[0] - start[0], point[1] - start[1])));
			if (depth === 8 || (length <= 48 && deviation <= .12)) { points.push(end); return; }
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
	variants.set(key, prepared);
	return prepared;
}
