// Subconcepts inside one overview group: child folders first, then content clusters.
// Content clusters use identifier and heading terms, recorded links and, when supplied, semantic affinity from semctx search profiles.
// A subconcept organizes the drawing and the drill-down. It does not prove a dependency or a shared behavior.
import { cosine, distinctiveTerms } from "./embedding.js";
const compare = (a, b) => a < b ? -1 : a > b ? 1 : 0;
const segments = path => path ? path.split("/") : [];
// A child folder that holds this share of at least `dominantMinimum` items is split into content clusters at the same level.
const dominantShare = .7, dominantMinimum = 12;
// Weights of the content similarity. Without semantic affinity, terms and links share its weight.
const semanticWeight = .5, termWeight = .35, linkWeight = .15;
function commonFolder(items) {
	let common;
	for (const item of items) {
		const parts = segments(item.folder);
		if (!common) { common = parts; continue; }
		let depth = 0; while (depth < common.length && depth < parts.length && common[depth] === parts[depth]) depth++;
		common = common.slice(0, depth);
	}
	return (common ?? []).join("/");
}
const pairKey = (a, b) => a < b ? `${a}\u0000${b}` : `${b}\u0000${a}`;
/** Pair lookups normalized by their largest value, so the strongest pair is 1. */
function normalizedPairs(pairs) {
	const values = new Map();
	for (const { a, b, weight } of pairs ?? []) if (a !== b && weight > 0) values.set(pairKey(a, b), Math.max(values.get(pairKey(a, b)) ?? 0, weight));
	const top = Math.max(0, ...values.values());
	return { size: values.size, get: (a, b) => top ? (values.get(pairKey(a, b)) ?? 0) / top : 0 };
}
/**
 * Item vectors reweighted by ln(n / members that carry the term), then L2-normalized.
 * Words that every member shares, such as a common manual title in the file names, cannot separate members, so they drop.
 */
function localVectors(items) {
	const frequency = new Map();
	for (const item of items) for (const term of item.vector?.keys() ?? []) frequency.set(term, (frequency.get(term) ?? 0) + 1);
	return new Map(items.map(item => {
		const weights = [...item.vector ?? []].map(([term, weight]) => [term, weight * Math.log(items.length / frequency.get(term))]).filter(([, weight]) => weight > 0);
		const norm = Math.hypot(...weights.map(([, weight]) => weight));
		return [item.id, norm ? new Map(weights.map(([term, weight]) => [term, weight / norm])) : undefined];
	}));
}
/**
 * Ward-linkage clusters of `items` over the dissimilarity 1 - similarity: at most `count` clusters, cut where the merge cost rises most.
 * Ward merges small outliers early, so clusters stay comparable in size. A cut that leaves more than 75% of the items in one cluster
 * is not chosen. Without a clear rise the cut keeps `count` clusters. Slots merge in sorted ID order on ties, so input order does not matter.
 */
function cluster(items, similarity, count) {
	const n = items.length, distance = items.map(() => new Float64Array(n)), clusters = items.map((_, index) => [index]);
	if (n <= count) return items.map(item => [item]);
	for (let i = 0; i < n; i++) for (let j = i + 1; j < n; j++) distance[i][j] = distance[j][i] = 1 - similarity(items[i], items[j]);
	// `costs[k]` is the cost of the merge that leaves k clusters.
	const cuts = new Map(), costs = [];
	for (let live = n; live > 1; live--) {
		if (live <= count) cuts.set(live, clusters.filter(Boolean).map(indices => indices.map(index => items[index])));
		let best = Infinity, left = -1, right = -1;
		for (let i = 0; i < n; i++) if (clusters[i]) for (let j = i + 1; j < n; j++) if (clusters[j] && distance[i][j] < best) { best = distance[i][j]; left = i; right = j; }
		const a = clusters[left].length, b = clusters[right].length;
		// The Lance-Williams update for Ward linkage.
		for (let other = 0; other < n; other++) if (clusters[other] && other !== left && other !== right) {
			const c = clusters[other].length;
			distance[left][other] = distance[other][left] = ((a + c) * distance[left][other] + (b + c) * distance[right][other] - c * best) / (a + b + c);
		}
		clusters[left] = [...clusters[left], ...clusters[right]].sort((x, y) => x - y); clusters[right] = null;
		costs[live - 1] = best;
	}
	let chosen = count, gain = 1e-9;
	for (let k = count; k >= 2; k--) {
		const rise = costs[k - 1] - costs[k], largest = Math.max(...cuts.get(k).map(group => group.length));
		if (rise > gain && largest <= .75 * n) { chosen = k; gain = rise; }
	}
	return cuts.get(chosen);
}
/** About √(n / 2) clusters, at least 2 and at most `limit`. */
export function clusterCount(n, limit) { return Math.max(2, Math.min(limit, Math.round(Math.sqrt(n / 2)))); }
// Ward linkage is cubic in the item count. Larger sets bucket each item under its strongest common term instead.
const linkageLimit = 160;
/** The `count - 1` terms that most items carry, but not more than 60% of them; each item joins its strongest one, the rest share one bucket. */
function termBuckets(items, count) {
	const frequency = new Map();
	for (const item of items) for (const term of item.vector?.keys() ?? []) frequency.set(term, (frequency.get(term) ?? 0) + 1);
	const chosen = [...frequency].filter(([, seen]) => seen <= .6 * items.length && seen > 1).sort((a, b) => b[1] - a[1] || compare(a[0], b[0])).slice(0, count - 1).map(([term]) => term);
	const buckets = new Map(chosen.map(term => [term, []])), rest = [];
	for (const item of items) {
		let best, weight = 0;
		for (const term of chosen) { const value = item.vector?.get(term) ?? 0; if (value > weight) { weight = value; best = term; } }
		(best ? buckets.get(best) : rest).push(item);
	}
	return [...buckets.values(), rest].filter(group => group.length);
}
/**
 * Subconcepts of a group's members.
 * `items`: `{ id, name, folder, vector }`, where `name` is a stable identity such as the path.
 * `affinity` and `links`: undirected `{ a, b, weight }` lists over item IDs.
 * Members in several child folders split by folder. A dominant child folder, or members in one folder, split into content clusters.
 * The result is ordered by size, then key, and folders beyond `limit` fold into one last subconcept. Every item belongs to exactly one subconcept.
 */
