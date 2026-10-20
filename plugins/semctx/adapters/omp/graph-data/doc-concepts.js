// Human concepts from the checkout's Markdown documents: titles, headings and explicit path, file or identifier mentions.
// Evidence is lexical and explicit. No model or server inference runs, and a mention does not prove behavior.
import { centroid, identifierTerms } from "./embedding.js";
import { normalizeConceptPath } from "./palette.js";
const compare = (a, b) => a < b ? -1 : a > b ? 1 : 0;
export const DOC_SOURCE_LIMITS = Object.freeze({ files: 256, fileBytes: 1 << 20, totalBytes: 16 << 20 });
export function isDocPath(path) { return typeof path === "string" && /\.(?:md|markdown|mdx)$/i.test(path); }
const families = new WeakMap();
export function bindConceptFamily(node, family) { if (family && typeof family.key === "string") families.set(node, family); else families.delete(node); }
export function conceptFamily(node) { return families.get(node); }
// Parse bounds: one pathological document cannot dominate the cost. Every parse pattern is linear in the text length.
// A catalog path has at most 4096 characters, so a longer token cannot name a file.
const mentionLimit = 4000, headingLimit = 24, tokenLimit = 4096;
// Weights fall as a mention gets less explicit. Names that more files share than the ambiguity bound give no evidence.
const fileWeight = 1, suffixWeight = .8, stemWeight = .6, symbolWeight = .5, suffixFiles = 2, identifierFiles = 3, evidenceCap = 3;
// A doc that points at more code than this is an index (hub) of the checkout, not a concept.
const hubMinimum = 64, hubShare = .2;
// A concept needs at least 3 evidenced code files from at least 2 distinct mentions, and must keep at least 3 files against competing docs.
// One example path, such as a sample argument in a tool page, does not name a topic.
const minimumReach = 3, minimumMentions = 2, minimumWinners = 3;
// Reference propagation needs a majority of the assigned neighbours that also covers a quarter of all code neighbours.
// Folder inheritance needs a clear plurality of at least 2 files that also covers a fifth of the folder, so a few files cannot claim a large tree.
// A directory node takes a concept only when that concept holds at least half of its files.
const propagationRounds = 3, propagationShare = .5, propagationCoverage = .25, folderShare = .6, folderCount = 2, folderCoverage = .2, directoryShare = .6, directoryCoverage = .5;
const labelLimit = 28, labelKeep = 6, headingWeight = .35, folderWeight = .3;

