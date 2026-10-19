import * as path from "node:path";
import { pathToFileURL } from "node:url";
import type { Component, DescribeContext, NativeNode, NativeUiEvent } from "@oh-my-pi/pi-tui";
import { nativeConceptCue } from "./native-concepts";
import type { NativeChild } from "@oh-my-pi/pi-tui/native/node";
import type { RenderResultContextOptions, ToolRenderer, ToolRenderResult } from "@oh-my-pi/pi-tui/tools";
import { GraphInspector, readGraph, type GraphData } from "./graph";
import { strengths } from "./graph-data/matches.js";
import { matchScoreIndicator, matchVerificationIcon } from "./match-indicators";
import { resultGraph } from "./result-graph";
import { acceptConceptOverview, conceptRevision, lookupConcept, subscribeConcepts } from "./concept-colors";
import { nativeElement, supportsElements } from "./native-elements";
import type { GraphAppearance } from "./graph-appearance";

const MAX_BYTES = 32 * 1024;
const MAX_HITS = 32;
const COMPACT_MATCHES = 6;
const OPERATIONS: Record<string, Display["kind"]> = {
	search_codebase: "search", read_source: "source", batch_lookup: "batch", sync_status: "status",
	find_definition: "batch", find_references: "batch", who_calls: "batch", implementations_of: "batch",
	search_symbols: "batch", expand_chunk: "batch", symbol_at_position: "batch",
	trace: "graph", call_graph: "graph", call_path: "graph", reaches: "graph", flows_into: "graph",
	flows_between: "graph", type_hierarchy: "graph", cycles: "graph", duplicates: "graph", unused: "graph",
	imports: "graph", symbol_edges: "graph", external_links: "graph", list_projects: "graph", file_outline: "graph",
	overview: "graph",
};
const TITLES: Record<string, string> = {
	search_codebase: "Code search", read_source: "Read source", batch_lookup: "Find symbols", sync_status: "Index status",
	find_definition: "Find definition", find_references: "Find references", who_calls: "Find callers",
	implementations_of: "Find implementations", search_symbols: "Search symbols", expand_chunk: "Expand source",
	symbol_at_position: "Inspect symbol",
};
// Raw, unsanitized MCP identities, not a prefix-based ownership test.
// Claude/agent-plugin discovery namespaces `${plugin}:${server}`; native OMP packages keep the raw server name.
const SERVERS = new Set(["semctx", "semctx:semctx"]);

type Obj = Record<string, unknown>;
interface Identity { codebaseId: string | null; name: string | null; localRoot: string | null; copy: "checkout" | "canonical" }
export interface Hit {
	path: string | null; symbol: string | null; qualifiedName: string | null; language: string | null; kind: string | null;
	score: number | null; startLine: number | null; endLine: number | null; startByte: number | null; endByte: number | null;
	snippet: string | null; snippetStartLine: number | null; snippetEndLine: number | null;
	snippetStartByte: number | null; snippetEndByte: number | null; focusLine: number | null; focusByte: number | null;
	enclosingSymbol?: string | null; isWrite?: boolean | null; referenceNamespace?: string | null;
	referenceKind?: string | null; externalTarget?: string | null; project?: string | null;
	codebaseId: string | null; binding: { state: "bound" | "changed-local" | "range-mismatch" | "unverified" | "not-probed"; reason: string | null };
}
const ROW_CAPABILITIES = ["meter", "el", "icon", "kv", "section", "code"] as const;
interface Search { kind: "search"; query: string; hits: Hit[]; indexWarning: string | null }
interface Source {
	kind: "source"; path: string; text: string; language: string | null; sourceOrigin: string;
	contentHash: string; encoding: string; binary: boolean; truncated: boolean;
	byteStart: number; byteEnd: number; fileSize: number; lineStart: number | null; lineEnd: number | null;
}
interface Batch { kind: "batch"; symbols: { symbol: string; hits: Hit[] }[] }
interface Status {
	kind: "status"; coordinator: Obj | null; catalog: { files: number; bytes: number } | null;
	catalogError: string | null; jobId: string | null; job: Obj | null; jobError: string | null;
}
export type Display = { version: 1; identity: Identity } & (Search | Source | Batch | Status | GraphData);
const object = (v: unknown): v is Obj => v !== null && typeof v === "object" && !Array.isArray(v);
const nullableString = (v: unknown): v is string | null => v === null || typeof v === "string";
const integer = (v: unknown): v is number => typeof v === "number" && Number.isSafeInteger(v);
const coordinate = (v: unknown, min: number) => v === null || (integer(v) && v >= min);
const nullableRange = (a: unknown, b: unknown, min: number) => coordinate(a, min) && coordinate(b, min) && (a === null || b === null || (a as number) <= (b as number));

