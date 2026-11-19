import type { GraphData } from "./graph";
import { DEFAULT_GRAPH_APPEARANCE, type GraphAppearance } from "./graph-appearance";
import { GRAPH_CANVAS } from "./graph-canvas";
import { bindTermVector, centroid, cosine, graphEmbedding, semanticFamilies, semanticMap, similarityMap } from "./graph-data/embedding.js";
import { affinityOf, bindAffinity, bindDrawingWeight, drawingWeight, edgeGeometry, isRemainder, layoutGraph, relationKind, representativeLinks, sharedConceptFamilies, type GraphPosition, type GraphRelationKind, type SharedConceptFamily } from "./graph-data/layout.js";
import { conceptPalette } from "./graph-data/palette.js";
import type { GraphPreviewSize } from "./graph-preview";
import { compactTooltip } from "./tooltip-text";

export type NativeGraphPoint = readonly [number, number];
export interface NativeGraphDot {
	node: GraphData["nodes"][number];
	x: number;
	y: number;
	/** 3.5 to 9.5 px in .5 px steps. */
	radius: number;
	degree: number;
	root: boolean;
	/** Bound drawing weight, for example the member count of an overview group. */
	weight?: number;
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
	/** Weight relative to the heaviest route, only when edge drawing weights are bound. 4 is the heaviest. */
	tier?: 1 | 2 | 3 | 4;
}
/**
 * Area around its member dots. "family": a doc concept family. "module": map-region groups or drill-down members that share a folder.
 * "lexical": shared identifier terms, from a map region with recorded links, from term similarity alone, or from a drill-down content subconcept.
 * A territory never moves a dot.
 */
export interface NativeGraphTerritory {
	key: string;
	kind: "family" | "module" | "lexical";
	label: string;
	/** Hover text for the label chip, already compact. */
	title?: string;
	/** Shared identifier terms of a lexical family; other kinds have none. */
	terms: string[];
	members: string[];
	x: number;
	y: number;
	rx: number;
	ry: number;
	/** Mean pairwise cosine of the member term vectors. */
	cohesion: number;
	color: { light: string; dark: string };
}
export interface NativeGraphLayout {
	width: number;
	height: number;
	dots: NativeGraphDot[];
	routes: NativeGraphRoute[];
	/** Force and concept map only; the other layouts return none. */
	territories: NativeGraphTerritory[];
}
interface LayoutBounds { left: number; top: number; right: number; bottom: number }
interface PackingBox { key: string; x: number; y: number; rx: number; ry: number; shift?: number }
const maxStretch = 1.6, gapFactor = 1.6, boxGap = 8;
const layouts = new WeakMap<GraphData, Map<string, NativeGraphLayout>>();
const conceptMaps = new WeakMap<GraphData, ReadonlyMap<string, NativeGraphPoint> | null>();
const compare = (a: string, b: string) => a < b ? -1 : a > b ? 1 : 0;

/** Eigenvectors have no sign. The first non-central node in weight/ID order fixes each axis. */
function orientMap(mapped: ReadonlyMap<string, NativeGraphPoint>, rank: readonly string[]): Map<string, NativeGraphPoint> {
	const sign = (axis: 0 | 1) => {
		const lead = rank.find(id => Math.abs(mapped.get(id)![axis]) > 1e-6);
		return lead !== undefined && mapped.get(lead)![axis] > 0 ? -1 : 1;
	};
	const sx = sign(0), sy = sign(1);
	return new Map([...mapped].map(([id, point]): [string, NativeGraphPoint] => [id, [point[0] * sx, point[1] * sy]]));
}

/**
 * Concept similarity S: shared identifier terms .6, recorded links .3 and one shared doc concept family .1.
 * With bound semantic affinity (semctx search profiles), terms and affinity take .35 each and links .2.
 * Classical MDS on the distance 1 − S places the nodes; the principal axis is horizontal.
 * The heaviest node, then the first ID, takes the left and top side of each axis.
 * Undefined without term, family or affinity evidence, or when all pairs are equally similar.
 */