export function partitionSubconcepts(items, { affinity = [], links = [], limit = 8 } = {}) {
	const sorted = [...items].sort((a, b) => compare(a.name, b.name) || compare(a.id, b.id));
	if (sorted.length < 2) return sorted.length ? [{ key: `content:${sorted[0].name}`, label: sorted[0].name.split("/").at(-1) ?? sorted[0].name, kind: "content", members: [sorted[0].id], terms: [] }] : [];
	const local = localVectors(sorted);
	const semantic = normalizedPairs(affinity), linked = normalizedPairs(links.map(link => ({ ...link, weight: Math.log1p(link.weight) })));
	const similarity = semantic.size
		? (a, b) => semanticWeight * semantic.get(a.id, b.id) + termWeight * cosine(a.vector, b.vector) + linkWeight * linked.get(a.id, b.id)
		: (a, b) => (termWeight + semanticWeight * .6) * cosine(a.vector, b.vector) + (linkWeight + semanticWeight * .4) * linked.get(a.id, b.id);
	// Each content split reweights its own members, so words that a whole dominant folder shares drop too.
	const content = (members, prefix, slots) => {
		const own = localVectors(members), weighted = members.map(item => ({ ...item, vector: own.get(item.id) }));
		const groups = members.length > linkageLimit ? termBuckets(weighted, Math.min(slots, members.length)) : cluster(weighted, similarity, Math.min(slots, members.length));
		const terms = distinctiveTerms(groups.map((group, index) => ({ id: String(index), vectors: group.map(item => item.vector), exclude: prefix ? [prefix] : [] })), { limit: 2 });
		return groups.map((group, index) => {
			const words = terms.get(String(index)) ?? [];
			return { key: `content:${group[0].name}`, label: [prefix, words.length ? words.join(" · ") : `${group.length} files`].filter(Boolean).join(" · "), kind: "content", members: group.map(item => item.id) };
		});
	};
	const common = commonFolder(sorted), depth = segments(common).length, buckets = new Map();
	for (const item of sorted) {
		const child = segments(item.folder)[depth] ?? "";
		if (!buckets.has(child)) buckets.set(child, []);
		buckets.get(child).push(item);
	}
	let result, rest;
	if (buckets.size < 2) result = content(sorted, "", clusterCount(sorted.length, limit));
	else {
		const ordered = [...buckets].sort((a, b) => b[1].length - a[1].length || compare(a[0], b[0]));
		const kept = ordered.length > limit ? ordered.slice(0, limit - 1) : ordered, folded = ordered.slice(kept.length).flatMap(([, members]) => members);
		result = [];
		for (const [child, members] of kept) {
			const folder = child ? [common, child].filter(Boolean).join("/") : common;
			const label = child || `${segments(common).at(-1) ?? "root"} · files`;
			if (members.length >= dominantMinimum && members.length >= dominantShare * sorted.length) result.push(...content(members, label, Math.max(2, limit - kept.length + 1)));
			else result.push({ key: `folder:${folder}${child ? "" : "/"}`, label, kind: "folder", folder, members: members.map(item => item.id) });
		}
		if (folded.length) rest = { key: `folder:${common}/*`, label: `${ordered.length - kept.length} other folders`, kind: "folder", members: folded.map(item => item.id) };
	}
	result.sort((a, b) => b.members.length - a.members.length || compare(a.key, b.key));
	if (rest) result.push(rest);
	const labels = new Map();
	for (const entry of result) { const count = (labels.get(entry.label) ?? 0) + 1; labels.set(entry.label, count); if (count > 1) entry.label += ` ${count}`; }
	const terms = distinctiveTerms(result.map((entry, index) => ({ id: String(index), vectors: entry.members.map(id => local.get(id)), exclude: [entry.label] })));
	for (const [index, entry] of result.entries()) entry.terms = terms.get(String(index)) ?? [];
	return result;
}