/** Bound traversal before serialization, including malformed/deep/cyclic host values. */
function bounded(value: unknown, localOverview = false): boolean {
	const maxBytes = localOverview ? 3 * 1024 * 1024 : MAX_BYTES, maxNodes = localOverview ? 400_000 : 4096;
	const stack: { value: unknown; depth: number }[] = [{ value, depth: 0 }];
	const seen = new Set<object>();
	let nodes = 0, chars = 0;
	while (stack.length > 0) {
		const entry = stack.pop()!;
		if (++nodes > maxNodes || entry.depth > 16) return false;
		const v = entry.value;
		if (typeof v === "string") chars += v.length;
		else if (v !== null && typeof v === "object") {
			if (seen.has(v)) return false;
			seen.add(v);
			for (const [key, child] of Object.entries(v)) {
				chars += key.length;
				stack.push({ value: child, depth: entry.depth + 1 });
				if (stack.length > maxNodes) return false;
			}
		} else if (v !== null && typeof v !== "number" && typeof v !== "boolean") return false;
		if (chars > maxBytes) return false;
	}
	return Buffer.byteLength(JSON.stringify(value), "utf8") <= maxBytes;
}
function hit(v: unknown): v is Hit {
	if (!object(v) || !["path", "symbol", "qualifiedName", "language", "kind", "snippet", "codebaseId"].every(k => nullableString(v[k]))) return false;
	if (!(v.score === null || (typeof v.score === "number" && Number.isFinite(v.score)))) return false;
	if (!["enclosingSymbol", "referenceNamespace", "referenceKind", "externalTarget", "project"].every(k => v[k] === undefined || nullableString(v[k])) ||
		!(v.isWrite === undefined || v.isWrite === null || typeof v.isWrite === "boolean")) return false;
	if (!nullableRange(v.startLine, v.endLine, 0) || !nullableRange(v.startByte, v.endByte, 0) ||
		!nullableRange(v.snippetStartLine, v.snippetEndLine, 0) || !nullableRange(v.snippetStartByte, v.snippetEndByte, 0) ||
		!coordinate(v.focusLine, 0) || !coordinate(v.focusByte, 0)) return false;
	return object(v.binding) && ["bound", "changed-local", "range-mismatch", "unverified", "not-probed"].includes(String(v.binding.state)) && nullableString(v.binding.reason);
}
function hits(v: unknown): v is Hit[] { return Array.isArray(v) && v.length <= MAX_HITS && v.every(hit); }

/** Consume only the typed, out-of-model carrier. Never parse transcript text. */
export function readDisplay(value: unknown, operation: string, localOverview = false): Display | undefined {
	try {
		const fullOverview = localOverview && operation === "overview";
		if ((operation === "overview" && !fullOverview) || !bounded(value, fullOverview) || !object(value) || value.version !== 1 || value.kind !== OPERATIONS[operation]) return undefined;
		const id = value.identity;
		if (!object(id) || !["codebaseId", "name", "localRoot"].every(k => nullableString(id[k])) || !["checkout", "canonical"].includes(String(id.copy))) return undefined;
		if (id.copy === "canonical" && id.localRoot !== null) return undefined;
		switch (value.kind) {
			case "search": if (typeof value.query !== "string" || !hits(value.hits) || !nullableString(value.indexWarning)) return undefined; break;
			case "graph": if (!readGraph(value, hit, fullOverview)) return undefined; break;
			case "batch": {
				if (!Array.isArray(value.symbols)) return undefined;
				let count = 0;
				for (const s of value.symbols) {
					if (!object(s) || typeof s.symbol !== "string" || !hits(s.hits)) return undefined;
					count += s.hits.length;
				}
				if (count > MAX_HITS) return undefined;
				break;
			}
			case "source":
				if (!["path", "text", "sourceOrigin", "contentHash", "encoding"].every(k => typeof value[k] === "string") ||
					!nullableString(value.language) || !["content-plane", "verified-local"].includes(String(value.sourceOrigin)) ||
					typeof value.binary !== "boolean" || typeof value.truncated !== "boolean" ||
					![value.byteStart, value.byteEnd, value.fileSize].every(v => integer(v) && v >= 0) ||
					(value.byteStart as number) > (value.byteEnd as number) || (value.byteEnd as number) > (value.fileSize as number) ||
					!nullableRange(value.lineStart, value.lineEnd, 0)) return undefined;
				break;
			case "status":
				if (!(value.coordinator === null || object(value.coordinator)) || !(value.job === null || object(value.job)) ||
					!["catalogError", "jobId", "jobError"].every(k => nullableString(value[k])) ||
					!(value.catalog === null || (object(value.catalog) && integer(value.catalog.files) && value.catalog.files >= 0 && integer(value.catalog.bytes)))) return undefined;
				if (object(value.job) && (!integer(value.job.filesEmbedded) || !integer(value.job.filesFailed) ||
					!["error", "startedAt", "completedAt"].every(k => nullableString((value.job as Obj)[k])) ||
					!["filesToEmbed", "filesToDelete", "filesDeleted", "chunkCount"].every(k => value.job && ((value.job as Obj)[k] === undefined || integer((value.job as Obj)[k]))))) return undefined;
				break;
			default: return undefined;
		}
		if (fullOverview) acceptConceptOverview(value as unknown as Display);
		return value as unknown as Display;
	} catch { return undefined; }
}

