// Deterministic lexical identifier vectors for drawing placement only. Terms come from supplied paths, labels and resolved symbol names.
// These vectors are not the server's semantic embeddings. Near vectors share identifier words; they do not prove related behavior.
import { normalizeConceptPath } from "./palette.js";
const compare = (a, b) => a < b ? -1 : a > b ? 1 : 0;
const capacity = 48, ignored = new Set(["src", "lib", "mod", "index", "main", "rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "go", "cs", "cpp", "cc", "h", "hpp", "py", "d", "the", "and", "of", "to", "for"]);
const splits = new Map(), bound = new WeakMap(), embeddings = new WeakMap();
function split(text) {
	let terms = splits.get(text); if (terms) return terms;
	const unique = new Set();
	// Letter runs only: digits and punctuation are boundaries, so pure digit tokens never become terms.
	for (const word of text.replace(/(\p{Ll})(\p{Lu})/gu, "$1 $2").replace(/(\p{Lu}+)(\p{Lu}\p{Ll})/gu, "$1 $2").toLowerCase().split(/[^\p{L}]+/u)) {
		const term = word.length > 4 && word.endsWith("s") && !word.endsWith("ss") ? word.slice(0, -1) : word;
		if (term.length > 1 && !ignored.has(term)) unique.add(term);
	}
	if (splits.size >= 4096) splits.delete(splits.keys().next().value);
	terms = Object.freeze([...unique]); splits.set(text, terms); return terms;
}
export const identifierTerms = text => typeof text === "string" ? [...split(text)] : [];
export function bindTermVector(node, vector) { if (vector instanceof Map) bound.set(node, vector); }
function normalized(weights) {
	const entries = [...weights].filter(([, weight]) => weight > 0).sort((a, b) => b[1] - a[1] || compare(a[0], b[0])).slice(0, capacity);
	const norm = Math.sqrt(entries.reduce((sum, [, weight]) => sum + weight * weight, 0));
	return new Map(entries.map(([term, weight]) => [term, weight / norm]));
}
function add(terms, text, weight) { if (typeof text === "string") for (const term of split(text)) terms.set(term, (terms.get(term) ?? 0) + weight); }
function sources(node) {
	const terms = new Map(), path = normalizeConceptPath(node.hit?.path ?? node.path), parts = path?.split("/") ?? [], last = parts.pop();
	if (node.category === "directory" || node.category === "project" || node.category === "group") { add(terms, last, 1); for (const part of parts) add(terms, part, .3); }
	else if (last) {
		const cut = last.lastIndexOf("."); add(terms, cut > 0 ? last.slice(0, cut) : last, 1);
		parts.reverse().slice(0, 3).forEach((part, index) => add(terms, part, index ? .2 : .45));
	}
	add(terms, node.hit?.symbol, 1); add(terms, node.hit?.qualifiedName, .4);
	if (!path && !node.hit) add(terms, node.label, .8);
	return terms;
}
const structural = edge => edge.kind !== "association" && edge.label !== "within project root" && edge.from !== edge.to;
// One step from pre-diffusion vectors. Bound aggregates are absent from `own`, so they neither receive nor give terms.
function diffuse(own, edges) {
	const neighbours = new Map(), link = (a, b) => { if (!neighbours.has(a)) neighbours.set(a, new Set()); neighbours.get(a).add(b); };
	for (const edge of edges) if (structural(edge) && own.has(edge.from) && own.has(edge.to)) { link(edge.from, edge.to); link(edge.to, edge.from); }
	return new Map([...own].map(([id, vector]) => {
		const around = neighbours.get(id); if (!around) return [id, vector];
		const sum = new Map(vector), share = .35 / around.size;
		for (const other of around) for (const [term, weight] of own.get(other)) sum.set(term, (sum.get(term) ?? 0) + share * weight);
		return [id, normalized(sum)];
	}));
}
// Informative vectors share at least one term and are not all the same vector.
function discriminates(vectors) {
	const seen = new Set(); let first, shared = false, distinct = false;
	for (const vector of vectors) {
		if (!vector?.size) continue;
		if (!first) first = vector; else if (!distinct && cosine(first, vector) < 1 - 1e-9) distinct = true;
		if (!shared) for (const [term, weight] of vector) if (weight > 0) { if (seen.has(term)) { shared = true; break; } seen.add(term); }
		if (shared && distinct) return true;
	}
	return false;
}
export function graphEmbedding(data) {
	const cached = embeddings.get(data); if (cached) return cached;
	const nodes = [...data.nodes].sort((a, b) => compare(a.id, b.id)), terms = new Map(), frequency = new Map();
	for (const node of nodes) if (!bound.has(node)) terms.set(node.id, sources(node));
	const edges = [...data.edges].sort((a, b) => compare(a.from, b.from) || compare(a.to, b.to) || compare(a.kind ?? "", b.kind ?? "") || compare(a.symbol ?? "", b.symbol ?? ""));
	for (const edge of edges) if (edge.kind === "reference" && typeof edge.symbol === "string") { if (terms.has(edge.to)) add(terms.get(edge.to), edge.symbol, .9); if (terms.has(edge.from)) add(terms.get(edge.from), edge.symbol, .5); }
	for (const weights of terms.values()) for (const term of weights.keys()) frequency.set(term, (frequency.get(term) ?? 0) + 1);
	const idf = term => 1 + Math.log((1 + terms.size) / (1 + frequency.get(term)));
	const own = new Map([...terms].map(([id, weights]) => [id, normalized([...weights].map(([term, weight]) => [term, Math.log1p(weight) * idf(term)]))]));
	const diffused = nodes.length <= 4096 ? diffuse(own, edges) : own, vectors = new Map(nodes.map(node => [node.id, bound.get(node) ?? diffused.get(node.id)]));
	const result = { vectors, informative: discriminates(vectors.values()) };
	embeddings.set(data, result); return result;
}
export function cosine(a, b) {
	if (!a?.size || !b?.size) return 0;
	const small = a.size <= b.size ? a : b, large = small === a ? b : a; let sum = 0;
	for (const [term, weight] of small) { const other = large.get(term); if (other !== undefined) sum += weight * other; }
	return Math.max(-1, Math.min(1, sum));
}
export function centroid(vectors) {
	const sum = new Map();
	for (const vector of vectors) if (vector) for (const [term, weight] of vector) sum.set(term, (sum.get(term) ?? 0) + weight);
	return normalized(sum);
}
const unique = ids => [...new Set(ids)].sort(compare);
function similarities(ids, vectors) {
	const list = ids.map(id => vectors.get(id)), matrix = ids.map(() => new Float64Array(ids.length));
	for (let i = 0; i < ids.length; i++) for (let j = i; j < ids.length; j++) matrix[i][j] = matrix[j][i] = cosine(list[i], list[j]);
	return matrix;
}
const dot = (a, b) => { let sum = 0; for (let i = 0; i < a.length; i++) sum += a[i] * b[i]; return sum; };
const multiply = (matrix, vector) => Float64Array.from(matrix, row => dot(row, vector));
const unit = vector => { const norm = Math.sqrt(dot(vector, vector)); return vector.map(value => norm > 1e-12 ? value / norm : 0); };
function orthonormal([first, second]) { const a = unit(first), projection = dot(second, a); return [a, unit(second.map((value, i) => value - projection * a[i]))]; }
// Eigenvector signs are arbitrary. Later sorted IDs take the positive side; the largest coordinate decides a symmetric axis.
function orientation(values) {
	const middle = (values.length - 1) / 2; let sum = 0, extreme = 0;
	values.forEach((value, i) => { sum += value * (i - middle); if (Math.abs(value) > Math.abs(extreme)) extreme = value; });
	return Math.abs(sum) > 1e-12 ? Math.sign(sum) : Math.sign(extreme) || 1;
}
// Subspace iteration finds the two eigenvalues of largest magnitude. A shift by the most negative one keeps only the largest values.
// J·S·J maps every vector to a centred one. The shifted run stays centred, because the shift would also lift the constant vector.
// The shift moves the eigenvalue ratios nearer to 1, so the shifted run takes more steps.
function principalAxes(centred, shift) {
	const n = centred.length;
	const centre = vector => { const mean = vector.reduce((sum, value) => sum + value, 0) / n; return vector.map(value => value - mean); };
	const apply = shift ? axis => centre(multiply(centred, axis).map((value, i) => value + shift * axis[i])) : axis => multiply(centred, axis);
	let axes = orthonormal([Math.cos, Math.sin].map(wave => Float64Array.from({ length: n }, (_, i) => wave(i * 2.399963))).map(axis => shift ? centre(axis) : axis));
	for (let step = 0; step < (shift ? 240 : 80); step++) axes = orthonormal(axes.map(apply));
	return axes;
}
/** Classical MDS: the eigenvectors of J·S·J for the two largest eigenvalues, so S need not be positive semidefinite. The squared map distance follows S(a, a) + S(b, b) − 2·S(a, b). RMS radius 1 around the origin. */
export function similarityMap(ids, similarity) {
	const sorted = unique(ids), n = sorted.length;
	if (n < 2 || n > 512) return undefined;
	const gram = sorted.map(() => new Float64Array(n));
	for (let i = 0; i < n; i++) for (let j = i; j < n; j++) gram[i][j] = gram[j][i] = similarity(sorted[i], sorted[j]);
	if (!gram.some((row, i) => row.some((value, j) => j !== i && value > 0))) return undefined;
	if (n === 2) return (gram[0][0] + gram[1][1] - 2 * gram[0][1]) / 2 < 1e-6 ? undefined : new Map([[sorted[0], [-1, 0]], [sorted[1], [1, 0]]]);
	const means = gram.map(row => row.reduce((sum, value) => sum + value, 0) / n), mean = means.reduce((sum, value) => sum + value, 0) / n;
	const centred = gram.map((row, i) => row.map((value, j) => value - means[i] - means[j] + mean));
	let axes = principalAxes(centred, 0), values = axes.map(axis => dot(axis, multiply(centred, axis)));
	if (Math.min(...values) < -1e-9) { axes = principalAxes(centred, -Math.min(...values)); values = axes.map(axis => dot(axis, multiply(centred, axis))); }
	values = values.map(value => Math.max(0, value));
	if (values[0] < 1e-6) return undefined;
	const columns = axes.map((axis, index) => {
		const scaled = axis.map(value => value * Math.sqrt(values[index])), centre = scaled.reduce((sum, value) => sum + value, 0) / n, shifted = scaled.map(value => value - centre), sign = orientation(shifted);
		return shifted.map(value => value * sign);
	});
	const radius = Math.sqrt(columns.reduce((sum, column) => sum + dot(column, column), 0) / n);
	return new Map(sorted.map((id, i) => [id, [columns[0][i] / radius, columns[1][i] / radius]]));
}
/** Classical MDS on chord distance, with the cosine matrix as S. */
export function semanticMap(ids, vectors) {
	return similarityMap(ids, (a, b) => cosine(vectors.get(a), vectors.get(b)));
}
export function semanticNeighbours(ids, vectors, { k = 3, minimum = .2 } = {}) {
	const sorted = unique(ids), n = sorted.length; if (n > 512) return [];
	const matrix = similarities(sorted, vectors), pairs = new Set();
	for (let i = 0; i < n; i++) {
		const candidates = [];
		for (let j = 0; j < n; j++) if (j !== i && matrix[i][j] > 0 && matrix[i][j] >= minimum) candidates.push(j);
		for (const j of candidates.sort((a, b) => matrix[i][b] - matrix[i][a] || a - b).slice(0, Math.max(0, k))) pairs.add(Math.min(i, j) * n + Math.max(i, j));
	}
	return [...pairs].sort((a, b) => a - b).map(pair => { const a = Math.floor(pair / n), b = pair % n; return { a: sorted[a], b: sorted[b], similarity: matrix[a][b] }; });
}
/** Average-linkage clusters on cosine. A slot index is always the smallest member index, so ties merge in sorted ID order. */
export function semanticFamilies(ids, vectors, { limit = 6, threshold = .22 } = {}) {
	const supplied = unique(ids); if (supplied.length > 128) return [];
	const sorted = supplied.filter(id => vectors.get(id)?.size), matrix = similarities(sorted, vectors), linkage = matrix.map(row => Float64Array.from(row)), clusters = sorted.map((_, i) => [i]);
	for (;;) {
		let best = -Infinity, left = -1, right = -1;
		for (let i = 0; i < clusters.length; i++) if (clusters[i]) for (let j = i + 1; j < clusters.length; j++) if (clusters[j] && linkage[i][j] > best) { best = linkage[i][j]; left = i; right = j; }
		if (left < 0 || best < threshold) break;
		const a = clusters[left].length, b = clusters[right].length;
		for (let other = 0; other < clusters.length; other++) if (clusters[other] && other !== left && other !== right) linkage[left][other] = linkage[other][left] = (a * linkage[left][other] + b * linkage[right][other]) / (a + b);
		clusters[left] = [...clusters[left], ...clusters[right]].sort((x, y) => x - y); clusters[right] = null;
	}
	const families = clusters.filter(members => members?.length > 1).map(members => {
		const list = members.map(i => vectors.get(sorted[i])), share = term => list.filter(vector => vector.get(term) > 0).length / list.length;
		const terms = [...centroid(list)].map(([term, weight]) => [term, weight * share(term)]).sort((a, b) => b[1] - a[1] || compare(a[0], b[0])).slice(0, 2).map(([term]) => term);
		let total = 0, pairs = 0;
		for (let x = 0; x < members.length; x++) for (let y = x + 1; y < members.length; y++) { total += matrix[members[x]][members[y]]; pairs++; }
		return { key: `family:${terms.join("+")}`, label: terms.join(" · "), terms, members: members.map(i => sorted[i]), cohesion: total / pairs };
	}).sort((a, b) => b.members.length - a.members.length || compare(a.key, b.key) || compare(a.members[0], b.members[0]));
	const repeats = new Map();
	for (const family of families) { const count = (repeats.get(family.key) ?? 0) + 1; repeats.set(family.key, count); if (count > 1) family.key += `#${count}`; }
	return families.slice(0, Math.max(0, limit));
}
