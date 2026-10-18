import * as path from "node:path";
import { pathToFileURL } from "node:url";
import type { ExtensionAPI } from "@oh-my-pi/pi-coding-agent";
import type { Component, DescribeContext, NativeNode, NativeUiEvent } from "@oh-my-pi/pi-tui";
import type { NativeChild } from "@oh-my-pi/pi-tui/native/node";
import type { ToolRendererFactory } from "@oh-my-pi/pi-tui/tools";

const MAX_BYTES = 32 * 1024;
const MAX_HITS = 32;
const OPERATIONS: Record<string, Display["kind"]> = {
	search_codebase: "search", read_source: "source", batch_lookup: "batch", sync_status: "status",
};
// Raw, unsanitized MCP identities, not a prefix-based ownership test.
// Claude/agent-plugin discovery namespaces `${plugin}:${server}`; native OMP packages keep the raw server name.
const SERVERS: Record<string, string> = { semctx: "mcp__semctx_", "semctx:semctx": "mcp__semctx_semctx_" };

type Obj = Record<string, unknown>;
interface Identity { codebaseId: string | null; name: string | null; localRoot: string | null; copy: "checkout" | "canonical" }
export interface Hit {
	path: string | null; symbol: string | null; qualifiedName: string | null; language: string | null; kind: string | null;
	score: number | null; startLine: number | null; endLine: number | null; startByte: number | null; endByte: number | null;
	snippet: string | null; snippetStartLine: number | null; snippetEndLine: number | null;
	snippetStartByte: number | null; snippetEndByte: number | null; focusLine: number | null; focusByte: number | null;
	codebaseId: string | null; binding: { state: "bound" | "changed-local" | "range-mismatch" | "unverified" | "not-probed"; reason: string | null };
}
const VERIFICATION: Record<Hit["binding"]["state"], { name: string; tone: "success" | "warning" | "error" | "muted" }> = {
	bound: { name: "check", tone: "success" },
	"changed-local": { name: "warn", tone: "warning" },
	"range-mismatch": { name: "x-circle", tone: "error" },
	unverified: { name: "info", tone: "muted" },
	"not-probed": { name: "info", tone: "muted" },
};
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
export type Display = { version: 1; identity: Identity } & (Search | Source | Batch | Status);
const object = (v: unknown): v is Obj => v !== null && typeof v === "object" && !Array.isArray(v);
const nullableString = (v: unknown): v is string | null => v === null || typeof v === "string";
const integer = (v: unknown): v is number => typeof v === "number" && Number.isSafeInteger(v);
const coordinate = (v: unknown, min: number) => v === null || (integer(v) && v >= min);
const nullableRange = (a: unknown, b: unknown, min: number) => coordinate(a, min) && coordinate(b, min) && (a === null || b === null || (a as number) <= (b as number));

/** Bound traversal before serialization, including malformed/deep/cyclic host values. */
function bounded(value: unknown): boolean {
	const stack: { value: unknown; depth: number }[] = [{ value, depth: 0 }];
	const seen = new Set<object>();
	let nodes = 0, chars = 0;
	while (stack.length > 0) {
		const entry = stack.pop()!;
		if (++nodes > 4096 || entry.depth > 16) return false;
		const v = entry.value;
		if (typeof v === "string") chars += v.length;
		else if (v !== null && typeof v === "object") {
			if (seen.has(v)) return false;
			seen.add(v);
			for (const [key, child] of Object.entries(v)) {
				chars += key.length;
				stack.push({ value: child, depth: entry.depth + 1 });
				if (stack.length > 4096) return false;
			}
		} else if (v !== null && typeof v !== "number" && typeof v !== "boolean") return false;
		if (chars > MAX_BYTES) return false;
	}
	return Buffer.byteLength(JSON.stringify(value), "utf8") <= MAX_BYTES;
}
function hit(v: unknown): v is Hit {
	if (!object(v) || !["path", "symbol", "qualifiedName", "language", "kind", "snippet", "codebaseId"].every(k => nullableString(v[k]))) return false;
	if (!(v.score === null || (typeof v.score === "number" && Number.isFinite(v.score)))) return false;
	if (!nullableRange(v.startLine, v.endLine, 0) || !nullableRange(v.startByte, v.endByte, 0) ||
		!nullableRange(v.snippetStartLine, v.snippetEndLine, 0) || !nullableRange(v.snippetStartByte, v.snippetEndByte, 0) ||
		!coordinate(v.focusLine, 0) || !coordinate(v.focusByte, 0)) return false;
	return object(v.binding) && ["bound", "changed-local", "range-mismatch", "unverified", "not-probed"].includes(String(v.binding.state)) && nullableString(v.binding.reason);
}
function hits(v: unknown): v is Hit[] { return Array.isArray(v) && v.length <= MAX_HITS && v.every(hit); }