const clean = (s: string) => s.replace(/[\x00-\x08\x0b-\x1f\x7f-\x9f]/g, "");
const label = (s: string) => clean(s).replace(/[\r\n\t]/g, " ");
const text = (s: string, p: Record<string, unknown> = {}): Extract<NativeNode, { k: "text" }> => ({
	k: "text",
	p: { ...(typeof p.tone === "string" ? { spans: [{ t: label(s), s: p.tone }] } : { text: label(s) }), ...p },
});

function lineCount(s: string): number { return s.length === 0 ? 0 : s.split("\n").length - (s.endsWith("\n") ? 1 : 0); }
/** Marks describe the returned snippet, never a verified jump into the current checkout. */
export function snippetWindow(h: Hit): { start: number; end: number; focus?: number; mark?: readonly [number, number] } | undefined {
	if (h.snippet === null || h.snippetStartLine === null || h.snippetStartLine < 1 || h.snippetEndLine === null ||
		lineCount(h.snippet) !== h.snippetEndLine - h.snippetStartLine + 1) return undefined;
	const focus = h.focusLine !== null && h.focusLine >= h.snippetStartLine && h.focusLine <= h.snippetEndLine &&
		(h.focusByte === null || h.snippetStartByte === null || h.snippetEndByte === null ||
			(h.focusByte >= h.snippetStartByte && h.focusByte < h.snippetEndByte)) ? h.focusLine : undefined;
	const range = h.focusLine === null && h.startLine !== null && h.startLine > 0 && h.endLine !== null && h.endLine >= h.startLine
		? [Math.max(h.startLine, h.snippetStartLine), Math.min(h.endLine, h.snippetEndLine)] as const : undefined;
	return { start: h.snippetStartLine, end: h.snippetEndLine, focus,
		mark: focus !== undefined ? [focus, focus] : range && range[0] <= range[1] ? range : undefined };
}
const UNVERIFIED_FILE_LINK = "Open current checkout file · indexed location unverified";
function sourceLink(id: Identity, h: Hit): string | undefined {
	if (id.copy !== "checkout" || id.localRoot === null || !path.isAbsolute(id.localRoot) || !h.path ||
		!id.codebaseId || h.codebaseId !== id.codebaseId || path.isAbsolute(h.path) || /^[a-z][a-z0-9+.-]*:/i.test(h.path)) return undefined;
	const root = path.resolve(id.localRoot), file = path.resolve(root, h.path);
	const rel = path.relative(root, file);
	if (!rel || rel === ".." || rel.startsWith(`..${path.sep}`) || path.isAbsolute(rel)) return undefined;
	return pathToFileURL(file).href;
}

function location(h: Hit): string {
	return `${h.path ?? "Path unavailable"}${h.startLine !== null && h.startLine > 0 ? `:${h.startLine}${h.endLine !== null && h.endLine !== h.startLine ? `–${h.endLine}` : ""}` : ""}`;
}

