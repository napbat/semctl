// Deterministic force geometry for native graph surfaces and settled previews.
const compare = (a, b) => a < b ? -1 : a > b ? 1 : 0;
const hash = text => { let value = 2166136261; for (const char of text) value = Math.imul(value ^ char.charCodeAt(0), 16777619); return value >>> 0; };
const normalizePath = value => value.replace(/\\/g, "/").replace(/^\.\//, "").replace(/\/+$/, "");
export function category(node) {
	if ((node.category === "file" || node.category === "directory") && typeof node.path === "string") {
		const project = normalizePath(node.project ?? ""), path = normalizePath(node.path);
		const relative = project && (path === project || path.startsWith(project + "/")) ? path.slice(project.length).replace(/^\//, "") : path;
		const directory = node.category === "directory" ? relative : relative.slice(0, Math.max(0, relative.lastIndexOf("/")));
		const parts = directory.split("/").filter(Boolean), depth = parts[0] === "plugins" ? 3 : 2, branch = parts.slice(0, depth).join("/");
		return { key: `module:${JSON.stringify([project, branch])}`, label: branch || project || ".", type: "Directory" };
	}
	const project = node.project ?? node.hit?.project, path = node.path ?? node.hit?.path;
	if (project !== undefined && project !== null) return { key: `project:${project}`, label: project || ".", type: "Project" };
	if (path) { const normalized = path.replace(/\\/g, "/"), cut = normalized.lastIndexOf("/"), directory = cut < 0 ? "." : normalized.slice(0, cut) || "/"; return { key: `path:${directory}`, label: directory, type: "Directory" }; }
	const kind = node.category ?? node.hit?.kind;
	return kind ? { key: `kind:${kind}`, label: kind, type: "Kind" } : { key: "structure", label: "Graph structure", type: "" };
}
// Relation identity is independent of group color. Only known legacy labels are promoted.
export const relations = Object.freeze({
	containment: { label: "Containment", dash: [] },
	association: { label: "Project association", dash: [1, 4] },
	import: { label: "Import", dash: [6, 4] },
	reference: { label: "Resolved reference", dash: [] },
	generic: { label: "Other relation", dash: [] },
});
export function relationKind(edge) {
	if (edge.kind) return Object.hasOwn(relations, edge.kind) ? edge.kind : "generic";
	if (edge.label === "contains" || edge.label === "contains reported child") return "containment";
	if (edge.label === "within project root") return "association";
	return edge.label === "imports" ? "import" : "generic";
}
export function relationStyle(edge) { return relations[relationKind(edge)]; }
/** One route per exact endpoint/direction/kind; every original edge remains inspectable. */
export function representativeLinks(data) {
	const pairs = new Map();
	data.edges.forEach((edge, index) => {
		const kind = relationKind(edge), key = JSON.stringify([edge.from, edge.to, kind]);
		if (!pairs.has(key)) pairs.set(key, { ...edge, kind, count: 0, indices: [] });
		const pair = pairs.get(key); pair.count++; pair.indices.push(index);
	});
	return [...pairs.values()].sort((a, b) => compare(a.from, b.from) || compare(a.to, b.to) || compare(a.kind, b.kind)).map((pair, index) => ({ ...pair, index, symbol: pair.count === 1 ? pair.symbol : null, moniker: pair.count === 1 ? pair.moniker : null, label: pair.count === 1 ? pair.label : relations[pair.kind].label }));
}
function tree(points) {
	let minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity;
	for (const point of points) { minX = Math.min(minX, point.x); minY = Math.min(minY, point.y); maxX = Math.max(maxX, point.x); maxY = Math.max(maxY, point.y); }
	const size = Math.max(1, maxX - minX, maxY - minY) + 1;
	const cell = (x, y, size) => ({ x, y, size, points: [], children: null, mass: 0, cx: 0, cy: 0 });
	const root = cell(Number.isFinite(minX) ? minX - .5 : 0, Number.isFinite(minY) ? minY - .5 : 0, size);
	function insert(branch, point, depth) {
		if (!branch.children && (branch.points.length < 4 || depth === 24)) { branch.points.push(point); return; }
		if (!branch.children) {
			const half = branch.size / 2;
			branch.children = [cell(branch.x, branch.y, half), cell(branch.x + half, branch.y, half), cell(branch.x, branch.y + half, half), cell(branch.x + half, branch.y + half, half)];
			for (const old of branch.points) insert(branch.children[(old.x >= branch.x + half ? 1 : 0) + (old.y >= branch.y + half ? 2 : 0)], old, depth + 1);
			branch.points = [];
		}
		const half = branch.size / 2;
		insert(branch.children[(point.x >= branch.x + half ? 1 : 0) + (point.y >= branch.y + half ? 2 : 0)], point, depth + 1);
	}
	for (const point of points) insert(root, point, 0);
	function accumulate(branch) {
		if (branch.children) for (const child of branch.children) { accumulate(child); branch.mass += child.mass; branch.cx += child.cx * child.mass; branch.cy += child.cy * child.mass; }
		else for (const point of branch.points) { branch.mass++; branch.cx += point.x; branch.cy += point.y; }
		if (branch.mass) { branch.cx /= branch.mass; branch.cy /= branch.mass; }
	}
	accumulate(root); return root;
}
function groupAnchors(groups, data, categoryFor) {
	const byId = new Map(data.nodes.map(node => [node.id, categoryFor(node).key]));
	const neighbours = new Map(groups.map(group => [group.key, new Map()]));
	for (const edge of data.edges) {
		const from = byId.get(edge.from), to = byId.get(edge.to), kind = relationKind(edge);
		// Redundant project membership must not overwhelm dependency or hierarchy evidence.
		if (from === to || kind === "association" || !neighbours.has(from) || !neighbours.has(to)) continue;
		const weight = kind === "reference" ? 4 : kind === "import" ? 2 : 1;
		neighbours.get(from).set(to, (neighbours.get(from).get(to) ?? 0) + weight);
		neighbours.get(to).set(from, (neighbours.get(to).get(from) ?? 0) + weight);
	}
	const degrees = new Map([...neighbours].map(([key, values]) => [key, [...values.values()].reduce((sum, weight) => sum + weight, 0)]));
	const remaining = new Map(groups.map(group => [group.key, group])), order = [], frontier = new Map();
	while (remaining.size) {
		let next;
		for (const candidate of remaining.values()) {
			if (!next || (frontier.get(candidate.key) ?? 0) > (frontier.get(next.key) ?? 0) || ((frontier.get(candidate.key) ?? 0) === (frontier.get(next.key) ?? 0) && (degrees.get(candidate.key) > degrees.get(next.key) || (degrees.get(candidate.key) === degrees.get(next.key) && compare(candidate.key, next.key) < 0)))) next = candidate;
		}
		remaining.delete(next.key); order.push(next);
		for (const [key, weight] of neighbours.get(next.key)) frontier.set(key, (frontier.get(key) ?? 0) + weight);
	}
	const radius = group => Math.max(30, Math.sqrt(group.nodes.length) * 30);
	const targetWidth = Math.sqrt(groups.reduce((area, group) => area + (radius(group) * 2 + 70) ** 2, 0)) * 1.3;
	const anchors = new Map(); let x = 0, y = 0, rowHeight = 0;
	for (const group of order) {
		const r = radius(group), width = r * 2 + 70;
		if (x && x + width > targetWidth) { x = 0; y += rowHeight; rowHeight = 0; }
		anchors.set(group.key, { x: x + r + 35, y: y + r + 35, radius: r });
		x += width; rowHeight = Math.max(rowHeight, width);
	}
	// Bounded coarse relaxation: dependencies pull groups together, envelopes repel.
	// Large category sets keep the topology-first packing rather than doing quadratic work.
	if (groups.length <= 128) for (let pass = 0; pass < 64; pass++) {
		const moves = new Map(groups.map(group => [group.key, { x: 0, y: 0 }]));
		for (let a = 0; a < groups.length; a++) for (let b = a + 1; b < groups.length; b++) {
			const left = groups[a].key, right = groups[b].key, p = anchors.get(left), q = anchors.get(right);
			const dx = q.x - p.x, dy = q.y - p.y, distance = Math.hypot(dx, dy) || 1, wanted = p.radius + q.radius + 70;
			const weight = neighbours.get(left).get(right) ?? 0;
			const strength = weight / Math.max(1, Math.sqrt(degrees.get(left) * degrees.get(right)));
			const force = distance < wanted ? (distance - wanted) * .3 : (distance - wanted) * strength * .055;
			const mx = dx / distance * force, my = dy / distance * force;
			moves.get(left).x += mx; moves.get(left).y += my; moves.get(right).x -= mx; moves.get(right).y -= my;
		}
		for (const [key, move] of moves) { const anchor = anchors.get(key); anchor.x += Math.max(-12, Math.min(12, move.x)); anchor.y += Math.max(-12, Math.min(12, move.y)); }
	}
	let centerX = 0, centerY = 0, count = 0;
	for (const group of groups) { const anchor = anchors.get(group.key); centerX += anchor.x * group.nodes.length; centerY += anchor.y * group.nodes.length; count += group.nodes.length; }
	for (const anchor of anchors.values()) { anchor.x -= centerX / Math.max(1, count); anchor.y -= centerY / Math.max(1, count); }
	return { anchors, order };
}
export function createSimulation(data, previousPositions, categoryFor = category) {
	const ordered = [...data.nodes].sort((a, b) => compare(a.id, b.id)), roots = new Set(data.roots), groups = new Map();
	for (const node of ordered) { const info = categoryFor(node); if (!groups.has(info.key)) groups.set(info.key, { ...info, nodes: [] }); groups.get(info.key).nodes.push(node); }
	const sorted = [...groups.values()].sort((a, b) => compare(a.key, b.key)), positions = new Map();
	const modular = ordered.some(node => node.category && typeof node.path === "string");
	const { anchors, order: categories } = groupAnchors(sorted, data, categoryFor);
	for (const group of categories) {
		const anchor = anchors.get(group.key);
		group.nodes.forEach((node, at) => {
			const phase = (hash(node.id) % 1000) / 1000 * .3, angle = at * 2.399963229728653 + phase, distance = 7 + Math.sqrt(at) * 13, old = previousPositions?.get(node.id);
			positions.set(node.id, { node, category: group.key, x: old?.x ?? anchor.x + Math.cos(angle) * distance, y: old?.y ?? anchor.y + Math.sin(angle) * distance, vx: 0, vy: 0, radius: roots.has(node.id) ? 4 : 2.2, degree: 0, forceDegree: 0 });
		});
	}
	const links = data.edges.map((edge, index) => ({ ...edge, index, source: positions.get(edge.from), target: positions.get(edge.to) }));
	for (const link of links) {
		link.source.degree++; link.target.degree++;
		if (!modular || (link.kind !== "association" && link.label !== "within project root")) { link.source.forceDegree++; link.target.forceDegree++; }
	}
	const points = [...positions.values()];
	for (const point of points) point.radius = Math.max(point.radius, Math.min(4.5, 2 + Math.log2(point.degree + 1) * .35));
	let spatial = tree(points), alpha = 1;
	const stats = { visits: 0, ticks: 0 };
	function tick() {
		if (alpha < .005) return false;
		stats.visits = 0; stats.ticks++;
		for (const point of points) {
			function repel(branch) {
				stats.visits++;
				if (!branch.mass) return;
				let dx = point.x - branch.cx, dy = point.y - branch.cy, square = dx * dx + dy * dy;
				const contains = point.x >= branch.x && point.x <= branch.x + branch.size && point.y >= branch.y && point.y <= branch.y + branch.size;
				if (branch.children && (contains || branch.size * branch.size > square * .72)) { for (const child of branch.children) repel(child); return; }
				if (!branch.children && branch.points.length > 4) {
					if (square < .01) { const angle = hash(point.node.id) * .01; dx = Math.cos(angle) * .1; dy = Math.sin(angle) * .1; square = .02; }
					const force = 32 * alpha * (branch.mass - (contains ? 1 : 0)) / Math.max(20, square); point.vx += dx * force; point.vy += dy * force; return;
				}
				if (!branch.children) {
					for (const other of branch.points) if (other !== point) {
						dx = point.x - other.x; dy = point.y - other.y;
						if (dx === 0 && dy === 0) { const angle = hash(point.node.id) * .01; dx = Math.cos(angle) * .1; dy = Math.sin(angle) * .1; }
						const force = 32 * alpha / Math.max(20, dx * dx + dy * dy); point.vx += dx * force; point.vy += dy * force;
					}
				} else { const force = 32 * alpha * branch.mass / Math.max(20, square); point.vx += dx * force; point.vy += dy * force; }
			}
			repel(spatial);
			const anchor = anchors.get(point.category);
			point.vx += (anchor.x - point.x) * (modular ? .045 : .002) * alpha - point.x * (modular ? 0 : .0004) * alpha;
			point.vy += (anchor.y - point.y) * (modular ? .045 : .002) * alpha - point.y * (modular ? 0 : .0004) * alpha;
		}
		for (const { source: a, target: b, label, kind } of links) if (a !== b && (!modular || (kind !== "association" && label !== "within project root"))) {
			const dx = b.x - a.x, dy = b.y - a.y, distance = Math.hypot(dx, dy) || 1, local = a.category === b.category, wanted = local ? 42 : modular ? 100 : 72;
			const reference = kind === "import" || kind === "reference" || label === "imports";
			const strength = !modular ? .018 : local ? reference ? .008 : .012 : reference ? .0004 : .0012;
			const force = (distance - wanted) / distance * strength * alpha, balance = Math.sqrt(Math.max(1, a.forceDegree, b.forceDegree));
			a.vx += dx * force / balance; a.vy += dy * force / balance; b.vx -= dx * force / balance; b.vy -= dy * force / balance;
		}
		for (const point of points) {
			point.vx = Math.max(-12, Math.min(12, point.vx * .72)); point.vy = Math.max(-12, Math.min(12, point.vy * .72));
			point.x += point.vx; point.y += point.vy;
		}
		alpha *= .94; spatial = tree(points); return alpha >= .005;
	}
	function bounds(subset = points) {
		let left = Infinity, right = -Infinity, top = Infinity, bottom = -Infinity;
		for (const point of subset) { left = Math.min(left, point.x - point.radius); right = Math.max(right, point.x + point.radius); top = Math.min(top, point.y - point.radius); bottom = Math.max(bottom, point.y + point.radius); }
		return Number.isFinite(left) ? { x: left, y: top, width: Math.max(1, right - left), height: Math.max(1, bottom - top) } : { x: 0, y: 0, width: 1, height: 1 };
	}
	return { positions, links, categories, stats, get alpha() { return alpha; }, tick, bounds };
}
export function layoutGraph(data, availableWidth = 1000) {
	const simulation = createSimulation(data);
	while (simulation.tick()) { /* Finite cooling; no wall-clock dependent stopping. */ }
	const bounds = simulation.bounds();
	return { positions: simulation.positions, bounds, width: bounds.width, height: bounds.height };
}
const edgePlans = new WeakMap();
export function edgeGeometry(edges, index, positions) {
	let cached = edgePlans.get(positions);
	if (!cached || cached.edges !== edges) {
		const groups = new Map();
		edges.forEach((edge, at) => { const key = JSON.stringify([edge.from, edge.to]); if (!groups.has(key)) groups.set(key, []); groups.get(key).push(at); });
		const lanes = new Map();
		for (const indices of groups.values()) indices.forEach((at, lane) => { const edge = edges[at], reverse = groups.has(JSON.stringify([edge.to, edge.from])); lanes.set(at, { lane, count: indices.length, reverse }); });
		cached = { edges, lanes }; edgePlans.set(positions, cached);
	}
	const edge = edges[index], a = positions.get(edge.from), b = positions.get(edge.to);
	if (!a || !b) return null;
	const { lane, count, reverse } = cached.lanes.get(index);
	if (a === b) {
		const size = a.radius + 13 + lane * 5, from = [a.x - a.radius * .7, a.y - a.radius * .7], to = [a.x + a.radius * .7, a.y - a.radius * .7], control = [a.x - size, a.y - size * 2, a.x + size, a.y - size * 2];
		return { from, to, control, self: true, x: a.x, y: a.y - size * 1.5 };
	}
	const dx = b.x - a.x, dy = b.y - a.y, length = Math.hypot(dx, dy) || 1, offset = (lane - (count - 1) / 2) * 10 + (reverse ? 15 : 0);
	const cx = (a.x + b.x) / 2 - dy / length * offset, cy = (a.y + b.y) / 2 + dx / length * offset;
	const clip = (point, x, y) => { const length = Math.hypot(x - point.x, y - point.y) || 1; return [point.x + (x - point.x) / length * (point.radius + 1), point.y + (y - point.y) / length * (point.radius + 1)]; };
	const from = clip(a, cx, cy), to = clip(b, cx, cy);
	return { from, to, control: [cx, cy], self: false, x: (from[0] + 2 * cx + to[0]) / 4, y: (from[1] + 2 * cy + to[1]) / 4 };
}
export function exactActivity(node, targets, codebaseId) {
	if (node.hit?.codebaseId && node.hit.codebaseId !== codebaseId) return false;
	return [node.id, node.label, node.path, node.hit?.symbol, node.hit?.qualifiedName, node.hit?.path].some(value => typeof value === "string" && targets.has(value));
}
