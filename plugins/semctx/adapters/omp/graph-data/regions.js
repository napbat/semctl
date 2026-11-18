// Map regions: sets of overview groups that share a documentation family, a home folder, or recorded links with identifier terms.
// Regions organize the drawing and the group list. They do not prove a dependency, an ownership boundary or a semantic relation.
import { centroid, cosine } from "./embedding.js";
const compare = (a, b) => a < b ? -1 : a > b ? 1 : 0;
// A home folder holds at least this share of a group's member folders.
const homeShare = .6;
// A path partition is informative only when its largest region holds at most this share of the partitioned groups.
const dominantShare = .7, maximumDepth = 3;
// Term regions merge on average linkage of .6 · normalized links + .4 · term cosine, and need one term that every member carries.
const linkWeight = .6, termWeight = .4, termThreshold = .3;
const segments = path => path ? path.split("/") : [];
/** The deepest folder that holds at least 60% of the member folders. A file contributes its parent folder; a folder node contributes itself. "" is the checkout root. */
export function homeFolder(folders) {
	const counts = new Map(); let total = 0;
	for (const folder of folders) {
		if (typeof folder !== "string") continue;
		total++;
		const parts = segments(folder);
		for (let depth = 1; depth <= parts.length; depth++) { const prefix = parts.slice(0, depth).join("/"); counts.set(prefix, (counts.get(prefix) ?? 0) + 1); }
	}
	let best = "";
	for (const [prefix, count] of counts) if (count >= homeShare * total && (segments(prefix).length > segments(best).length || (segments(prefix).length === segments(best).length && compare(prefix, best) < 0))) best = prefix;
	return best;
}
const pairKey = (a, b) => a < b ? `${a}\u0000${b}` : `${b}\u0000${a}`;
function linkStrength(links) {
	const totals = new Map(), pairs = new Map();
	for (const { a, b, weight } of links) {
		if (a === b || !(weight > 0)) continue;
		const key = pairKey(a, b); pairs.set(key, (pairs.get(key) ?? 0) + weight);
		totals.set(a, (totals.get(a) ?? 0) + weight); totals.set(b, (totals.get(b) ?? 0) + weight);
	}
	// Normalized like a cosine, so one busy group does not attract every other group.
	return (a, b) => { const weight = pairs.get(pairKey(a, b)); return weight ? weight / Math.sqrt(totals.get(a) * totals.get(b)) : 0; };
}
// One region per prefix with at least 2 groups. Deeper prefixes refine a region that would hold most of the map.
function pathRegions(items) {
	let fallback;
	for (let depth = 1; depth <= maximumDepth; depth++) {
		const buckets = new Map();
		for (const item of items) {
			const parts = segments(item.home); if (parts.length < depth) continue;
			const prefix = parts.slice(0, depth).join("/");
			if (!buckets.has(prefix)) buckets.set(prefix, []); buckets.get(prefix).push(item);
		}
		const regions = [...buckets].filter(([, members]) => members.length > 1).map(([prefix, members]) => ({ prefix, members }));
		if (!regions.length) break;
		const largest = Math.max(...regions.map(region => region.members.length));
		if (largest <= dominantShare * items.length) return regions;
		if (regions.length > 1) fallback = regions;
	}
	return fallback ?? [];
}
// Average linkage over the items that no other evidence placed. Slots merge in sorted ID order on ties.
function termRegions(items, strength) {
	const n = items.length, linkage = items.map(() => new Float64Array(n)), clusters = items.map((_, index) => [index]);
	for (let i = 0; i < n; i++) for (let j = i + 1; j < n; j++) linkage[i][j] = linkage[j][i] = linkWeight * strength(items[i].id, items[j].id) + termWeight * cosine(items[i].vector, items[j].vector);
	for (;;) {
		let best = -Infinity, left = -1, right = -1;
		for (let i = 0; i < n; i++) if (clusters[i]) for (let j = i + 1; j < n; j++) if (clusters[j] && linkage[i][j] > best) { best = linkage[i][j]; left = i; right = j; }
		if (left < 0 || best < termThreshold) break;
		const a = clusters[left].length, b = clusters[right].length;
		for (let other = 0; other < n; other++) if (clusters[other] && other !== left && other !== right) linkage[left][other] = linkage[other][left] = (a * linkage[left][other] + b * linkage[right][other]) / (a + b);
		clusters[left] = [...clusters[left], ...clusters[right]].sort((x, y) => x - y); clusters[right] = null;
	}
	return clusters.filter(members => members?.length > 1).flatMap(indices => {
		const members = indices.map(index => items[index]), vectors = members.map(item => item.vector);
		// A label term must occur in every member, so the label never describes only part of the region.
		const terms = [...centroid(vectors)].filter(([term]) => vectors.every(vector => vector?.get(term) > 0)).slice(0, 2).map(([term]) => term);
		return terms.length ? [{ terms, members }] : [];
	});
}
/**
 * Regions over overview groups, in this precedence: a documentation family that holds at least 2 groups,
 * then a shared home-folder prefix, then recorded links with shared identifier terms. Every region has at least 2 groups.
 * At most `limit` regions remain, largest first; the members of a dropped region stay unassigned.
 * Pure and deterministic: input order does not change the result.
 */
export function mapRegions(groups, links = [], { limit = 6 } = {}) {
	const items = [...groups].sort((a, b) => compare(a.id, b.id)).map(group => ({ ...group, home: typeof group.home === "string" ? group.home : "" }));
	const strength = linkStrength(links), regions = [], placed = new Set();
	const families = new Map();
	for (const item of items) if (item.family?.key) { if (!families.has(item.family.key)) families.set(item.family.key, []); families.get(item.family.key).push(item); }
	for (const [key, members] of families) if (members.length > 1) {
		regions.push({ key, label: members[0].family.label, kind: "doc", members }); for (const item of members) placed.add(item.id);
	}
	for (const { prefix, members } of pathRegions(items.filter(item => !placed.has(item.id)))) {
		regions.push({ key: `region:path:${prefix}`, label: prefix, kind: "path", members }); for (const item of members) placed.add(item.id);
	}
	const labels = new Set(regions.map(region => region.label));
	for (const { terms, members } of termRegions(items.filter(item => !placed.has(item.id)), strength)) {
		let label = terms.join(" · ");
		for (let ordinal = 2; labels.has(label); ordinal++) label = `${terms.join(" · ")} ${ordinal}`;
		labels.add(label);
		regions.push({ key: `region:terms:${terms.join("+")}:${members[0].id}`, label, kind: "terms", members });
	}
	const weight = region => region.members.reduce((sum, item) => sum + (item.weight ?? 1), 0);
	const kept = regions.sort((a, b) => weight(b) - weight(a) || compare(a.key, b.key)).slice(0, Math.max(0, limit)).map(region => ({
		key: region.key, label: region.label, kind: region.kind, members: region.members.map(item => item.id),
		...(region.kind === "path" ? { folder: region.label } : {}),
	}));
	const regionOf = new Map(kept.flatMap(region => region.members.map(id => [id, region])));
	return { regions: kept, regionOf };
}