export class SemctlInspector implements Component {
	private readonly expandedHits = new Set<number>();
	private readonly expandedSections = new Set<string>();
	private readonly graph?: GraphInspector;
	private readonly supplementaryGraph: boolean;
	private comparisonHits?: readonly Hit[];
	private readonly hits: Hit[];
	private rowsCache: { mask: number; dark: boolean; revision: number; comparison: readonly Hit[]; nodes: NativeNode[] } | undefined;
	private static repaintConcepts(owner: SemctlInspector): void { owner.requestRender(); }
	constructor(
		readonly display: Display,
		private readonly requestRender: () => void,
		private readonly fallbackBody: () => readonly NativeChild[] = () => [],
		private readonly fallbackText = "",
		graphSummary: "header" | "body" = "body",
		presentation: "inline" | "overview" = "inline",
		private readonly expandedResult: () => boolean = () => false,
	) {
		this.hits = display.kind === "search" ? display.hits : display.kind === "batch" ? display.symbols.flatMap(group => group.hits) :
			display.kind === "graph" ? display.nodes.flatMap(node => node.hit ? [node.hit] : []) : [];
		subscribeConcepts(this, SemctlInspector.repaintConcepts);
		const graph = display.kind === "graph" ? display : resultGraph(display);
		this.supplementaryGraph = display.kind !== "graph" && graph !== undefined;
		if (graph) this.graph = new GraphInspector(graph, requestRender, hit => {
			const preview = new SemctlInspector({ version: 1, identity: display.identity, kind: "search",
				query: "", hits: [hit], indexWarning: null }, requestRender, undefined, undefined, "body", "inline", this.expandedResult);
			// A graph selection is still ranked against its original result, not a one-hit query.
			preview.comparisonHits = this.hits;
			preview.expandedHits.add(0);
			return preview;
		}, display.identity.codebaseId, this.hits, this.supplementaryGraph ? "body" : graphSummary, presentation);
	}
	invalidate(): void { this.rowsCache = undefined; this.graph?.invalidate(); }
	move(delta: number): void { this.graph?.move(delta); }
	setActivity(targets: readonly string[]): void { this.graph?.setActivity(targets); }
	setAppearance(appearance: Readonly<GraphAppearance>): void { this.graph?.setAppearance(appearance); }
	handleNativeEvent(event: NativeUiEvent): void {
		if (event.type === "toggle" && event.key !== "") {
			const key = event.key.slice(event.key.lastIndexOf("/") + 1);
			if (event.collapsed) this.expandedSections.delete(key);
			else this.expandedSections.add(key);
		} else if (event.type === "action" && event.act === "graph") {
			if (this.expandedSections.has("graph")) this.expandedSections.delete("graph");
			else this.expandedSections.add("graph");
		} else if (event.type === "action" && event.act.startsWith("hit:")) {
			const n = Number(event.act.slice(4));
			const d = this.display;
			const count = d.kind === "search" ? d.hits.length : d.kind === "batch" ? d.symbols.reduce((sum, s) => sum + s.hits.length, 0) : 0;
			if (Number.isSafeInteger(n) && n >= 0 && n < count) {
				if (this.expandedHits.has(n)) this.expandedHits.delete(n);
				else this.expandedHits.add(n);
			}
		} else return;
		this.rowsCache = undefined;
		this.requestRender();
	}
	private section(key: string, head: string, children: NativeNode[], cx: DescribeContext): NativeNode[] {
		if (!cx.supports("section")) return children;
		return [{ k: "section", key, p: { head, collapsible: true, collapsed: !this.expandedSections.has(key) }, c: children }];
	}
	private preview(h: Hit, cx: DescribeContext): NativeNode[] {
		const w = snippetWindow(h);
		const out: NativeNode[] = [];
		if (h.snippet) out.push(...this.sourceNodes(h.snippet, { path: h.path ?? undefined, language: h.language ?? undefined,
			start: w?.start, focus: w?.focus ?? w?.mark?.[0], mark: w?.mark,
			limit: this.expandedResult() ? Math.max(12, lineCount(h.snippet)) : 12 }, cx));
		else out.push(text("No source preview in this result.", { tone: "muted" }));
		return out;
	}
	private matchDetails(d: Display, h: Hit, index: number, cx: DescribeContext): NativeNode[] {
		const items = [
			{ k: "Location", v: location(h) },
			{ k: "Symbol", v: h.qualifiedName ?? h.symbol ?? "Unavailable" },
		];
		if (h.enclosingSymbol) items.push({ k: "Enclosing symbol", v: h.enclosingSymbol });
		if (typeof h.isWrite === "boolean") items.push({ k: "Access", v: h.isWrite ? "write" : "read" });
		if (h.referenceNamespace) items.push({ k: "Namespace", v: h.referenceNamespace });
		if (h.referenceKind) items.push({ k: "Reference kind", v: h.referenceKind });
		if (h.externalTarget) items.push({ k: "External target", v: h.externalTarget });
		if (h.project) items.push({ k: "Project", v: h.project });
		items.push({ k: "Query codebase", v: `${d.identity.codebaseId ?? "Unknown"} · ${d.identity.copy}` });
		if (h.codebaseId && h.codebaseId !== d.identity.codebaseId) items.push({ k: "Hit codebase", v: h.codebaseId });
		if (d.identity.localRoot) items.push({ k: "Checkout", v: d.identity.localRoot });
		const details: NativeNode[] = cx.supports("kv")
			? [{ k: "kv", p: { items: items.map(item => ({ k: item.k, v: clean(item.v) })) } }]
			: items.map(item => text(`${item.k}: ${item.v}`));
		return this.section(`match-details-${index}`, "Details", details, cx);
	}
	private sourceNodes(source: string, info: { path?: string; language?: string; start?: number; focus?: number; mark?: readonly [number, number]; limit: number }, cx: DescribeContext): NativeNode[] {
		// The host owns full-result expansion; each open match keeps a bounded source window.
		const lines = source.match(/[^\n]*\n|[^\n]+$/g) ?? [];
		const focusIndex = info.start !== undefined && info.focus !== undefined ? info.focus - info.start : 0;
		const offset = lines.length > info.limit
			? Math.max(0, Math.min(lines.length - info.limit, focusIndex - Math.floor(info.limit / 2))) : 0;
		// A final line terminator is not another source line. Preserve intentional blank lines.
		const shown = lines.slice(offset, offset + info.limit).join("").replace(/\r?\n$/, "");
		const start = info.start !== undefined ? info.start + offset : undefined;
		const marks: { line: number; tone: "accent" }[] = [];
		if (start !== undefined && info.mark) {
			const end = Math.min(info.mark[1], start + Math.min(info.limit, lines.length - offset) - 1);
			for (let line = Math.max(start, info.mark[0]); line <= end; line++) marks.push({ line, tone: "accent" });
		}
		const result: NativeNode[] = [cx.supports("code") ? {
			k: "code", key: "source", p: { text: clean(shown), path: info.path, lang: info.language, start,
				numbers: start !== undefined, wrap: false, marks: marks.length ? marks : undefined },
		} : { k: "text", p: { text: clean(shown), wrap: "none" } }];
		if (lines.length > info.limit) result.push(text(`${info.limit} of ${lines.length} lines · expand tool for full result`, { tone: "muted" }));
		return result;
	}
	private hitRows(d: Display, rows: Hit[], cx: DescribeContext): NativeNode[] {
		const compared = this.comparisonHits ?? rows;
		let mask = (Number(cx.feature("styles")) << ROW_CAPABILITIES.length) |
			(Number(cx.reduceMotion) << (ROW_CAPABILITIES.length + 1)) | (Number(this.expandedResult()) << (ROW_CAPABILITIES.length + 2));
		for (const [index, kind] of ROW_CAPABILITIES.entries()) if (kind === "el" ? supportsElements(cx) : cx.supports(kind)) mask |= 1 << index;
		const revision = conceptRevision(), cached = this.rowsCache;
		if (cached && cached.mask === mask && cached.dark === cx.dark && cached.revision === revision && cached.comparison === compared) return cached.nodes;
		const scales = strengths(compared);
		const result = rows.map((h, i): NativeNode => {
			const expanded = this.expandedHits.has(i);
			const rank = this.comparisonHits ? compared.indexOf(h) : i;
			const strength = scales[rank];
			const indicator = matchScoreIndicator(h, rank, strength ?? null, cx, `hit:${i}`);
			const tip = `${h.qualifiedName ?? h.symbol ?? h.kind ?? "Symbol unavailable"}${h.enclosingSymbol ? `\nIn ${h.enclosingSymbol}` : ""}\n${expanded ? "Collapse" : "Expand"} source`;
			const file = label(h.path ?? "Path unavailable");
			const symbol = h.symbol ?? h.qualifiedName ?? h.kind ?? "—";
			const concept = lookupConcept(h.codebaseId ?? d.identity.codebaseId, h.path);
			const cue = concept && nativeConceptCue(concept, cx, expanded);
			const context = h.enclosingSymbol ? `${symbol} in ${h.enclosingSymbol}` : symbol;
			const cut = Math.max(file.lastIndexOf("/"), file.lastIndexOf("\\")) + 1, href = sourceLink(d.identity, h);
			const filename: NativeNode = { k: "text", p: { role: "semctl.match.path", spans: [
				{ t: file.slice(0, cut), s: "dim" },
				{ t: file.slice(cut), s: "strong", ...(href ? { href } : {}) },
				{ t: label(location(h).slice((h.path ?? "Path unavailable").length)), s: "muted" },
			], shrink: 1, min: { w: 0 }, wrap: "none", truncate: "middle",
				title: `${location(h)}${href ? `\n${h.binding.state === "bound" ? "Open checkout file" : UNVERIFIED_FILE_LINK}` : ""}` } };
			const chip: NativeNode = { k: "row", key: `hit-${i}`, p: { role: "omp.tool.chip", gap: "xs", align: "center", wrap: false, mark: expanded ? "pick" : undefined, max: { w: 1 }, actions: { click: `hit:${i}` }, title: tip, aria: `${expanded ? "Collapse" : "Expand"} match ${i + 1}: ${location(h)}. ${label(context)}` }, c: [
				cue ? nativeElement({ ...cue, k: "el", p: { ...cue.p, actions: { click: `hit:${i}` } } }) : text(expanded ? "▾" : "▸", { tone: "muted", wrap: "none", shrink: 0 }),
				...(cx.supports("icon") ? [{ k: "icon" as const, key: "file-icon", p: { name: "file", shrink: 0, aria: file } }] : []),
				filename,
				...(indicator ? [indicator] : []),
				matchVerificationIcon(h, cx, `hit:${i}`),
			] };
			return { k: "col", key: `match-${i}`, p: { role: expanded ? "semctl.match.expanded" : "semctl.match",
				basis: expanded ? undefined : "content", grow: 0, shrink: 1, min: { w: 0 }, max: { w: 1 },
				hidden: !this.expandedResult() && i >= COMPACT_MATCHES }, c: [
				chip,
				...(expanded ? [{ k: "col" as const, key: "preview", p: { role: "semctl.match.preview", gap: "xs" as const }, c: [
					text(context, { tone: "muted", wrap: "none", truncate: "end", title: [
						h.qualifiedName ?? h.symbol, h.enclosingSymbol ? `In ${h.enclosingSymbol}` : null, h.project ? `Project: ${h.project}` : null,
					].filter(Boolean).join("\n") }),
					...this.preview(h, cx), ...this.matchDetails(d, h, i, cx),
				] }] : []),
			] };
		});
		this.rowsCache = { mask, dark: cx.dark, revision, comparison: compared, nodes: result };
		return result;
	}
	describe(cx: DescribeContext): NativeNode | null {
		if (!(["col", "row", "text"] as const).every(k => cx.supports(k))) {
			return { k: "col", c: [...this.fallbackBody()] };
		}
		const children: NativeNode[] = [];
		const d = this.display;
		const id = d.identity;
		const displayName = id.name ?? (id.localRoot !== null && path.isAbsolute(id.localRoot) ? path.basename(id.localRoot) || "checkout" : "Codebase");
		const identity = text(`${id.copy} · ${displayName}`, { tone: "muted", title: `Codebase: ${id.codebaseId ?? "unbound"}${id.localRoot ? `\nCheckout: ${id.localRoot}` : ""}` });
		const freshness = d.kind === "search" && d.indexWarning ? {
			role: "semctl.index-warning", tone: "warning" as const, shrink: 0, title: d.indexWarning, aria: `Index warning: ${d.indexWarning}`,
		} : undefined;
		const warning: NativeNode | undefined = freshness ? cx.supports("icon")
			? { k: "icon", key: "index-warning", p: { name: "warn", ...freshness } }
			: { ...text("!", freshness), key: "index-warning" } : undefined;
		const graphOpen = this.expandedSections.has("graph");
		const graphAction = { actions: { click: "graph" }, title: `${graphOpen ? "Hide" : "Show"} result graph\nFile membership, not calls or dependencies.`,
			aria: graphOpen ? "Hide result graph" : "Show result graph", shrink: 0 };
		const graphControl: NativeNode = supportsElements(cx) && cx.feature("styles")
			? nativeElement({ k: "el", key: "graph-control", p: { tag: "button", class: "semctl-icon-action", ...graphAction,
				attrs: { "aria-pressed": graphOpen, "aria-expanded": graphOpen } }, c: [cx.supports("icon") ? { k: "icon", p: { name: "diagram" } } : text("Graph")] })
			: cx.supports("icon") ? { k: "icon", key: "graph-control", p: { name: "diagram", ...graphAction } }
				: { ...text("Graph", { ...graphAction, tone: "accent" }), key: "graph-control" };
		children.push(this.supplementaryGraph || warning ? {
			k: "row", p: { gap: "sm", align: "center" }, c: [
				{ ...identity, p: { ...identity.p, grow: 1, min: { w: 0 }, truncate: "end", wrap: "none" } },
				...(warning ? [warning] : []), ...(this.supplementaryGraph ? [graphControl] : []),
			],
		} : identity);
		if (d.kind === "graph") return { k: "col", p: { gap: "sm" }, c: [identity, this.graph!] };
		if (this.supplementaryGraph && this.expandedSections.has("graph")) children.push({
			k: "col", key: "result-graph", p: { gap: "xs" }, c: [text("Matches by file", { tone: "muted" }), this.graph!],
		});
		if (d.kind === "search" || d.kind === "batch") {
			const rows = this.hits;
			const rendered = this.hitRows(d, rows, cx);
			if (d.kind === "search" && rows.length === 0) {
				children.push(text("No matches"), text("Try a symbol name or a shorter description.", { tone: "muted" }));
			}
			const entries: NativeNode[] = [];
			if (d.kind === "batch") {
				let offset = 0;
				for (const [index, s] of d.symbols.entries()) {
					if (d.symbols.length > 1 || s.hits.length === 0) {
						entries.push({ ...text(`${s.symbol} · ${s.hits.length ? `${s.hits.length} matches` : "no match"}`, {
							role: "semctl.batch.heading", basis: "100%", tone: "muted", wrap: "none", truncate: "end",
							hidden: !this.expandedResult() && (s.hits.length ? offset : index) >= COMPACT_MATCHES,
						}),
							key: `${s.hits.length ? "batch-group" : "batch-miss"}-${index}` });
					}
					entries.push(...rendered.slice(offset, offset + s.hits.length)); offset += s.hits.length;
				}
			} else entries.push(...rendered);
			// Keep stable match nodes and selection while the native tool header
			// switches between the compact preview and the complete result.
			children.push({ k: "row", key: "matches", p: { role: "semctl.matches", gap: "xs", wrap: true, align: "start" }, c: entries });
		} else if (d.kind === "source") {
			const header = text(`${d.path} · ${d.sourceOrigin} · bytes ${d.byteStart}–${d.byteEnd}/${d.fileSize}${d.truncated ? " · truncated" : ""}${d.binary ? " · binary" : ""}`, { title: `Hash: ${d.contentHash}\nEncoding: ${d.encoding}` });
			const concept = lookupConcept(id.codebaseId, d.path), cue = concept && nativeConceptCue(concept, cx);
			children.push(cue ? { k: "row", p: { gap: "sm", align: "center" }, c: [cue, header] } : header);
			const numbered = d.lineStart !== null && d.lineStart > 0 && d.lineEnd !== null && lineCount(d.text) === d.lineEnd - d.lineStart + 1;
			children.push(...this.sourceNodes(d.text, { path: d.path, language: d.language ?? undefined, start: numbered ? d.lineStart! : undefined,
				limit: this.expandedResult() ? Math.max(16, lineCount(d.text)) : 16 }, cx));
		} else children.push(...this.status(d, cx));
		return { k: "col", p: { gap: "sm" }, c: children };
	}
	private status(d: Status, cx: DescribeContext): NativeNode[] {
		const out: NativeNode[] = [text("Index snapshot · not live progress", { tone: "muted" })];
		if (d.catalog) out.push(text(`Catalog: ${d.catalog.files} files · ${d.catalog.bytes} bytes`));
		if (d.coordinator) {
			const c = d.coordinator;
			if (typeof c.running === "boolean") out.push(text(`Coordinator: ${c.running ? "running" : "idle"}`));
			if (typeof c.last_error === "string") out.push(text(`Last indexing error: ${c.last_error}`, { tone: "error" }));
		}
		if (d.job) out.push(text(`Job ${d.jobId ?? "unknown"}: ${d.job.filesEmbedded} files embedded · ${d.job.filesFailed} failed${d.job.filesToEmbed !== undefined ? ` · planned ${d.job.filesToEmbed}` : ""}`));
		for (const e of [d.catalogError, d.jobError, d.job?.error]) if (typeof e === "string") out.push(text(e, { tone: "error" }));
		const details: NativeNode = cx.supports("code") ? { k: "code", p: { text: clean(JSON.stringify({ coordinator: d.coordinator, catalog: d.catalog, jobId: d.jobId, job: d.job }, null, 2)), lang: "json", numbers: false, preview: { lines: 12 } } } : text(JSON.stringify(d));
		if (cx.supports("section")) out.push({ k: "section", key: "snapshot", p: { head: "Snapshot details", collapsible: true, collapsed: !this.expandedSections.has("snapshot") }, c: [details] });
		else out.push(details);
		return out;
	}
	render(width: number): readonly string[] {
		return clean(this.fallbackText).split(/\r?\n/).map(line => line.slice(0, Math.max(0, Math.floor(width))));
	}
}