// Literal-first patterns let the engine skip to candidates. Line constructs are found with indexOf and confirmed at their line start.
const headingLine = / {0,3}(#{1,6})(?:[ \t]+([^\r\n]*?))?(?:[ \t]+#+)?[ \t]*(?=\r?\n|$)/y, definition = /(?:^|\n) {0,3}\[[^\]\n]+\]:[ \t]*<?([^\s<>]+)>?[^\r\n]*/g;
const codeSpan = /`([^`\n]+)`/g, codeSeparator = /[^\w./\\*#:-]+/, link = /\[[^\]\n]*\]\([ \t]*<?([^()\s<>]*)>?(?:[ \t]+(?:"[^"\n]*"|'[^'\n]*'))?[ \t]*\)/g;
const edgePunctuation = /^[("'<[{]+|[)"'>\]},;:!?.]+$/g;
const frontMatter = /^---[ \t]*\r?\n([\s\S]*?)\r?\n(?:---|\.\.\.)[ \t]*(?:\r?\n|$)/y, identifier = /^[A-Za-z_]\w*$/;
// Character classes of bare path-like tokens: 1 = [\w.-] (head and tail), 2 = "*" (tail only).
const pathCharacters = new Uint8Array(128);
for (const character of "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_.-") pathCharacters[character.charCodeAt(0)] = 1;
pathCharacters[42] = 2;
const pathHead = code => pathCharacters[code] === 1, pathTail = code => pathCharacters[code] > 0;
function addMention(doc, raw, linked) {
	if (!raw || raw.length > tokenLimit || doc.count >= mentionLimit) return;
	doc.count++; const key = (linked ? "L" : "C") + raw; doc.mentions.set(key, (doc.mentions.get(key) ?? 0) + 1);
}
// Bare tokens match `[\w.-]+(/[\w.*-]+)+/?` with the longest head. A slash scan is linear; "scheme://" URLs are skipped to the next space.
function scanPaths(text, doc) {
	for (let slash = text.indexOf("/"); slash >= 0 && doc.count < mentionLimit; slash = text.indexOf("/", slash + 1)) {
		if (text.charCodeAt(slash - 1) === 58 && text.charCodeAt(slash + 1) === 47) { while (slash < text.length && text.charCodeAt(slash) > 32) slash++; continue; }
		let start = slash, end = slash;
		while (start > 0 && pathHead(text.charCodeAt(start - 1))) start--;
		if (start === slash) continue;
		while (text.charCodeAt(end) === 47 && pathTail(text.charCodeAt(end + 1))) { end++; while (pathTail(text.charCodeAt(end))) end++; }
		if (end === slash) continue;
		if (text.charCodeAt(end) === 47) end++;
		addMention(doc, text.slice(start, end), false); slash = end - 1;
	}
}
// Reference definitions, code spans, link targets and bare path-like tokens are mentions. URLs, images and prose words are not.
// Each matched construct is blanked before the next scan, so one token counts once.
function scanMentions(region, doc) {
	let rest = region;
	if (rest.includes("]:")) rest = rest.replace(definition, (_, target) => { addMention(doc, target, true); return "\n"; });
	if (rest.includes("`")) rest = rest.replace(codeSpan, (_, body) => { if (doc.count < mentionLimit) for (const token of body.split(codeSeparator)) addMention(doc, token, false); return " "; });
	if (rest.includes("](")) rest = rest.replace(link, (_, target, offset, text) => { if (text.charCodeAt(offset - 1) !== 33) addMention(doc, target, true); return " "; });
	scanPaths(rest, doc);
}
function cleanMarkdown(text) {
	return text.replace(/!?\[([^\]]*)\]\([^)]*\)/g, "$1").replace(/<[^>]*>/g, "").replace(/[`*]+|~~/g, "").replace(/(?<!\w)_+|_+(?!\w)/g, "")
		.replace(/&amp;/g, "&").replace(/&lt;/g, "<").replace(/&gt;/g, ">").replace(/&quot;/g, "\"").replace(/&#39;/g, "'").replace(/\s+/g, " ").trim();
}
// Start of the line that holds `index` when at most 3 spaces precede it on that line, otherwise -1.
function lineStart(text, index) {
	let start = index; while (start > 0 && index - start < 3 && text.charCodeAt(start - 1) === 32) start--;
	return start === 0 || text.charCodeAt(start - 1) === 10 ? start : -1;
}
// Remove blockquote markers before measuring code indentation. A tab advances to the next 4-column stop.
function blockLine(text, start, end) {
	let position = start, quotes = 0;
	for (;;) {
		let marker = position; while (marker < end && marker - position < 3 && text.charCodeAt(marker) === 32) marker++;
		if (text.charCodeAt(marker) !== 62) break;
		position = marker + 1; quotes++;
		if (text.charCodeAt(position) === 32 || text.charCodeAt(position) === 9) position++;
	}
	let indent = 0;
	while (position < end) {
		const code = text.charCodeAt(position);
		if (code === 32) indent++; else if (code === 9) indent += 4 - indent % 4; else break;
		position++;
	}
	return { position, indent, quotes };
}
// List content starts after its marker and 1–4 spaces. Extra indentation can start a code block within the item.
function listContent(text, position, end, indent) {
	const first = text.charCodeAt(position); let marker = position;
	if (first === 42 || first === 43 || first === 45) marker++;
	else {
		while (marker < end && marker - position < 9 && text.charCodeAt(marker) >= 48 && text.charCodeAt(marker) <= 57) marker++;
		if (marker === position || (text.charCodeAt(marker) !== 46 && text.charCodeAt(marker) !== 41)) return undefined;
		marker++;
	}
	if (text.charCodeAt(marker) !== 32 && text.charCodeAt(marker) !== 9) return undefined;
	let after = marker, padding = 0;
	while (after < end && (text.charCodeAt(after) === 32 || text.charCodeAt(after) === 9)) {
		padding += text.charCodeAt(after) === 9 ? 4 - (indent + marker - position + padding) % 4 : 1; after++;
	}
	if (padding > 4) { after = marker + 1; padding = text.charCodeAt(marker) === 9 ? 4 - (indent + marker - position) % 4 : 1; }
	return { position: after, indent: indent + marker - position + padding };
}
// Code blocks give no titles, headings or mentions. Track quote/list containers so nested fences and indented code stay excluded.
// An unclosed fence ends with its containing quote/list, or at the end of the document.
function proseRegions(text) {
	const regions = [], lists = []; let fence, from = 0, start = 0, previousQuotes = 0;
	while (start < text.length) {
		let end = text.indexOf("\n", start); if (end < 0) end = text.length;
		const next = Math.min(end + 1, text.length), first = text.charCodeAt(start);
		// Plain unindented lines cannot open code blocks or list containers.
		if (!fence && !lists.length && first !== 32 && first !== 9 && first !== 62 && first !== 96 && first !== 126 && first !== 42 && first !== 43 && first !== 45 && !(first >= 48 && first <= 57)) {
			previousQuotes = 0; start = next; continue;
		}
		const block = blockLine(text, start, end), blank = block.position >= end || text.charCodeAt(block.position) === 13;
		if (fence && !blank && (block.quotes < fence.quotes || block.indent < fence.listIndent)) fence = undefined;
		if (block.quotes !== previousQuotes) { lists.length = 0; previousQuotes = block.quotes; }
		if (!fence && !blank) while (lists.length && lists[lists.length - 1] > block.indent) lists.pop();
		const parent = lists[lists.length - 1] ?? 0, item = fence || block.indent - parent >= 4 ? undefined : listContent(text, block.position, end, block.indent);
		let position = item?.position ?? block.position, base = fence?.listIndent ?? parent;
		if (item) { lists.push(item.indent); base = item.indent; }
		let indentation = item ? 0 : block.indent - base;
		while (position < end && (text.charCodeAt(position) === 32 || text.charCodeAt(position) === 9)) {
			indentation += text.charCodeAt(position) === 9 ? 4 - indentation % 4 : 1; position++;
		}
		const code = text.charCodeAt(position); let markerEnd = position;
		if (code === 96 || code === 126) while (text.charCodeAt(markerEnd) === code) markerEnd++;
		const length = markerEnd - position, rest = length >= 3 ? text.slice(markerEnd, end) : "";
		let excluded = Boolean(fence) || indentation >= 4;
		if (fence) {
			if (block.quotes === fence.quotes && code === fence.code && length >= fence.length && indentation <= 3 && !rest.trim()) fence = undefined;
		} else if (indentation <= 3 && length >= 3 && (code === 126 || !rest.includes("`"))) {
			fence = { code, length, quotes: block.quotes, listIndent: base }; excluded = true;
		}
		if (excluded) { if (from < start) regions.push(text.slice(from, start)); from = next; }
		start = next;
	}
	if (from < text.length) regions.push(text.slice(from));
	return regions;
}
// ATX headings: the first level-1 text is the title; level-2 and level-3 texts are headings. A full slot skips the parse.
function scanHeadings(region, doc) {
	for (let index = region.indexOf("#"); index >= 0 && (doc.h1 === undefined || doc.headings.length < headingLimit); index = region.indexOf("#", index + 1)) {
		const start = lineStart(region, index); if (start < 0) continue;
		let level = 0; while (level < 7 && region.charCodeAt(index + level) === 35) level++;
		if (level === 1 ? doc.h1 === undefined : level <= 3 && doc.headings.length < headingLimit) {
			headingLine.lastIndex = start;
			const value = cleanMarkdown(headingLine.exec(region)?.[2] ?? "");
			if (value && level === 1) doc.h1 = value; else if (value) doc.headings.push(value);
		}
		index = region.indexOf("\n", index); if (index < 0) break;
	}
}
function parseDoc(source) {
	const doc = { h1: undefined, front: undefined, headings: [], mentions: new Map(), count: 0 };
	let text = source.charCodeAt(0) === 0xfeff ? source.slice(1) : source;
	frontMatter.lastIndex = 0;
	const front = frontMatter.exec(text);
	if (front) {
		const title = /^title:[ \t]*(.+?)[ \t]*$/m.exec(front[1])?.[1].replace(/^(["'])(.*)\1$/, "$2");
		doc.front = title ? cleanMarkdown(title) || undefined : undefined; text = text.slice(frontMatter.lastIndex);
	}
	for (const region of proseRegions(text)) { scanHeadings(region, doc); if (doc.count < mentionLimit) scanMentions(region, doc); }
	return doc;
}
// Normalized mention: no URL, anchor, line suffix, leading "./" or "/". A "*" glob becomes a path prefix.
// Most tokens name nothing, so every rewrite runs only when its trigger character is present.
function normalizeMention(raw) {
	let token = raw.includes("\\") ? raw.replace(/\\/g, "/") : raw;
	if ("(\"'<[{".includes(token[0]) || ")\"'>]},;:!?.".includes(token[token.length - 1])) token = token.replace(edgePunctuation, "");
	if (token.includes(":")) {
		if (/^(?:[a-z][a-z\d+.-]*:\/\/|mailto:|data:)/i.test(token)) return undefined;
		token = token.replace(/(?::\d+)+(?:-\d+)?$/, "");
		const scope = token.lastIndexOf("::"); if (scope >= 0 && !token.includes("/")) token = token.slice(scope + 2);
	}
	const hash = token.indexOf("#"); if (hash === 0) return undefined; if (hash > 0) token = token.slice(0, hash);
	if (token[0] === "." || token[0] === "/") token = token.replace(/^(?:\.\/)+/, "").replace(/^\/+/, "");
	const star = token.indexOf("*"), glob = star >= 0; if (glob) token = token.slice(0, star);
	if (token && ".,;:!?".includes(token[token.length - 1])) token = token.replace(/[.,;:!?]+$/, "");
	return token ? { token, glob } : undefined;
}
function joinPath(folder, token) {
	if (!folder && !token.includes("./") && !token.includes("//") && !token.endsWith("/")) return token;
	const parts = folder ? folder.split("/") : [];
	for (const part of token.split("/")) { if (!part || part === ".") continue; if (part !== "..") parts.push(part); else if (!parts.pop()) return undefined; }
	return parts.join("/");
}
const parentOf = path => { const cut = path.lastIndexOf("/"); return cut < 0 ? "" : path.slice(0, cut); };
const nameOf = path => path.slice(path.lastIndexOf("/") + 1);
const stemOf = path => { const name = nameOf(path), cut = name.lastIndexOf("."); return cut > 0 ? name.slice(0, cut) : name; };
function ancestors(path) { const folders = []; for (let folder = parentOf(path); folder; folder = parentOf(folder)) folders.push(folder); return folders; }
function catalog(data) {
	const byPath = category => {
		const counts = new Map(), ids = new Map();
		for (const node of data.nodes) if (node?.category === category && typeof node.id === "string") {
			const path = normalizeConceptPath(node.path); if (!path) continue;
			counts.set(path, (counts.get(path) ?? 0) + 1); ids.set(path, node.id);
		}
		// Two nodes with one path are ambiguous. Neither receives a concept.
		for (const [path, count] of counts) if (count > 1) ids.delete(path);
		return ids;
	};
	const files = byPath("file"), directories = byPath("directory"), paths = [...files.keys()].sort(compare), code = paths.filter(path => !isDocPath(path));
	const names = new Map(), stems = new Map(), symbols = new Map(), neighbours = new Map(code.map(path => [path, new Set()])), pathOf = new Map(), sizes = new Map();
	const link = (a, b) => { if (a !== b && neighbours.has(a) && neighbours.has(b)) { neighbours.get(a).add(b); neighbours.get(b).add(a); } };
	for (const path of paths) {
		pathOf.set(files.get(path), path);
		for (const [index, key] of [[names, nameOf(path)], [stems, stemOf(path)]]) { if (!index.has(key)) index.set(key, []); index.get(key).push(path); }
		for (const folder of ancestors(path)) sizes.set(folder, (sizes.get(folder) ?? 0) + 1);
	}
	for (const edge of data.edges) {
		const from = pathOf.get(edge?.from), to = pathOf.get(edge?.to); if (!from || !to || from === to) continue;
		if (edge.kind === "reference" && typeof edge.symbol === "string") { if (!symbols.has(edge.symbol)) symbols.set(edge.symbol, new Set()); symbols.get(edge.symbol).add(to); }
		if (edge.kind === "reference" || edge.kind === "import") link(from, to);
	}
	// Files that share a stem within the identifier ambiguity bound (a header, its source and its test) are linked like an import.
	for (const group of stems.values()) if (group.length > 1 && group.length <= identifierFiles) for (const a of group) for (const b of group) link(a, b);
	// Resolved mention targets per call: code tokens resolve from the checkout root, link targets also from the doc folder.
	return { files, directories, paths, code, codeSet: new Set(code), names, stems, symbols, neighbours, sizes, cache: { code: new Map(), linked: new Map() } };
}
const nothing = Object.freeze([]);
// Sorted code paths make a directory or glob prefix one binary-searched range.
function prefixRange(sorted, prefix) {
	const bound = value => { let low = 0, high = sorted.length; while (low < high) { const middle = (low + high) >> 1; if (sorted[middle] < value) low = middle + 1; else high = middle; } return low; };
	const low = bound(prefix); if (low === sorted.length || !sorted[low].startsWith(prefix)) return nothing;
	return sorted.slice(low, bound(prefix + "\uffff"));
}
const spread = paths => paths.map(path => [path, 1 / Math.sqrt(paths.length)]);
const shared = (paths, weight) => paths.map(path => [path, weight / paths.length]);
// Precedence: exact file, directory or glob prefix, path suffix, file stem, defining file of a referenced symbol.
function resolveMention(index, mention, linked, folder) {
	const { token, glob } = mention, roots = [];
	if (linked) {
		const relative = joinPath(folder, token);
		// A link that leaves the checkout names an external file, not a local suffix or basename.
		if (relative === undefined) return nothing;
		roots.push(relative);
	}
	roots.push(joinPath("", token));
	for (const root of roots) {
		if (!root) continue;
		if (glob) { const below = prefixRange(index.code, token.endsWith("/") ? root + "/" : root); if (below.length) return spread(below); continue; }
		if (index.files.has(root)) return [[root, fileWeight]];
		// A bare word names a directory only with a slash or as a link target. Words such as `hv` or `tools` are too ambiguous.
		if (linked || token.includes("/")) { const below = prefixRange(index.code, root + "/"); if (below.length) return spread(below); }
	}
	if (glob) return nothing;
	const suffix = token.startsWith("../") || token.endsWith("/") ? token.replace(/^(?:\.\.\/)+/, "").replace(/\/+$/, "") : token;
	if (!suffix || suffix.includes("..")) return nothing;
	const named = index.names.get(nameOf(suffix))?.filter(path => path === suffix || path.endsWith("/" + suffix));
	if (named?.length) return named.length <= suffixFiles ? shared(named, suffixWeight) : nothing;
	if (suffix.length < 5 || !identifier.test(suffix) || !(suffix.includes("_") || /[a-z\d][A-Z]/.test(suffix))) return nothing;
	const stems = index.stems.get(suffix);
	if (stems && stems.length <= identifierFiles) return shared(stems, stemWeight);
	const defined = index.symbols.get(suffix);
	return defined && defined.size <= identifierFiles ? shared([...defined].sort(compare), symbolWeight) : nothing;
}
function humanize(text) {
	const words = text.replace(/([a-z\d])([A-Z])/g, "$1 $2").replace(/[\s._-]+/g, " ").trim();
	return words ? words[0].toUpperCase() + words.slice(1) : "";
}
function fallbackTitle(path) {
	const stem = stemOf(path);
	if (!/^readme$/i.test(stem)) return humanize(stem) || stem;
	const folder = parentOf(path); return humanize(nameOf(folder)) || "Readme";
}
// The humanized folder name takes the casing that a member title uses for it, so "amd" becomes "AMD" when a title says "AMD".
function familyLabel(folder, titles) {
	const plain = humanize(nameOf(folder)), pattern = new RegExp(`(?<![\\p{L}\\d])${plain.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}(?![\\p{L}\\d])`, "iu");
	for (const title of titles) { const match = plain ? pattern.exec(title) : null; if (match) return match[0][0].toUpperCase() + match[0].slice(1); }
	return plain || folder;
}
function capLabel(text, limit) {
	if (text.length <= limit) return text;
	const head = text.slice(0, limit - 1), space = head.lastIndexOf(" ");
	return `${(space > 0 ? head.slice(0, space) : head).replace(/[\s,;:·–—-]+$/, "")}…`;
}
// A title that fits stays whole. A longer title keeps a subtitle-free prefix of at least 6 characters.
// Coordinating words are not subtitle separators: cutting "source and implementation bindings" at "and" loses the subject.
function shortLabel(title) {
	if (title.length <= labelLimit) return title;
	let cut = -1;
	for (const separator of [" — ", " – ", ": ", " ("]) {
		const index = title.indexOf(separator, labelKeep);
		if (index >= 0 && (cut < 0 || index < cut)) cut = index;
	}
	return cut < 0 ? title : title.slice(0, cut).replace(/[\s,;:·–—-]+$/, "");
}
// Repeated labels take the doc folder name. An ordinal resolves what still repeats.
function labels(concepts) {
	const bases = new Map(concepts.map(concept => [concept.key, shortLabel(concept.title)])), counts = new Map();
	for (const base of bases.values()) { const label = capLabel(base, labelLimit); counts.set(label, (counts.get(label) ?? 0) + 1); }
	const used = new Set(), result = new Map();
	for (const concept of concepts) {
		const base = bases.get(concept.key); let named = capLabel(base, labelLimit);
		if (counts.get(named) > 1) { const suffix = ` · ${capLabel(nameOf(parentOf(concept.source)) || stemOf(concept.source), 12)}`; named = capLabel(base, labelLimit - suffix.length) + suffix; }
		let label = named;
		for (let ordinal = 2; used.has(label); ordinal++) { const suffix = ` ${ordinal}`; label = capLabel(named, labelLimit - suffix.length) + suffix; }
		used.add(label); result.set(concept.key, label);
	}
	return result;
}
function termVector(title, headings, source) {
	const weights = new Map(), add = (text, weight) => { for (const term of identifierTerms(text)) weights.set(term, (weights.get(term) ?? 0) + weight); };
	add(title, 1); for (const value of headings) add(value, headingWeight); add(nameOf(parentOf(source)), folderWeight);
	return centroid([weights]);
}
// Plurality with its count and share. Ties have no winner.
function plurality(tally) {
	let best, count = 0, total = 0, tied = false;
	for (const [key, value] of tally) { total += value; if (value > count) { best = key; count = value; tied = false; } else if (value === count) tied = true; }
	return best === undefined || tied ? undefined : { key: best, count, share: count / total };
}
function tallyAdd(tally, key) { tally.set(key, (tally.get(key) ?? 0) + 1); }
// Concept counts of the assigned files below each folder. The checkout root is not a folder here.
function folderTallies(assigned) {
	const folders = new Map();
	for (const [path, { key }] of assigned) for (const folder of ancestors(path)) { if (!folders.has(folder)) folders.set(folder, new Map()); tallyAdd(folders.get(folder), key); }
	return folders;
}
function selectDocs(index, docs) {
	const sources = new Map();
	for (const doc of docs) {
		const path = normalizeConceptPath(doc?.path);
		if (!path || typeof doc.text !== "string" || !isDocPath(path) || !index.files.has(path)) continue;
		// Repeated paths keep the smallest text, so source order cannot change the result.
		const text = doc.text.slice(0, DOC_SOURCE_LIMITS.fileBytes), known = sources.get(path);
		if (known === undefined || text < known) sources.set(path, text);
	}
	const selected = []; let total = 0;
	for (const path of [...sources.keys()].sort(compare)) {
		const text = sources.get(path);
		if (selected.length >= DOC_SOURCE_LIMITS.files || total + text.length > DOC_SOURCE_LIMITS.totalBytes) break;
		total += text.length; selected.push({ path, text });
	}
	return selected;
}
function evidenceOf(index, doc, path) {
	const evidence = new Map(), cache = index.cache, folder = parentOf(path); let mentions = 0;
	for (const [key, count] of doc.mentions) {
		const linked = key[0] === "L", raw = key.slice(1), cacheKey = linked ? `${folder}\u0000${raw}` : raw;
		let targets = (linked ? cache.linked : cache.code).get(cacheKey);
		if (!targets) {
			const mention = normalizeMention(raw);
			targets = mention ? resolveMention(index, mention, linked, folder) : nothing;
			(linked ? cache.linked : cache.code).set(cacheKey, targets);
		}
		let evidenced = false;
		for (const [target, weight] of targets) if (target !== path && index.codeSet.has(target)) { evidence.set(target, Math.min(evidenceCap, (evidence.get(target) ?? 0) + weight * count)); evidenced = true; }
		if (evidenced) mentions++;
	}
	return { evidence, mentions };
}
// Each evidenced code file goes to its best candidate: score, then the narrower doc, then the source path.
// A candidate that keeps fewer than 3 files dissolves, weakest first, and its files go to their best remaining candidate.
function mentionWinners(candidates, codeCount) {
	const preferences = new Map();
	for (const doc of candidates) {
		const specificity = Math.log1p(codeCount / doc.reach);
		for (const [path, evidence] of doc.evidence) { if (!preferences.has(path)) preferences.set(path, []); preferences.get(path).push({ doc, score: evidence * specificity }); }
	}
	for (const list of preferences.values()) list.sort((a, b) => b.score - a.score || a.doc.reach - b.doc.reach || compare(a.doc.path, b.doc.path));
	const active = new Set(candidates), winners = new Map(), best = path => preferences.get(path).find(item => active.has(item.doc))?.doc;
	for (const path of [...preferences.keys()].sort(compare)) winners.set(path, best(path));
	for (;;) {
		const won = new Map([...active].map(doc => [doc, 0]));
		for (const doc of winners.values()) if (doc) won.set(doc, won.get(doc) + 1);
		let weakest;
		for (const [doc, count] of won) if (!weakest || count < weakest.count || (count === weakest.count && compare(doc.key, weakest.doc.key) < 0)) weakest = { doc, count };
		if (!weakest || weakest.count >= minimumWinners) break;
		active.delete(weakest.doc);
		for (const [path, doc] of winners) if (doc === weakest.doc) winners.set(path, best(path));
	}
	const assigned = new Map();
	for (const [path, doc] of winners) if (doc) assigned.set(path, { key: doc.key, via: "mention" });
	return { active, assigned };
}
// Synchronous rounds: every decision in one round reads the state from the round start.
function propagate(index, assigned) {
	for (let round = 0; round < propagationRounds; round++) {
		const updates = [];
		for (const path of index.code) {
			if (assigned.has(path)) continue;
			const tally = new Map(), neighbours = index.neighbours.get(path);
			for (const other of neighbours) { const item = assigned.get(other); if (item) tallyAdd(tally, item.key); }
			// The coverage term counts every code neighbour, so a sparse assignment cannot spread through unassigned code.
			const top = plurality(tally); if (top && top.share >= propagationShare && top.count >= propagationCoverage * neighbours.size) updates.push([path, top.key]);
		}
		if (!updates.length) return;
		for (const [path, key] of updates) assigned.set(path, { key, via: "reference" });
	}
}
// One pass from the current state: every remaining file takes the decision of its nearest qualifying ancestor folder.
function inheritFolders(index, assigned) {
	const inherited = new Map();
	for (const [folder, tally] of folderTallies(assigned)) {
		const top = plurality(tally);
		if (top && top.count >= Math.max(folderCount, folderCoverage * index.sizes.get(folder)) && top.share >= folderShare) inherited.set(folder, top.key);
	}
	const inheritance = [];
	for (const path of index.paths) { const folder = assigned.has(path) ? undefined : ancestors(path).find(value => inherited.has(value)); if (folder) inheritance.push([path, inherited.get(folder)]); }
	for (const [path, key] of inheritance) assigned.set(path, { key, via: "folder" });
}
function directoryAssignments(index, assigned) {
	const below = folderTallies(assigned), assignments = [];
	for (const [path, id] of index.directories) {
		const top = below.has(path) ? plurality(below.get(path)) : undefined;
		if (top && top.share >= directoryShare && top.count >= directoryCoverage * (index.sizes.get(path) ?? 0)) assignments.push([id, { key: top.key, via: "folder" }]);
	}
	return assignments;
}
// Concept docs arrive in path order, so labels and ordinals follow concept keys.
function describeConcepts(concepts, members) {
	const folderTitles = new Map();
	for (const doc of concepts) { const folder = parentOf(doc.path); if (folder) { if (!folderTitles.has(folder)) folderTitles.set(folder, []); folderTitles.get(folder).push(doc.title); } }
	const familyOf = new Map([...folderTitles].filter(([, titles]) => titles.length >= 2).map(([folder, titles]) => [folder, Object.freeze({ key: `family:${folder}`, label: familyLabel(folder, titles) })]));
	const labelOf = labels(concepts.map(doc => ({ key: doc.key, title: doc.title, source: doc.path })));
	return concepts.map(doc => {
		const family = familyOf.get(parentOf(doc.path)), concept = { key: doc.key, label: labelOf.get(doc.key), title: doc.title, source: doc.path, terms: termVector(doc.title, doc.headings, doc.path), members: members.get(doc.key) };
		return family ? { ...concept, family } : concept;
	}).sort((a, b) => b.members.length - a.members.length || compare(a.key, b.key));
}
export function docConcepts(data, docs) {
	const index = catalog(data);
	const parsed = selectDocs(index, docs).map(({ path, text }) => {
		const doc = parseDoc(text), { evidence, mentions } = evidenceOf(index, doc, path);
		return { path, key: `doc:${path}`, title: doc.h1 ?? doc.front ?? fallbackTitle(path), headings: doc.headings, evidence, mentions, reach: evidence.size };
	});
	const codeCount = index.code.length, hubLimit = Math.max(hubMinimum, hubShare * codeCount);
	const hubs = parsed.filter(doc => doc.reach > hubLimit), candidates = parsed.filter(doc => doc.reach <= hubLimit && doc.reach >= minimumReach && doc.mentions >= minimumMentions);
	const { active, assigned } = mentionWinners(candidates, codeCount);
	propagate(index, assigned);
	const concepts = parsed.filter(doc => active.has(doc));
	for (const doc of concepts) assigned.set(doc.path, { key: doc.key, via: "doc" });
	inheritFolders(index, assigned);
	const assignments = [...[...assigned].map(([path, item]) => [index.files.get(path), item]), ...directoryAssignments(index, assigned)].sort((a, b) => compare(a[0], b[0]));
	const members = new Map(concepts.map(doc => [doc.key, []]));
	for (const [id, { key }] of assignments) members.get(key).push(id);
	return { concepts: describeConcepts(concepts, members), assignments: new Map(assignments), stats: { docs: parsed.length, hubs: hubs.map(doc => doc.path), files: index.paths.length, assigned: assigned.size } };
}
