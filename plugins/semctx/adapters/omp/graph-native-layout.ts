import type { GraphData } from "./graph";
import { DEFAULT_GRAPH_APPEARANCE, type GraphAppearance } from "./graph-appearance";
import { bindTermVector, centroid, cosine, graphEmbedding, semanticFamilies, semanticMap, similarityMap } from "./graph-data/embedding.js";
import { bindDrawingWeight, drawingWeight, edgeGeometry, layoutGraph, relationKind, representativeLinks, sharedConceptFamilies, type GraphPosition, type GraphRelationKind, type SharedConceptFamily } from "./graph-data/layout.js";
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
 * Family around its member dots: a doc concept family, or else a lexical identifier family.
 * It is not the server's embedding and never moves a dot.
 */
export interface NativeGraphTerritory {
	key: string;
	kind: "family" | "lexical";
	label: string;
	/** Hover text for the label chip, already compact. */
	title?: string;
	/** Shared identifier terms of a lexical family; doc concept families have none. */
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
interface NativeCurve {
	from: readonly number[];
	to: readonly number[];
	control: number[];
	self: boolean;
}
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
 * Classical MDS on the distance 1 − S places the nodes; the principal axis is horizontal.
 * The heaviest node, then the first ID, takes the left and top side of each axis.
 * Undefined without term or family evidence, or when all pairs are equally similar.
 */
function conceptMap(data: GraphData): ReadonlyMap<string, NativeGraphPoint> | undefined {
	const cached = conceptMaps.get(data);
	if (cached !== undefined) return cached ?? undefined;
	const ids = [...new Set(data.nodes.map(node => node.id))].sort(compare), n = ids.length, at = new Map(ids.map((id, index) => [id, index]));
	const { vectors, informative } = graphEmbedding(data), shared = sharedConceptFamilies(data.nodes);
	let result: Map<string, NativeGraphPoint> | undefined;
	if (n >= 2 && n <= 512 && (informative || shared.length)) {
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
				const value = .6 * cosine(vectors.get(ids[i]), vectors.get(ids[j])) + (strongest ? .3 * (affinity.get(i * n + j) ?? 0) / strongest : 0) +
					(family.has(ids[i]) && family.get(ids[i]) === family.get(ids[j]) ? .1 : 0);
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

/** Resolve visual collisions without feeding transient state back into the force simulation. */
function separate(points: GraphPosition[], left: number, top: number, right: number, bottom: number, layered: boolean): void {
	const width = right - left, height = bottom - top;
	const base = Math.min(26, Math.sqrt(width * height / Math.max(1, points.length) / 1.5));
	// The gap grows with both radii so that large weighted dots never touch.
	const gap = (p: GraphPosition, q: GraphPosition) => Math.max(base, p.radius + q.radius + 10);
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
	const columns = Math.max(1, Math.floor(width / pitch) + 1), rows = Math.max(1, Math.ceil(points.length / columns));
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
 * A family is one outer-map island, not a weak pairwise hint. Its centroid and the ungrouped nodes form the outer MDS.
 * Local MDS keeps the family's terms readable. Packing reserves each family's final territory before any singleton is placed.
 */
function familyPositions(data: GraphData, points: GraphPosition[], families: readonly SharedConceptFamily[], left: number, top: number, right: number, bottom: number): boolean {
	if (!families.length) return false;
	const { vectors } = graphEmbedding(data), byId = new Map(points.map(point => [point.node.id, point]));
	const grouped = new Set(families.flatMap(family => family.members));
	const groups = [
		...families.map(family => ({ key: JSON.stringify(["family", family.key]), members: family.members.map(id => byId.get(id)!) })),
		...points.filter(point => !grouped.has(point.node.id)).map(point => ({ key: JSON.stringify(["node", point.node.id]), members: [point] })),
	].sort((a, b) => compare(a.key, b.key));
	const groupOf = new Map(groups.flatMap(group => group.members.map(point => [point.node.id, group.key] as const)));
	const nodes = groups.map(group => {
		const node: GraphData["nodes"][number] = { id: group.key, label: "", category: "group", hit: null, detail: null };
		bindTermVector(node, centroid(group.members.map(point => vectors.get(point.node.id))));
		bindDrawingWeight(node, group.members.reduce((sum, point) => sum + (drawingWeight(point.node) ?? 1), 0));
		return node;
	});
	const edges = data.edges.flatMap(edge => {
		const from = groupOf.get(edge.from)!, to = groupOf.get(edge.to)!;
		if (from === to) return [];
		const link = { ...edge, from, to };
		bindDrawingWeight(link, drawingWeight(edge) ?? 1);
		return [link];
	});
	const mapped = conceptMap({ ...data, nodes, edges, roots: [] });
	// The heaviest original node fixes the outer orientation, even when it belongs to a larger family.
	const rank = [...points].sort((a, b) => (drawingWeight(b.node) ?? 0) - (drawingWeight(a.node) ?? 0) || compare(a.node.id, b.node.id)).map(point => groupOf.get(point.node.id)!);
	const outer = mapped && orientMap(mapped, rank);
	const islands = groups.map((group, groupIndex) => {
		const members = group.members;
		if (members.length > 1) {
			const local = semanticMap(members.map(point => point.node.id), vectors), extent = Math.max(24, Math.sqrt(members.length) * 16);
			const spread = local ? Math.max(...[...local.values()].map(([x, y]) => Math.hypot(x, y))) : 1;
			members.forEach((point, index) => {
				const position = local?.get(point.node.id), angle = index * Math.PI * 2 / members.length;
				// A pair reads down the page, leaving room for both node labels beside its dots.
				point.x = position ? position[members.length === 2 ? 1 : 0] / spread * extent : Math.cos(angle) * extent;
				point.y = position ? position[members.length === 2 ? 0 : 1] / spread * extent : Math.sin(angle) * extent;
			});
			separate(members, -extent, -extent, extent, extent, false);
		} else { members[0].x = 0; members[0].y = 0; }
		const envelope = memberEllipse(members, false);
		// Keep the territory border and label chip clear of neighbouring islands.
		const rx = members.length > 1 ? envelope.rx + 10 : members[0].radius + 13, ry = members.length > 1 ? envelope.ry + 10 : members[0].radius + 13;
		for (const point of members) { point.x -= envelope.x; point.y -= envelope.y; }
		const seed = outer?.get(group.key);
		return { ...group, rx, ry, seed: seed ?? [groupIndex, 0], x: 0, y: 0 };
	});
	const xs = islands.map(island => island.seed[0]), ys = islands.map(island => island.seed[1]);
	const minX = Math.min(...xs), maxX = Math.max(...xs), minY = Math.min(...ys), maxY = Math.max(...ys);
	// Largest families go first. Every candidate is a nearest free box edge, so small unrelated groups cannot split a family.
	const placed: typeof islands = [];
	for (const island of [...islands].sort((a, b) => b.members.length - a.members.length || b.rx * b.ry - a.rx * a.ry || compare(a.key, b.key))) {
		const x = minX === maxX ? (left + right) / 2 : left + (island.seed[0] - minX) / (maxX - minX) * (right - left);
		const y = minY === maxY ? (top + bottom) / 2 : top + (island.seed[1] - minY) / (maxY - minY) * (bottom - top);
		const clampX = (value: number) => Math.max(left + island.rx, Math.min(right - island.rx, value));
		const clampY = (value: number) => Math.max(top + island.ry, Math.min(bottom - island.ry, value));
		const candidatesX = [...new Set([clampX(x), left + island.rx, right - island.rx, ...placed.flatMap(other => [other.x - other.rx - island.rx - 8, other.x + other.rx + island.rx + 8])])]
			.filter(value => value >= left + island.rx && value <= right - island.rx).sort((a, b) => Math.abs(a - x) - Math.abs(b - x) || a - b);
		const candidatesY = [...new Set([clampY(y), top + island.ry, bottom - island.ry, ...placed.flatMap(other => [other.y - other.ry - island.ry - 8, other.y + other.ry + island.ry + 8])])]
			.filter(value => value >= top + island.ry && value <= bottom - island.ry).sort((a, b) => Math.abs(a - y) - Math.abs(b - y) || a - b);
		let best = Infinity;
		island.x = clampX(x); island.y = clampY(y);
		for (const cx of candidatesX) {
			if ((cx - x) ** 2 >= best) break;
			for (const cy of candidatesY) {
				const score = (cx - x) ** 2 + (cy - y) ** 2;
				if (score >= best) break;
				if (placed.some(other => Math.abs(cx - other.x) < island.rx + other.rx + 7.9 && Math.abs(cy - other.y) < island.ry + other.ry + 7.9)) continue;
				best = score; island.x = cx; island.y = cy;
			}
		}
		placed.push(island);
		for (const point of island.members) { point.x += island.x; point.y += island.y; }
	}
	return true;
}

/** Doc concept families, or else lexical identifier families, around settled dots. They never feed back into dot positions. */
function territories(data: GraphData, dots: NativeGraphDot[], width: number, height: number, compact: boolean): NativeGraphTerritory[] {
	if (dots.length < 4) return [];
	const { vectors, informative } = graphEmbedding(data), byId = new Map(dots.map(dot => [dot.node.id, dot])), concepts = sharedConceptFamilies(data.nodes);
	type Family = Pick<NativeGraphTerritory, "key" | "kind" | "label" | "title" | "terms" | "members" | "cohesion">;
	// A doc concept family names a concept from the docs; lexical families only show shared identifier words.
	const families: Family[] = concepts.length ? concepts.slice(0, 6).map((family): Family => {
		let total = 0, pairs = 0;
		for (let a = 0; a < family.members.length; a++) for (let b = a + 1; b < family.members.length; b++) {
			total += cosine(vectors.get(family.members[a]), vectors.get(family.members[b])); pairs++;
		}
		const folder = family.key.startsWith("family:") ? family.key.slice("family:".length) : "";
		return { key: family.key, kind: "family", label: family.label, terms: [], members: family.members, cohesion: total / pairs,
			title: compactTooltip(`${family.label} · concept family`, `${family.members.length} concepts from docs in ${folder || "the repository root"}`) };
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
	const families = choice === "map" ? sharedConceptFamilies(data.nodes) : [];
	const mapped = choice === "map" && !families.length ? conceptMap(data) : undefined, mode = choice === "map" && !mapped && !families.length ? "force" : choice;
	const links = representativeLinks(data), roots = new Set(data.roots);
	const weights = new Map(data.nodes.map(node => [node.id, drawingWeight(node)]));
	const heaviest = Math.max(0, ...[...weights.values()].map(weight => weight ?? 0));
	// Bound weights size dots by area. Without them, roots and hubs get the larger fixed dot.
	const radiusOf = (point: GraphPosition) => {
		if (!heaviest) return roots.has(point.node.id) || point.degree >= 4 ? 5 : 3.5;
		const weight = weights.get(point.node.id);
		return weight ? Math.round((3.5 + 6 * Math.sqrt(weight / heaviest)) * 2) / 2 : 3.5;
	};
	const width = size === "compact" ? 280 : size === "overview" ? 760 : 560;
	const height = size === "compact" ? 176 : size === "overview" ? 320 : data.nodes.length <= 8 ? 156 : data.nodes.length <= 24 ? 216 : 264;
	let positions: Map<string, GraphPosition>, project: (x: number, y: number) => NativeGraphPoint;
	let geometries: NativeCurve[];
	if (mode === "force" && size !== "compact") {
		// Preserve established inline/tool-response geometry when no new mode is requested.
		const layout = layoutGraph(data);
		positions = layout.positions;
		geometries = links.map((_, index) => edgeGeometry(links, index, positions)!);
		let left = layout.bounds.x, top = layout.bounds.y, right = left + layout.width, bottom = top + layout.height;
		for (const edge of geometries) for (let at = 0; at < edge.control.length; at += 2) {
			left = Math.min(left, edge.control[at]); right = Math.max(right, edge.control[at]);
			top = Math.min(top, edge.control[at + 1]); bottom = Math.max(bottom, edge.control[at + 1]);
		}
		// Fit each axis to the wide canvas, but limit the stretch so that the topology reads the same.
		let sx = Math.min(2.4, (width - 64) / Math.max(1, right - left)), sy = Math.min(2.4, (height - 48) / Math.max(1, bottom - top));
		sx = Math.min(sx, sy * 1.45); sy = Math.min(sy, sx * 1.45);
		project = (x, y) => [(x - (left + right) / 2) * sx + width / 2, (y - (top + bottom) / 2) * sy + height / 2];
	} else {
		if (mode === "force") positions = new Map([...layoutGraph(data).positions].map(([id, point]) => [id, { ...point }]));
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
		// Each axis fills the canvas. The concept map places half by value and half by rank, so one distant cluster cannot squeeze the rest.
		const fill = (values: number[], from: number, to: number) => {
			const low = Math.min(...values), high = Math.max(...values);
			if (high === low) return values.map(() => (from + to) / 2);
			const first = new Map<number, number>(), last = new Map<number, number>();
			if (mode === "map") [...values].sort((a, b) => a - b).forEach((value, index) => { if (!first.has(value)) first.set(value, index); last.set(value, index); });
			return values.map(value => {
				const linear = (value - low) / (high - low);
				return from + (mode === "map" ? (linear + (first.get(value)! + last.get(value)!) / 2 / (values.length - 1)) / 2 : linear) * (to - from);
			});
		};
		if (mode !== "map" || !familyPositions(data, points, families, left, top, right, bottom)) {
			const xs = fill(points.map(point => point.x), left, right), ys = fill(points.map(point => point.y), top, bottom);
			points.forEach((point, index) => { point.x = xs[index]; point.y = ys[index]; });
		}
		separate(points, left, top, right, bottom, mode === "layers");
		geometries = links.map((_, index) => edgeGeometry(links, index, positions)!);
		project = (x, y) => [x, y];
	}
	const dots = [...positions.values()].map((point): NativeGraphDot => {
		const [x, y] = project(point.x, point.y), weight = weights.get(point.node.id);
		return { node: point.node, x, y, radius: radiusOf(point), degree: point.degree, root: roots.has(point.node.id), ...(weight === undefined ? {} : { weight }) };
	});
	const byId = new Map(dots.map(dot => [dot.node.id, dot]));
	// Route tiers exist only when edge drawing weights are bound. An unweighted original edge counts once.
	const routeWeights = data.edges.some(edge => drawingWeight(edge) !== undefined)
		? links.map(link => link.indices.reduce((sum, index) => sum + (drawingWeight(data.edges[index]) ?? 1), 0)) : undefined;
	const heaviestRoute = Math.max(0, ...routeWeights ?? []);
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
		if (!routeWeights) return { link: links[index], points };
		const ratio = routeWeights[index] / heaviestRoute;
		return { link: links[index], points, tier: ratio >= .6 ? 4 : ratio >= .3 ? 3 : ratio >= .12 ? 2 : 1 };
	});
	const prepared = { width, height, dots, routes, territories: mode === "force" || mode === "map" ? territories(data, dots, width, height, size === "compact") : [] };
	if (!variants) { variants = new Map(); layouts.set(data, variants); }
	variants.set(key, prepared);
	return prepared;
}