/** One tool can render many retained cards. MCP details own each response's local UI state. */
export function createInspectorRenderer(server: string, operation: string) {
	return ({ requestRender, fallback }: { requestRender(): void; fallback: ToolRenderer }): ToolRenderer => {
		interface State {
			view?: SemctlInspector;
			result: ToolRenderResult;
			options: RenderResultContextOptions;
			args?: unknown;
		}
		const states = new WeakMap<object, State>();
		return {
			...fallback,
			mergeCallAndResult: true,
			describeCall(args, options) {
				const original = fallback.describeCall?.(args, options);
				if (operation !== "search_codebase" || !object(args) || typeof args.query !== "string") return original;
				return { ...original, tool: { ...original?.tool, title: TITLES[operation], target: clean(args.query), targetKind: "query" } };
			},
			describeResult(result, options, args) {
				const details = result.details;
				if (result.isError || !object(details) || details.isError === true || details.serverName !== server ||
					details.mcpToolName !== operation || !object(details.mcpMeta) || options.isPartial || options.cancelled) {
					return fallback.describeResult?.(result, options, args);
				}
				const display = readDisplay(details.mcpMeta.semctlDisplay, operation);
				if (!display) return fallback.describeResult?.(result, options, args);
				let state = states.get(details);
				if (!state) { state = { result, options, args }; states.set(details, state); }
				else { state.result = result; state.options = options; state.args = args; }
				const current = state;
				if (current.view?.display !== display) {
					current.view = new SemctlInspector(display, requestRender,
						() => fallback.describeResult?.(current.result, current.options, current.args)?.body ?? [],
						result.content.filter(block => block.type === "text").map(block => block.text ?? "").join("\n\n"),
						"header", "inline", () => current.options.expanded);
				}
				const count = display.kind === "search" ? display.hits.length :
					display.kind === "batch" ? display.symbols.reduce((sum, s) => sum + s.hits.length, 0) : undefined;
				return {
					tool: { title: display.kind === "graph" ? clean(display.title) : TITLES[operation] ?? `${server}/${operation}`,
						target: display.kind === "graph" ? clean(display.query ?? "") :
							display.kind === "search" ? clean(display.query) : display.kind === "source" ? display.path :
							display.kind === "batch" && display.symbols.length === 1 ? clean(display.symbols[0].symbol) : undefined,
						targetKind: display.kind === "source" || operation === "expand_chunk" ? "path" :
							display.kind === "search" || operation === "search_symbols" ? "query" : "text",
						meta: display.kind === "graph" ? [`${display.nodes.length} nodes`, `${display.edges.length} links`,
							...(display.truncated ? ["partial"] : [])] : count === undefined ? undefined : [
								`${!options.expanded && count > COMPACT_MATCHES ? `${COMPACT_MATCHES} of ` : ""}${count} ${count === 1 ? "match" : "matches"}`,
							] },
					body: [current.view],
					inline: true,
					preview: "children",
				};
			},
		};
	};
}

/** Match raw MCP identities. Similar names from other servers never acquire semctl rendering. */
export function supportsInspector(server: string, operation: string): boolean {
	return SERVERS.has(server) && Object.hasOwn(OPERATIONS, operation);
}