function conceptMap(data: GraphData): ReadonlyMap<string, NativeGraphPoint> | undefined {
	const cached = conceptMaps.get(data);
	if (cached !== undefined) return cached ?? undefined;
	const ids = [...new Set(data.nodes.map(node => node.id))].sort(compare), n = ids.length, at = new Map(ids.map((id, index) => [id, index]));
	const { vectors, informative } = graphEmbedding(data), shared = sharedConceptFamilies(data.nodes), semantic = affinityOf(data);
	const [termShare, linkShare, semanticShare] = semantic ? [.35, .2, .35] : [.6, .3, 0];
	let result: Map<string, NativeGraphPoint> | undefined;
	if (n >= 2 && n <= 512 && (informative || shared.length || semantic)) {
		const family = new Map(shared.flatMap(item => item.members.map(id => [id, item.key] as const)));
		// Links count in both directions. Project membership repeats the hierarchy, so it stays out.
		const pairs = new Map<number, number[]>();
		for (const edge of data.edges) {
			const a = at.get(edge.from), b = at.get(edge.to);
			if (a === undefined || b === undefined || a === b || relationKind(edge) === "association") continue;
			const pair = Math.min(a, b) * n + Math.max(a, b), weights = pairs.get(pair);
			if (weights) weights.push(drawingWeight(edge) ?? 1); else pairs.set(pair, [drawingWeight(edge) ?? 1]);
		}
		// Sorted sums keep the totals independent of the edge order.
		const affinity = new Map([...pairs].map(([pair, weights]) => [pair, Math.log1p(weights.sort((x, y) => x - y).reduce((sum, weight) => sum + weight, 0))]));
		const strongest = Math.max(0, ...affinity.values()), similarity = new Float64Array(n * n);
		let low = Infinity, high = -Infinity;
		for (let i = 0; i < n; i++) {
			similarity[i * n + i] = 1;
			for (let j = i + 1; j < n; j++) {
				const value = termShare * cosine(vectors.get(ids[i]), vectors.get(ids[j])) + (strongest ? linkShare * (affinity.get(i * n + j) ?? 0) / strongest : 0) +
					(semantic ? semanticShare * semantic(ids[i], ids[j]) : 0) + (family.has(ids[i]) && family.get(ids[i]) === family.get(ids[j]) ? .1 : 0);
				similarity[i * n + j] = similarity[j * n + i] = value;
				low = Math.min(low, value); high = Math.max(high, value);
			}
		}
		// With S′ = 1 − (1 − S)² / 2, the squared map distance 2 − 2·S′ is (1 − S)².
		const mapped = high - low > 1e-9 ? similarityMap(ids, (a, b) => 1 - (1 - similarity[at.get(a)! * n + at.get(b)!]) ** 2 / 2) : undefined;
		if (mapped) {
			const weights = new Map(data.nodes.map(node => [node.id, drawingWeight(node) ?? 0]));
			const rank = [...ids].sort((a, b) => weights.get(b)! - weights.get(a)! || compare(a, b));
			result = orientMap(mapped, rank);
		}
	}
	conceptMaps.set(data, result ?? null);
	return result;
}

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
	const { roots, neighbours, components, order } = topology(data), semantic = conceptMap(data), result = new Map<string, NativeGraphPoint>();
	const largest = components.reduce<string[] | undefined>((best, component) => !best || component.length > best.length ? component : best, undefined);
	// One dominant map carries the isolated nodes on one more outer ring instead of a detached row.
	// That ring encodes no distance: an isolated node has no recorded relation.
	const isolated = largest && largest.length >= 3 && components.every(component => component === largest || component.length < largest.length)
		? components.filter(component => component.length === 1).map(([id]) => id).sort(order) : [];
	let left = 0, top = 0, rowHeight = 0;
	const blocks = components.filter(component => component.length > 1 || !isolated.length).map(component => {
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
		// Breadth-first depths are contiguous, so the ring index is the undirected distance.
		const levels = [...rings].sort(([a], [b]) => a - b).map(([, ring]) => ring.sort(order));
		if (component === largest && isolated.length) levels.push(isolated);
		if (semantic) {
			// Related nodes sit side by side: each ring follows the concept map angle around the seeds, clockwise from the top.
			const cx = seeds.reduce((sum, id) => sum + semantic.get(id)![0], 0) / seeds.length, cy = seeds.reduce((sum, id) => sum + semantic.get(id)![1], 0) / seeds.length;
			const angles = new Map(levels.flat().map(id => [id, (Math.atan2(semantic.get(id)![1] - cy, semantic.get(id)![0] - cx) + Math.PI * 2.5) % (Math.PI * 2)]));
			for (const ring of levels) ring.sort((a, b) => angles.get(a)! - angles.get(b)! || order(a, b));
		}
		let radius = 0;
		const points = new Map<string, NativeGraphPoint>();
		levels.forEach((ring, depth) => {
			radius = depth === 0 && ring.length === 1 ? 0 : Math.max(radius + 42, ring.length * 30 / (2 * Math.PI));
			ring.forEach((id, index) => {
				const angle = -Math.PI / 2 + index / ring.length * Math.PI * 2;
				points.set(id, [Math.cos(angle) * radius, Math.sin(angle) * radius]);
			});
		});
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

/** Fit settled positions without changing their order. An empty axis does not limit the scale. */
function canvasFit(bounds: LayoutBounds, frame: LayoutBounds, stretch = maxStretch, corner = false) {
	const width = bounds.right - bounds.left, height = bounds.bottom - bounds.top;
	let sx = width ? (frame.right - frame.left) / width : Infinity;
	let sy = height ? (frame.bottom - frame.top) / height : Infinity;
	if (sx === Infinity && sy === Infinity) sx = sy = 1;
	else { sx = Math.min(sx, sy * stretch); sy = Math.min(sy, sx * stretch); }
	const dx = frame.left - bounds.left * sx + (frame.right - frame.left - width * sx) * (corner ? 1 : .5);
	const dy = frame.top - bounds.top * sy + (frame.bottom - frame.top - height * sy) * (corner ? 1 : .5);
	return { sx, sy, dx, dy };
}

function fitPoints(points: GraphPosition[], frame: LayoutBounds, stretch = maxStretch): void {
	if (!points.length) return;
	const xs = points.map(point => point.x), ys = points.map(point => point.y);
	const { sx, sy, dx, dy } = canvasFit({ left: Math.min(...xs), top: Math.min(...ys), right: Math.max(...xs), bottom: Math.max(...ys) }, frame, stretch);
	for (const point of points) { point.x = point.x * sx + dx; point.y = point.y * sy + dy; }
}

const dotGap = (p: GraphPosition, q: GraphPosition) => 2.2 * Math.max(p.radius, q.radius);

/** Resolve visual collisions without feeding transient state back into the force simulation. */
function separate(points: GraphPosition[], left: number, top: number, right: number, bottom: number, layered: boolean): void {
	const width = right - left, height = bottom - top;
	const base = Math.min(26, Math.sqrt(width * height / Math.max(1, points.length) / 1.5));
	// The gap grows with both radii so that large weighted dots never touch.
	const gap = (p: GraphPosition, q: GraphPosition) => Math.max(base, p.radius + q.radius + 10, dotGap(p, q));
	const ordered = layered ? [...points].sort((a, b) => a.x - b.x || a.y - b.y || compare(a.node.id, b.node.id)) : undefined;
	const clamp = (point: GraphPosition) => { point.x = Math.max(left, Math.min(right, point.x)); point.y = Math.max(top, Math.min(bottom, point.y)); };
	for (let pass = 0; pass < 160; pass++) {
		let overlap = 0;
		for (let a = 0; a < points.length; a++) for (let b = a + 1; b < points.length; b++) {
			const p = points[a], q = points[b], wanted = gap(p, q);
			let dx = q.x - p.x, dy = q.y - p.y, distance = Math.hypot(dx, dy);
			if (distance >= wanted) continue;
			if (layered) {
				// Preserve directed rank columns while there is enough vertical room.
				const shift = (Math.sqrt(Math.max(0, wanted * wanted - dx * dx)) - Math.abs(dy)) / 2, sign = dy < 0 ? -1 : 1;
				p.y -= sign * shift; q.y += sign * shift; overlap = Math.max(overlap, shift);
			} else {
				if (distance < .001) { const angle = (a * 31 + b * 17) * 2.399963229728653; dx = Math.cos(angle); dy = Math.sin(angle); distance = 1; }
				const shift = (wanted - distance) / 2;
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
		if (Math.hypot(points[a].x - points[b].x, points[a].y - points[b].y) < gap(points[a], points[b]) - .2) { jammed = true; break; }
	}
	if (!jammed) return;
	const pitch = Math.max(base, 2 * Math.max(...points.map(point => point.radius)) + 10);
	let columns = Math.max(1, Math.floor(width / pitch) + 1), rows = Math.max(1, Math.ceil(points.length / columns));
	if (rows > 1 && height / (rows - 1) < pitch) {
		// Use the grid with the largest minimum spacing when neither axis can keep the preferred pitch.
		let best = height / (rows - 1);
		for (let count = 1; count <= points.length; count++) {
			const rowCount = Math.ceil(points.length / count);
			const spacing = Math.min(count > 1 ? width / (count - 1) : Infinity, rowCount > 1 ? height / (rowCount - 1) : Infinity);
			if (spacing > best) { columns = count; rows = rowCount; best = spacing; }
		}
	}
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

/** The same envelope sizes both a family island and its final territory. */
function memberEllipse(members: readonly { x: number; y: number; radius: number }[], compact: boolean) {
	const x = members.reduce((sum, point) => sum + point.x, 0) / members.length, y = members.reduce((sum, point) => sum + point.y, 0) / members.length;
	const rx = Math.max(compact ? 14 : 18, ...members.map(point => Math.abs(point.x - x) + point.radius + 10));
	const ry = Math.max(compact ? 12 : 15, ...members.map(point => Math.abs(point.y - y) + point.radius + 9));
	// Points off both axes need more than their axis extents to fit inside an ellipse.
	const grow = Math.max(1, ...members.map(point => Math.hypot((Math.abs(point.x - x) + point.radius) / rx, (Math.abs(point.y - y) + point.radius) / ry)));
	return { x, y, rx: rx * grow, ry: ry * grow };
}

/**
 * Half value, half rank on [0, 1]: one distant outlier cannot squeeze every other position into a corner.
 * Values equal within projection precision share their mean rank.
 */
function rankBlend(values: readonly number[]): number[] {
	const ordered = values.map((_, index) => index).sort((a, b) => values[a] - values[b]);
	const low = values[ordered[0]], range = values[ordered.at(-1)!] - low;
	if (!(range >= 1e-6)) return values.map(() => .5);
	const shares = values.map(() => 0);
	for (let start = 0; start < ordered.length;) {
		let end = start;
		while (end + 1 < ordered.length && values[ordered[end + 1]] - values[ordered[start]] <= range * 1e-6) end++;
		const fraction = ((values[ordered[start]] + values[ordered[end]]) / 2 - low) / range;
		const share = (fraction + (start + end) / 2 / (ordered.length - 1)) / 2;
		for (let at = start; at <= end; at++) shares[ordered[at]] = share;
		start = end + 1;
	}
	return shares;
}

/**
 * Dot-and-label footprints must not overlap. A preview label sits above and to the right of its dot by default.
 * Overlapping footprints move apart vertically, so a crowded island reads as a short list.
 * Positions stay deterministic: pairs resolve in input order for a bounded number of passes.
 */
function separateLabels(points: GraphPosition[], width: (point: GraphPosition) => number): void {
	// The label box spans 14 px and ends 1.5 px below the dot top; see `graphLabels` in graph-preview.ts.
	const ends = points.map(point => point.x + point.radius + 5.5 + width(point));
	for (let pass = 0; pass < 80; pass++) {
		let moved = false;
		for (let a = 0; a < points.length; a++) for (let b = a + 1; b < points.length; b++) {
			const p = points[a], q = points[b];
			const overlapX = Math.min(ends[a], ends[b]) - Math.max(p.x - p.radius, q.x - q.radius) + 4;
			if (overlapX <= 0) continue;
			const [upper, lower] = q.y > p.y || (q.y === p.y && compare(p.node.id, q.node.id) < 0) ? [p, q] : [q, p];
			const overlapY = upper.y + upper.radius + lower.radius + 15.5 - lower.y;
			if (overlapY <= 0) continue;
			upper.y -= overlapY / 2; lower.y += overlapY / 2; moved = true;
		}
		if (!moved) return;
	}
}

/**
 * A family is one outer-map island, not a weak pairwise hint. Its centroid and the ungrouped nodes form the outer MDS.
 * Inside an island, the concept map of its members and their own links keeps related members side by side.
 * Packing reserves each family's final territory before any singleton is placed. A folded remainder takes the free spot nearest the lower right corner.
 * `labelWidth`, when present, reserves room for each dot's label.
 */
function familyPositions(data: GraphData, points: GraphPosition[], families: readonly SharedConceptFamily[], frame: LayoutBounds,
	labelWidth?: (point: GraphPosition) => number): boolean {
	if (!families.length) return false;
	const { left, top, right, bottom } = frame;
	const { vectors } = graphEmbedding(data), byId = new Map(points.map(point => [point.node.id, point]));
	const grouped = new Set(families.flatMap(family => family.members)), remainders = points.filter(point => isRemainder(point.node));
	const groups = [
		...families.map(family => ({ key: JSON.stringify(["family", family.key]), members: family.members.map(id => byId.get(id)!) })),
		...points.filter(point => !grouped.has(point.node.id) && !isRemainder(point.node)).map(point => ({ key: JSON.stringify(["node", point.node.id]), members: [point] })),
	].sort((a, b) => compare(a.key, b.key));
	const groupOf = new Map(groups.flatMap(group => group.members.map(point => [point.node.id, group.key] as const)));
	const nodes = groups.map(group => {
		const node: GraphData["nodes"][number] = { id: group.key, label: "", category: "group", hit: null, detail: null };
		bindTermVector(node, centroid(group.members.map(point => vectors.get(point.node.id))));
		bindDrawingWeight(node, group.members.reduce((sum, point) => sum + (drawingWeight(point.node) ?? 1), 0));
		return node;
	});
	const edges = data.edges.flatMap(edge => {
		const from = groupOf.get(edge.from), to = groupOf.get(edge.to);
		if (from === undefined || to === undefined || from === to) return [];
		const link = { ...edge, from, to };
		bindDrawingWeight(link, drawingWeight(edge) ?? 1);
		return [link];
	});
	const outerData = { ...data, nodes, edges, roots: [] }, semantic = affinityOf(data);
	// Island affinity is the mean affinity of their member pairs.
	if (semantic) {
		const members = new Map(groups.map(group => [group.key, group.members.map(point => point.node.id)]));
		bindAffinity(outerData, (a, b) => {
			const left = members.get(a) ?? [], right = members.get(b) ?? [];
			let sum = 0; for (const x of left) for (const y of right) sum += semantic(x, y);
			return left.length && right.length ? sum / (left.length * right.length) : 0;
		});
	}
	const mapped = conceptMap(outerData);
	// The heaviest original node fixes the outer orientation, even when it belongs to a larger family.
	const rank = points.filter(point => groupOf.has(point.node.id)).sort((a, b) => (drawingWeight(b.node) ?? 0) - (drawingWeight(a.node) ?? 0) || compare(a.node.id, b.node.id)).map(point => groupOf.get(point.node.id)!);
	const outer = mapped && orientMap(mapped, rank);
	// Islands grow with the canvas, so a large popup spends its room on spacing instead of empty margins.
	// If labels make the islands too large, keep the wide member spacing without reserving labels.
	// The smallest canvas can trade space around territories for room between their dots.
	const floor = Math.min(16, Math.max(10, Math.sqrt((right - left) * (bottom - top)) / 14));
	const roomy = Math.max(floor, Math.min(32, Math.sqrt(.3 * (right - left) * (bottom - top) / Math.max(1, points.length)) * .55));
	const attempts = [
		...(labelWidth ? [
			{ unit: roomy, labels: labelWidth, reserve: true, padding: 10 },
			{ unit: Math.max(floor, roomy * .75), labels: labelWidth, reserve: true, padding: 10 },
			{ unit: floor, labels: labelWidth, reserve: false, padding: 10 },
		] : []),
		{ unit: roomy, labels: undefined, reserve: false, padding: 10 },
		{ unit: floor, labels: undefined, reserve: false, padding: 10 },
		{ unit: floor * .7, labels: undefined, reserve: false, padding: 4 },
	];
	const local = new Map(groups.filter(group => group.members.length > 1).map(group => {
		const ids = new Set(group.members.map(point => point.node.id));
		const island = { ...data, nodes: group.members.map(point => point.node), edges: data.edges.filter(edge => ids.has(edge.from) && ids.has(edge.to)), roots: [] };
		bindAffinity(island, semantic);
		return [group.key, conceptMap(island) ?? semanticMap([...ids], vectors)];
	}));
	const arrange = (unit: number, width: ((point: GraphPosition) => number) | undefined, reserve: boolean, padding: number): boolean => {
		const islands = groups.map((group, groupIndex) => {
			const members = group.members;
			if (members.length > 1) {
				const extent = Math.max(24, Math.sqrt(members.length) * unit), coordinates = local.get(group.key);
				const spread = coordinates ? Math.max(1e-6, ...[...coordinates.values()].map(([x, y]) => Math.hypot(x, y))) : 1;
				members.forEach((point, index) => {
					const position = coordinates?.get(point.node.id), angle = index * Math.PI * 2 / members.length;
					// A pair reads down the page, leaving room for both node labels beside its dots.
					point.x = position ? position[members.length === 2 ? 1 : 0] / spread * extent : Math.cos(angle) * extent;
					point.y = position ? position[members.length === 2 ? 0 : 1] / spread * extent : Math.sin(angle) * extent;
				});
				const localWidth = members.length > 2 ? extent * 1.25 : extent;
				for (const point of members) point.x *= localWidth / extent;
				separate(members, -localWidth, -extent, localWidth, extent, false);
				if (width) separateLabels(members, width);
			} else { members[0].x = 0; members[0].y = 0; }
			const envelope = memberEllipse(members, false);
			// Keep the territory border and label chip clear of neighbouring islands.
			const rx = members.length > 1 ? envelope.rx + padding : members[0].radius + padding + 3;
			const ry = members.length > 1 ? envelope.ry + padding : rx;
			// Labels read to the right of their dots. The packing box also reserves the part of a label that leaves the island.
			const overhang = width && reserve ? Math.max(0, ...members.map(point => point.x + point.radius + 6 + width(point) - envelope.x - rx)) : 0;
			for (const point of members) { point.x -= envelope.x + overhang / 2; point.y -= envelope.y; }
			const seed = outer?.get(group.key);
			return { ...group, rx: rx + overhang / 2, ry, shift: overhang / 2, seed: seed ?? [groupIndex, 0], x: 0, y: 0 };
		});
		const xs = rankBlend(islands.map(island => island.seed[0])), ys = rankBlend(islands.map(island => island.seed[1]));
		islands.forEach((island, index) => { island.x = left + xs[index] * (right - left); island.y = top + ys[index] * (bottom - top); });
		// The remainder starts in the lower right corner and reserves its own label.
		const tail = remainders.map(point => {
			const overhang = width && reserve ? Math.max(0, width(point) + 6 - 13) : 0;
			return { point, key: JSON.stringify(["remainder", point.node.id]), x: right, y: bottom, rx: point.radius + 13 + overhang / 2, ry: point.radius + 13, shift: overhang / 2 };
		});
		const boxes = [...islands, ...tail];
		if (!packBoxes(boxes, frame)) return false;
		compactIslands(islands, boxes, frame, Math.sqrt((right - left) * (bottom - top) / points.length));
		// A uniform final fit preserves the capped distance ratios when there are more than two islands.
		const fit = fitBoxes(boxes, frame, islands.length > 2, tail.length > 0);
		for (const island of islands) for (const point of island.members) {
			point.x = (point.x + island.x) * fit.sx + fit.dx;
			point.y = (point.y + island.y) * fit.sy + fit.dy;
		}
		for (const box of tail) {
			box.point.x = (box.x - box.shift) * fit.sx + fit.dx;
			box.point.y = box.y * fit.sy + fit.dy;
		}
		return true;
	};
	if (attempts.some(({ unit, labels, reserve, padding }) => arrange(unit, labels, reserve, padding))) return true;
	// If the footprints cannot fit, use the outer map as the seed for dot packing.
	for (const [index, group] of groups.entries()) for (const point of group.members) {
		[point.x, point.y] = outer?.get(group.key) ?? [index, 0];
	}
	return false;
}

/**
 * Pushes overlapping boxes apart along their relatively smaller overlap and keeps them inside the canvas.
 * Boxes keep an 8 px gap. Pairs resolve in input order for a bounded number of passes. True when no overlap remains.
 */
function packBoxes(boxes: PackingBox[], { left, top, right, bottom }: LayoutBounds): boolean {
	if (boxes.some(box => 2 * box.rx > right - left || 2 * box.ry > bottom - top)) return false;
	const clamp = (box: typeof boxes[number]) => {
		box.x = Math.max(left + box.rx, Math.min(right - box.rx, box.x)); box.y = Math.max(top + box.ry, Math.min(bottom - box.ry, box.y));
	};
	boxes.forEach(clamp);
	for (let pass = 0; pass < 240; pass++) {
		let overlap = 0;
		for (let a = 0; a < boxes.length; a++) for (let b = a + 1; b < boxes.length; b++) {
			const p = boxes[a], q = boxes[b], dx = q.x - p.x, dy = q.y - p.y;
			const ox = p.rx + q.rx + boxGap - Math.abs(dx), oy = p.ry + q.ry + boxGap - Math.abs(dy);
			if (ox <= 0 || oy <= 0) continue;
			overlap = Math.max(overlap, Math.min(ox, oy));
			const tie = compare(p.key, q.key) <= 0 ? 1 : -1;
			const canSeparateX = 2 * (p.rx + q.rx) + boxGap <= right - left;
			const canSeparateY = 2 * (p.ry + q.ry) + boxGap <= bottom - top;
			if (!canSeparateX && !canSeparateY) return false;
			if (canSeparateX && (!canSeparateY || ox / (p.rx + q.rx) < oy / (p.ry + q.ry))) {
				const sign = Math.sign(dx) || tie; p.x -= sign * ox / 2; q.x += sign * ox / 2;
			} else {
				const sign = Math.sign(dy) || tie; p.y -= sign * oy / 2; q.y += sign * oy / 2;
			}
			clamp(p); clamp(q);
		}
		if (overlap < .5) return true;
	}
	return false;
}

function fitBoxes(boxes: PackingBox[], frame: LayoutBounds, uniform: boolean, corner = false) {
	return canvasFit({
		left: Math.min(...boxes.map(box => box.x - box.rx)), right: Math.max(...boxes.map(box => box.x + box.rx)),
		top: Math.min(...boxes.map(box => box.y - box.ry)), bottom: Math.max(...boxes.map(box => box.y + box.ry)),
	}, frame, uniform ? 1 : maxStretch, corner);
}

/**
 * Move outlying island centers toward their nearest neighbors. Keep label space and retries for overlap recovery.
 * The folded remainder takes part in packing, but not in the neighbor distances.
 */
function compactIslands(islands: PackingBox[], boxes: PackingBox[], frame: LayoutBounds, pairGap: number): boolean {
	if (islands.length < 2) return false;
	const x = (box: PackingBox) => box.x - (box.shift ?? 0);
	const distance = (a: PackingBox, b: PackingBox) => Math.hypot(x(a) - x(b), a.y - b.y);
	let compacted = false;
	for (let pass = 0; pass < 4; pass++) {
		const nearest = islands.map(p => {
			let pair = p, gap = Infinity;
			for (const q of islands) if (q !== p) {
				const candidate = distance(p, q);
				if (candidate < gap) { pair = q; gap = candidate; }
			}
			return { pair, gap };
		});
		const gaps = nearest.map(item => item.gap).sort((a, b) => a - b), mid = gaps.length >> 1;
		const cap = gapFactor * (gaps[mid] + gaps[(gaps.length - 1) >> 1]) / 2;
		let before: NativeGraphPoint[] | undefined;
		for (const [index, p] of islands.entries()) {
			const q = nearest[index].pair, dx = x(p) - x(q), dy = p.y - q.y, d = Math.hypot(dx, dy);
			if (!d) continue;
			const boxDx = p.x - q.x;
			// Leave room for both boxes on at least one axis.
			const contact = Math.min(
				Math.abs(dx) < 1e-6 || boxDx * dx <= 0 ? Infinity : Math.max(0, d + (p.rx + q.rx + boxGap - Math.abs(boxDx)) * d / Math.abs(dx)),
				Math.abs(dy) < 1e-6 ? Infinity : (p.ry + q.ry + boxGap) * d / Math.abs(dy),
			);
			// The available room also limits gaps when there are only two islands and the ratio is always one.
			const limit = Math.max(contact, Math.min(cap, contact + pairGap));
			if (d - limit < .5) continue;
			before ??= boxes.map((box): NativeGraphPoint => [box.x, box.y]);
			p.x -= dx * (1 - limit / d); p.y -= dy * (1 - limit / d);
		}
		if (!before) break;
		if (!packBoxes(boxes, frame)) {
			boxes.forEach((box, index) => { [box.x, box.y] = before![index]; });
			break;
		}
		compacted = true;
	}
	return compacted;
}

/** Translate settled force groups or lexical islands without changing their membership. */
function compactSettledGroups(points: GraphPosition[], frame: LayoutBounds, groupOf: (point: GraphPosition) => string, limitPairGap: boolean): void {
	const groups = new Map<string, GraphPosition[]>();
	for (const point of points) {
		const key = groupOf(point), members = groups.get(key);
		if (members) members.push(point); else groups.set(key, [point]);
	}
	if (groups.size < 2) return;
	const islands = [...groups].map(([key, members]) => ({ key, members, ...(members.length > 1 ? memberEllipse(members, false) : {
		x: members[0].x, y: members[0].y, rx: members[0].radius + 5, ry: members[0].radius + 5,
	}) }));
	const origins = islands.map(box => [box.x, box.y]);
	const { left, top, right, bottom } = frame;
	const pairGap = limitPairGap ? Math.sqrt((right - left) * (bottom - top) / points.length) : Infinity;
	if (!compactIslands(islands, islands, frame, pairGap)) return;
	const { sx, sy, dx, dy } = fitBoxes(islands, frame, true);
	for (const [index, box] of islands.entries()) for (const point of box.members) {
		point.x = (point.x + box.x - origins[index][0]) * sx + dx;
		point.y = (point.y + box.y - origins[index][1]) * sy + dy;
	}
}

/** Bound families (doc concept families or map regions), or else lexical identifier families, around settled dots. They never feed back into dot positions. */
function territories(data: GraphData, dots: NativeGraphDot[], width: number, height: number, compact: boolean): NativeGraphTerritory[] {
	if (dots.length < 4) return [];
	const { vectors, informative } = graphEmbedding(data), byId = new Map(dots.map(dot => [dot.node.id, dot])), concepts = sharedConceptFamilies(data.nodes);
	type Family = Pick<NativeGraphTerritory, "key" | "kind" | "label" | "title" | "terms" | "members" | "cohesion">;
	// A doc concept family names a concept from the docs, a module region names a folder, and lexical families only show shared identifier words.
	const families: Family[] = concepts.length ? concepts.slice(0, 6).map((family): Family => {
		let total = 0, pairs = 0;
		for (let a = 0; a < family.members.length; a++) for (let b = a + 1; b < family.members.length; b++) {
			total += cosine(vectors.get(family.members[a]), vectors.get(family.members[b])); pairs++;
		}
		const count = family.members.length, shared = { key: family.key, label: family.label, members: family.members, cohesion: total / pairs };
		const groups = family.members.every(id => byId.get(id)?.node.category === "group");
		if (family.kind === "path") return { ...shared, kind: "module", terms: [], title: groups
			? compactTooltip(`${family.label}/ · folder region`, `${count} groups with most files under ${family.label}/`)
			: compactTooltip(`${family.label} · folder`, `${count} nodes in this folder`) };
		if (family.kind === "terms") return { ...shared, kind: "lexical", terms: family.label.split(" · "),
			title: compactTooltip(`${family.label} · shared terms`, `${count} groups with recorded links and these identifier terms`) };
		if (family.kind === "content") return { ...shared, kind: "lexical", terms: [],
			title: compactTooltip(`${family.label} · subconcept`, `${count} ${groups ? "groups" : "nodes"} with related names, headings, links or semctx search results`) };
		const folder = family.key.startsWith("family:") ? family.key.slice("family:".length) : "";
		return { ...shared, kind: "family", terms: [],
			title: compactTooltip(`${family.label} · concept family`, `${count} concepts from docs in ${folder || "the repository root"}`) };
	}) : informative ? semanticFamilies([...byId.keys()].sort(compare), vectors).map((family): Family => ({ ...family, kind: "lexical",
		title: compactTooltip(`${family.label} · shared terms`, "Lexical identifier similarity") })) : [];
	// Cut the overflow of one axis at the canvas margin and center the radius on what remains.
	const fit = (center: number, radius: number, limit: number) => {
		const low = Math.max(2, center - radius), high = Math.min(limit - 2, center + radius);
		return [(low + high) / 2, (high - low) / 2] as const;
	};
	// Squared normalized distance from the ellipse center: below 1 is inside, above 1 is outside.
	const reach = (ellipse: { x: number; y: number; rx: number; ry: number }, x: number, y: number) => ((x - ellipse.x) / ellipse.rx) ** 2 + ((y - ellipse.y) / ellipse.ry) ** 2;
	const candidates = families.flatMap((family): NativeGraphTerritory[] => {
		const members = family.members.map(id => byId.get(id)!), own = new Set(family.members);
		const bounds = memberEllipse(members, compact);
		const [x, fx] = fit(bounds.x, bounds.rx, width), [y, fy] = fit(bounds.y, bounds.ry, height), ellipse = { x, y, rx: fx, ry: fy };
		// The canvas cut can expose a disc edge, but every member center must stay inside and every other center outside.
		// A territory that covers most of the canvas, misses a member or covers an unrelated node shows a false grouping.
		if (Math.PI * fx * fy > width * height * .55 || dots.some(dot => own.has(dot.node.id) ? reach(ellipse, dot.x, dot.y) > 1 : reach(ellipse, dot.x, dot.y) < 1)) return [];
		return [{ key: family.key, kind: family.kind, label: family.label, title: family.title, terms: family.terms, members: family.members, ...ellipse, cohesion: family.cohesion, color: conceptPalette(family.key) }];
	});
	// Where two territories cover each other's centers, the family with more members stays; the key breaks ties.
	const kept: NativeGraphTerritory[] = [];
	for (const candidate of candidates.sort((a, b) => b.members.length - a.members.length || compare(a.key, b.key))) {
		if (kept.every(other => reach(other, candidate.x, candidate.y) >= 1 && reach(candidate, other.x, other.y) >= 1)) kept.push(candidate);
	}
	return kept;
}

/** Settled evidence geometry only; activity, selection, theme and actions never enter this cache. */
export function nativeGraphLayout(data: GraphData, size: GraphPreviewSize, choice: GraphAppearance["layout"] = DEFAULT_GRAPH_APPEARANCE.layout): NativeGraphLayout {
	let variants = layouts.get(data);
	const key = `${size}:${choice}`, previous = variants?.get(key);
	if (previous) return previous;
	// Without term or family evidence the concept map draws the force geometry. Families use their own two-level map.
	// A folded remainder has no evidence of its own, so it never steers the concept map.
	const families = choice === "map" ? sharedConceptFamilies(data.nodes) : [], core = data.nodes.filter(node => !isRemainder(node));
	const mapped = choice === "map" && !families.length ? conceptMap(core.length === data.nodes.length ? data : { ...data, nodes: core, edges: data.edges.filter(edge => core.some(node => node.id === edge.from) && core.some(node => node.id === edge.to)) }) : undefined;
	const mode = choice === "map" && !mapped && !families.length ? "force" : choice;
	const links = representativeLinks(data), roots = new Set(data.roots);
	const weights = new Map(data.nodes.map(node => [node.id, drawingWeight(node)]));
	const heaviest = Math.max(0, ...[...weights.values()].map(weight => weight ?? 0));
	// Bound weights size dots by area. Without them, roots and hubs get the larger fixed dot. The compact canvas uses a smaller weighted range.
	const radiusOf = (point: GraphPosition) => {
		if (!heaviest) return roots.has(point.node.id) || point.degree >= 4 ? 5 : 3.5;
		const weight = weights.get(point.node.id);
		return weight ? Math.round((3.5 + (size === "compact" ? 4 : 6) * Math.sqrt(weight / heaviest)) * 2) / 2 : 3.5;
	};
	const { width, height } = GRAPH_CANVAS[size !== "inline" ? size : data.nodes.length <= 8 ? "small" : data.nodes.length <= 24 ? "medium" : "inline"];
	let positions: Map<string, GraphPosition>;
	if (mode === "force") positions = layoutGraph(data).positions;
	else {
		const coordinates = mapped ?? (mode === "map" ? undefined : mode === "radial" ? radialPositions(data) : mode === "components" ? componentPositions(data) : layeredPositions(data));
		positions = new Map([...data.nodes].sort((a, b) => compare(a.id, b.id)).map(node => [node.id, {
			node, x: coordinates?.get(node.id)?.[0] ?? 0, y: coordinates?.get(node.id)?.[1] ?? 0, radius: 0, degree: 0, category: "",
		}]));
		for (const edge of data.edges) { positions.get(edge.from)!.degree++; positions.get(edge.to)!.degree++; }
	}
	const points = [...positions.values()];
	for (const point of points) point.radius = radiusOf(point);
	const pairs = new Map<string, number>();
	for (const link of links) { const pair = JSON.stringify([link.from, link.to]); pairs.set(pair, (pairs.get(pair) ?? 0) + 1); }
	// Leave a wider gutter above and to the right, where labels start. Large curves need more room.
	let inset = 20, headroom = 24;
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
	const frame = { left: inset, top: Math.max(inset, headroom), right: width - Math.max(inset, 24), bottom: height - inset };
	const { left, top, right, bottom } = frame;
	// Estimate the preview label: the symbol, else the file name, else the label, at most 30 characters of about 6.8 px.
	const labelWidth = size === "compact" ? undefined : (point: GraphPosition) => {
		const path = point.node.hit?.path ?? point.node.path;
		const name = point.node.hit?.symbol ?? (point.node.category === "file" && path ? path.split("/").at(-1)! : point.node.label);
		return Math.min(30, Array.from(name).length) * 6.8 + 4;
	};
	if (mode !== "map" || !familyPositions(data, points, families, frame, labelWidth)) {
		if (mode === "map") {
			const placed = points.filter(point => !isRemainder(point.node));
			const xs = placed.map(point => point.x), ys = placed.map(point => point.y);
			const rankX = rankBlend(xs), rankY = rankBlend(ys);
			const extentX = Math.max(...xs) - Math.min(...xs), extentY = Math.max(...ys) - Math.min(...ys);
			placed.forEach((point, index) => { point.x = rankX[index] * extentX; point.y = rankY[index] * extentY; });
			fitPoints(placed, frame);
			if (!families.length) {
				const { vectors } = graphEmbedding(data);
				const lexical = semanticFamilies(placed.map(point => point.node.id), vectors);
				const groupOf = new Map(lexical.flatMap(family => family.members.map(id => [id, family.members[0]] as const)));
				compactSettledGroups(placed, frame, point => groupOf.get(point.node.id) ?? point.node.id, false);
			}
			// The remainder starts in the lower right corner; separation moves it to free space.
			for (const point of points) if (isRemainder(point.node)) { point.x = right; point.y = bottom; }
		} else {
			fitPoints(points, frame);
			if (mode === "force") compactSettledGroups(points, frame, point => point.category, true);
		}
		// Keep the force positions when displayed dots already have enough space.
		if (mode !== "force" || size === "compact" ||
			points.some((p, a) => points.some((q, b) => b > a && Math.hypot(p.x - q.x, p.y - q.y) < dotGap(p, q)))) {
			separate(points, left, top, right, bottom, mode === "layers");
			if (!points.some(point => isRemainder(point.node))) fitPoints(points, frame, 1);
		}
	}
	const dots = points.map((point): NativeGraphDot => {
		const { x, y, radius } = point, weight = weights.get(point.node.id);
		return { node: point.node, x, y, radius, degree: point.degree, root: roots.has(point.node.id), ...(weight === undefined ? {} : { weight }) };
	});
	// Route tiers exist only when edge drawing weights are bound. An unweighted original edge counts once.
	const routeWeights = data.edges.some(edge => drawingWeight(edge) !== undefined)
		? links.map(link => link.indices.reduce((sum, index) => sum + (drawingWeight(data.edges[index]) ?? 1), 0)) : undefined;
	const heaviestRoute = Math.max(0, ...routeWeights ?? []);
	const routes = links.map((link, index): NativeGraphRoute => {
		// Route geometry uses final dot positions and rendered radii.
		const edge = edgeGeometry(links, index, positions)!;
		const controls: NativeGraphPoint[] = [edge.from];
		for (let at = 0; at < edge.control.length; at += 2) controls.push([edge.control[at], edge.control[at + 1]]);
		controls.push(edge.to);
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
		if (!routeWeights) return { link, points };
		const ratio = routeWeights[index] / heaviestRoute;
		return { link, points, tier: ratio >= .6 ? 4 : ratio >= .3 ? 3 : ratio >= .12 ? 2 : 1 };
	});
	const prepared = { width, height, dots, routes, territories: mode === "force" || mode === "map" ? territories(data, dots, width, height, size === "compact") : [] };
	if (!variants) { variants = new Map(); layouts.set(data, variants); }
	variants.set(key, prepared);
	return prepared;
}