/** Consume only the typed, out-of-model carrier. Never parse transcript text. */
export function readDisplay(value: unknown, operation: string): Display | undefined {
	try {
		if (!bounded(value) || !object(value) || value.version !== 1 || value.kind !== OPERATIONS[operation]) return undefined;
		const id = value.identity;
		if (!object(id) || !["codebaseId", "name", "localRoot"].every(k => nullableString(id[k])) || !["checkout", "canonical"].includes(String(id.copy))) return undefined;
		if (id.copy === "canonical" && id.localRoot !== null) return undefined;
		switch (value.kind) {
			case "search": if (typeof value.query !== "string" || !hits(value.hits) || !nullableString(value.indexWarning)) return undefined; break;
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
		return value as unknown as Display;
	} catch { return undefined; }
}

/** Query-relative visual strength only; never a validity/confidence score. */
export function strengths(rows: readonly Hit[]): (number | null)[] {
	if (rows.some(h => h.score === null || !Number.isFinite(h.score) || h.score < 0)) return rows.map(() => null);
	const max = Math.max(0, ...rows.map(h => h.score!));
	return rows.map(h => max > 0 ? h.score! / max : null);
}
const clean = (s: string) => s.replace(/[\x00-\x08\x0b-\x1f\x7f-\x9f]/g, "");
const label = (s: string) => clean(s).replace(/[\r\n\t]/g, " ");
const text = (s: string, p: Record<string, unknown> = {}): NativeNode => ({ k: "text", p: { text: label(s), ...p } });

function lineCount(s: string): number { return s.length === 0 ? 0 : s.split("\n").length - (s.endsWith("\n") ? 1 : 0); }
export function snippetWindow(h: Hit): { start: number; end: number; focus?: number } | undefined {
	if (h.snippet === null || h.snippetStartLine === null || h.snippetStartLine < 1 || h.snippetEndLine === null ||
		lineCount(h.snippet) !== h.snippetEndLine - h.snippetStartLine + 1) return undefined;
	return { start: h.snippetStartLine, end: h.snippetEndLine,
		focus: h.binding.state === "bound" && h.focusLine !== null && h.focusLine >= h.snippetStartLine && h.focusLine <= h.snippetEndLine &&
			(h.focusByte === null || h.snippetStartByte === null || h.snippetEndByte === null ||
				(h.focusByte >= h.snippetStartByte && h.focusByte < h.snippetEndByte)) ? h.focusLine : undefined };
}
function sourceLink(id: Identity, h: Hit): string | undefined {
	if (id.copy !== "checkout" || id.localRoot === null || !path.isAbsolute(id.localRoot) || h.path === null || h.binding.state !== "bound" ||
		h.codebaseId !== id.codebaseId || path.isAbsolute(h.path) || /^[a-z][a-z0-9+.-]*:/i.test(h.path)) return undefined;
	const root = path.resolve(id.localRoot), file = path.resolve(root, h.path);
	const rel = path.relative(root, file);
	if (rel === ".." || rel.startsWith(`..${path.sep}`) || path.isAbsolute(rel)) return undefined;
	return pathToFileURL(file).href;
}

function verificationIcon(h: Hit, cx: DescribeContext): NativeNode {
	const status = VERIFICATION[h.binding.state];
	const scope = h.binding.state === "bound"
		? "Source bytes and coordinates verified; semantic symbol/focus identity is not verified."
		: "Source bytes and coordinates are not verified; semantic symbol/focus identity is not verified.";
	const title = `${h.binding.state} · ${scope}${h.binding.reason ? ` ${h.binding.reason}` : ""}`;
	return cx.supports("icon")
		? { k: "icon", p: { ...status, title, aria: `Source ${h.binding.state}`, shrink: 0 } }
		: text(h.binding.state, { tone: status.tone, title, shrink: 0 });
}


export class SemctlInspector implements Component {
	private hitIndex = 0;
	private matchesExpanded = false;
	private readonly expandedSections = new Set<string>();
	constructor(
		readonly display: Display,
		private readonly requestRender: () => void,
		private readonly fallbackBody: () => readonly NativeChild[] = () => [],
		private readonly fallbackText = "",
	) {}
	invalidate(): void {}
	handleNativeEvent(event: NativeUiEvent): void {
		if (event.type === "toggle" && event.key !== "") {
			if (event.collapsed) this.expandedSections.delete(event.key);
			else this.expandedSections.add(event.key);
		}
		else if (event.type === "action" && event.act.startsWith("hit:")) {
			const n = Number(event.act.slice(4));
			const d = this.display;
			const count = d.kind === "search" ? d.hits.length : d.kind === "batch" ? d.symbols.reduce((sum, s) => sum + s.hits.length, 0) : 0;
			if (Number.isSafeInteger(n) && n >= 0 && n < count) this.hitIndex = n;
		} else if (event.type === "action" && event.key === "matches-control" &&
			(event.act === "matches:more" || event.act === "matches:less")) {
			this.matchesExpanded = event.act === "matches:more";
		} else return;
		this.requestRender();
	}
	private preview(h: Hit, cx: DescribeContext): NativeNode[] {
		const w = snippetWindow(h);
		const out: NativeNode[] = [{ k: "row", p: { gap: "xs", align: "center" }, c: [
			verificationIcon(h, cx),
			text(`${h.path ?? "Path unavailable"}${w?.focus ? ` · indexed focus L${w.focus}` : ""}`, { tone: "muted" }),
		] }];
		if (h.snippet !== null) out.push(...this.sourceNodes(h.snippet, { path: h.path ?? undefined, language: h.language ?? undefined,
			start: w?.start, focus: w?.focus, limit: 12 }, cx));
		else out.push(text("Source context unavailable in this result; no additional source was fetched.", { tone: "muted" }));
		return out;
	}
	private sourceNodes(source: string, info: { path?: string; language?: string; start?: number; focus?: number; limit: number }, cx: DescribeContext): NativeNode[] {
		// The host owns full-result expansion; keep only the selected source window here.
		const lines = source.match(/[^\n]*\n|[^\n]+$/g) ?? [];
		const focusIndex = info.start !== undefined && info.focus !== undefined ? info.focus - info.start : 0;
		const offset = lines.length > info.limit
			? Math.max(0, Math.min(lines.length - info.limit, focusIndex - Math.floor(info.limit / 2))) : 0;
		// A final line terminator is not another source line. Preserve intentional blank lines.
		const shown = lines.slice(offset, offset + info.limit).join("").replace(/\r?\n$/, "");
		const start = info.start !== undefined ? info.start + offset : undefined;
		const focus = start !== undefined && info.focus !== undefined && info.focus >= start &&
			info.focus < start + Math.min(info.limit, lines.length) ? info.focus : undefined;
		const result: NativeNode[] = [cx.supports("code") ? {
			k: "code", key: "source", p: { text: clean(shown), path: info.path, lang: info.language, start,
				numbers: start !== undefined, wrap: false, marks: focus !== undefined ? [{ line: focus, tone: "accent" }] : undefined },
		} : { k: "text", p: { text: clean(shown), wrap: "none" } }];
		if (lines.length > info.limit) result.push(text(`${info.limit} of ${lines.length} lines · expand tool for full result`, { tone: "muted" }));
		return result;
	}
	private hitRows(d: Display, rows: Hit[], cx: DescribeContext): NativeNode[] {
		const scale = strengths(rows);
		return rows.map((h, i): NativeNode => {
			const score = h.score === null ? "unavailable" : String(h.score);
			const tip = `Relative match score · rank ${i + 1} · raw hybrid score ${score} · query-relative, not confidence${scale[i] === null ? " · scale unavailable" : ""}`;
			const href = sourceLink(d.identity, h);
			return { k: "row", key: `hit-${i}`, p: { gap: "sm", align: "center", wrap: false, mark: i === this.hitIndex ? "pick" : undefined, actions: { click: `hit:${i}` }, title: tip }, c: [
				text(String(i + 1).padStart(2), { min: { w: "3ch" }, max: { w: "3ch" }, shrink: 0, wrap: "none", tone: "muted" }),
				cx.supports("meter") && scale[i] !== null ? { k: "meter", p: { value: scale[i]!, style: "bar", size: "sm", tone: scale[i]! >= 0.75 ? "accent" : "info", title: tip, aria: tip, min: { w: "8ch" }, max: { w: "8ch" }, shrink: 0 } } : text(scale[i] === null ? "—" : String(h.score), { title: tip, aria: tip, tone: "muted", min: { w: "8ch" }, max: { w: "8ch" }, shrink: 0, wrap: "none", truncate: "end" }),
				{ k: "row", p: { gap: "xs", align: "center", wrap: false, grow: 3, basis: 0, min: { w: 0 } }, c: [
					verificationIcon(h, cx),
					text(`${h.path ?? "Path unavailable"}${h.startLine !== null && h.startLine > 0 ? `:${h.startLine}${h.endLine !== null && h.endLine !== h.startLine ? `–${h.endLine}` : ""}` : ""}`, { grow: 1, basis: 0, min: { w: 0 }, wrap: "none", truncate: "middle", href, actions: href ? { click: "open" } : undefined, title: h.path ?? "Path unavailable" }),
				] },
				text(h.qualifiedName ?? h.symbol ?? h.kind ?? "—", { grow: 2, basis: 0, min: { w: 0 }, wrap: "none", truncate: "end", tone: "muted", title: h.qualifiedName ?? h.symbol ?? undefined }),
			] };
		});
	}
	describe(cx: DescribeContext): NativeNode | null {
		if (cx.cols < 36 || !(["col", "row", "text"] as const).every(k => cx.supports(k))) {
			return { k: "col", c: [...this.fallbackBody()] };
		}
		const children: NativeNode[] = [];
		const d = this.display;
		const id = d.identity;
		const displayName = id.name ?? (id.localRoot !== null && path.isAbsolute(id.localRoot) ? path.basename(id.localRoot) || "checkout" : "Codebase");
		const identity = text(`${id.copy} · ${displayName}`, { tone: "muted", title: `Codebase ID: ${id.codebaseId ?? "unbound"}\nCopy: ${id.copy}\nLocal root: ${id.localRoot ?? "none"}` });
		if (d.kind === "search" && d.indexWarning) {
			const props = { tone: "warning" as const, title: d.indexWarning, aria: `Index warning: ${d.indexWarning}` };
			children.push({ k: "row", p: { gap: "sm", align: "center" }, c: [identity,
				cx.supports("icon") ? { k: "icon", p: { name: "warn", ...props } } : text("Index warning", props),
			] });
		} else children.push(identity);
		if (d.kind === "search" || d.kind === "batch") {
			const rows = d.kind === "search" ? d.hits : d.symbols.flatMap(s => s.hits);
			// Native table cells are text/spans only: rows retain meters, score titles and source selection.
			children.push({ k: "row", key: "columns", p: { gap: "sm", align: "center", wrap: false, tone: "muted" }, c: [
				text("#", { min: { w: "3ch" }, max: { w: "3ch" }, shrink: 0, wrap: "none" }),
				text("", { min: { w: "8ch" }, max: { w: "8ch" }, shrink: 0, wrap: "none", title: "Relative match score: query-relative, not confidence", aria: "Relative match score" }),
				text("File:line", { grow: 3, basis: 0, min: { w: 0 }, wrap: "none", truncate: "end" }),
				text("Symbol", { grow: 2, basis: 0, min: { w: 0 }, wrap: "none", truncate: "end" }),
			] });
			const rendered = this.hitRows(d, rows, cx);
			const entries: NativeNode[] = [];
			if (d.kind === "batch") {
				let offset = 0;
				for (const [index, s] of d.symbols.entries()) {
					entries.push({ ...text(`${s.symbol} · ${s.hits.length ? `${s.hits.length} matches` : "no match"}`, { tone: "muted", wrap: "none", truncate: "end" }),
						key: `${s.hits.length ? "batch-group" : "batch-miss"}-${index}` });
					entries.push(...rendered.slice(offset, offset + s.hits.length)); offset += s.hits.length;
				}
			} else entries.push(...rendered);
			const visible = this.matchesExpanded ? entries : entries.slice(0, 8);
			// Do not leave a group heading alone when its first match is outside the compact window.
			if (!this.matchesExpanded && visible.at(-1)?.key?.startsWith("batch-group-")) visible.pop();
			children.push(...visible);
			if (entries.length > visible.length || this.matchesExpanded) children.push({
				...text(this.matchesExpanded ? "Show fewer matches" : `Show more matches (${entries.length - visible.length} more rows)`, {
					tone: "accent", wrap: "none", aria: this.matchesExpanded ? "Show fewer matches" : "Show more matches",
					actions: { click: this.matchesExpanded ? "matches:less" : "matches:more" },
				}), key: "matches-control",
			});
			const selected = rows[this.hitIndex] ?? rows[0];
			if (selected) children.push(...this.preview(selected, cx));
		} else if (d.kind === "source") {
			children.push(text(`${d.path} · ${d.sourceOrigin} · bytes ${d.byteStart}–${d.byteEnd}/${d.fileSize}${d.truncated ? " · truncated" : ""}${d.binary ? " · binary" : ""}`, { title: `Content hash ${d.contentHash} · ${d.encoding}` }));
			const numbered = d.lineStart !== null && d.lineStart > 0 && d.lineEnd !== null && lineCount(d.text) === d.lineEnd - d.lineStart + 1;
			children.push(...this.sourceNodes(d.text, { path: d.path, language: d.language ?? undefined, start: numbered ? d.lineStart! : undefined, limit: 16 }, cx));
		} else children.push(...this.status(d, cx));
		return { k: "col", c: children };
	}
	private status(d: Status, cx: DescribeContext): NativeNode[] {
		const out = [text("Index snapshot · not live progress", { tone: "muted" })];
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

/** One renderer instance belongs to one native tool card, including xd:// calls. */
export function createInspectorRenderer(server: string, operation: string): ToolRendererFactory {
	return ({ requestRender, fallback }) => {
		let view: SemctlInspector | undefined;
		return {
			...fallback,
			mergeCallAndResult: true,
			describeCall(args, options) {
				const original = fallback.describeCall?.(args, options);
				if (operation !== "search_codebase" || !object(args) || typeof args.query !== "string") return original;
				return { ...original, tool: { ...original?.tool, title: `${server}/${operation}`, target: clean(args.query), targetKind: "text" } };
			},
			describeResult(result, options, args) {
				const details = result.details;
				const display = !result.isError && object(details) && details.isError !== true &&
					details.serverName === server && details.mcpToolName === operation && object(details.mcpMeta)
					? readDisplay(details.mcpMeta.semctlDisplay, operation) : undefined;
				if (!display || options.expanded || options.isPartial || options.cancelled) {
					return fallback.describeResult?.(result, options, args);
				}
				if (view?.display !== display) {
					view = new SemctlInspector(display, requestRender,
						() => fallback.describeResult?.(result, options, args)?.body ?? [],
						result.content.filter(block => block.type === "text").map(block => block.text ?? "").join("\n\n"));
				}
				const count = display.kind === "search" ? display.hits.length :
					display.kind === "batch" ? display.symbols.reduce((sum, s) => sum + s.hits.length, 0) : undefined;
				return {
					tool: { title: `${server}/${operation}`, target: display.kind === "search" ? clean(display.query) : display.kind === "source" ? display.path : undefined,
						targetKind: display.kind === "source" ? "path" : "text", meta: count === undefined ? undefined : [`${count} matches`] },
					body: [view],
					preview: "children",
				};
			},
		};
	};
}

/** Older hosts keep their normal MCP cards; never mount a replacement result widget. */
export function installInspector(pi: ExtensionAPI): void {
	if (typeof pi.registerToolRenderer !== "function") return;
	for (const [server, prefix] of Object.entries(SERVERS)) {
		for (const operation of Object.keys(OPERATIONS)) {
			pi.registerToolRenderer(`${prefix}${operation}`, createInspectorRenderer(server, operation));
		}
	}
}
