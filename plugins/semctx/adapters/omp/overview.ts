import * as path from "node:path";
import type { ExtensionAPI, ExtensionContext } from "@oh-my-pi/pi-coding-agent";
import type { Component, DescribeContext, NativeNode, NativeScroll, NativeUiEvent } from "@oh-my-pi/pi-tui";
import type { StatusUpdate } from "./status";
import type { Display } from "./inspector";
import { GraphActivityTracker, type GraphActivity } from "./graph-activity";
import { resetConcepts, setConceptSnapshot } from "./concept-colors";
import { LiveGraphPopup, type LiveGraphState } from "./live-graph";

interface Operation { id: string; label: string; operation: string; fields: readonly string[] }
export const GRAPH_OPERATIONS: readonly Operation[] = [
	{ id: "overview", label: "Indexed overview", operation: "overview", fields: [] },
	{ id: "projects", label: "Projects", operation: "list_projects", fields: [] },
	{ id: "imports", label: "File imports", operation: "imports", fields: [] },
	{ id: "symbol-edges", label: "Symbol bindings", operation: "symbol_edges", fields: [] },
	{ id: "external-links", label: "External links", operation: "external_links", fields: [] },
	{ id: "trace", label: "Symbol neighbourhood", operation: "trace", fields: ["Symbol"] },
	{ id: "call-graph", label: "Call graph", operation: "call_graph", fields: ["Symbol"] },
	{ id: "call-path", label: "Shortest call path", operation: "call_path", fields: ["From symbol", "To symbol"] },
	{ id: "type-hierarchy", label: "Type hierarchy", operation: "type_hierarchy", fields: ["Symbol"] },
	{ id: "reaches", label: "Value flow: destinations", operation: "reaches", fields: ["From boundary"] },
	{ id: "flows-into", label: "Value flow: sources", operation: "flows_into", fields: ["To boundary"] },
	{ id: "flows-between", label: "Value flow witness", operation: "flows_between", fields: ["From boundary", "To boundary"] },
	{ id: "cycles", label: "Call cycles", operation: "cycles", fields: [] },
	{ id: "duplicates", label: "Duplicate groups", operation: "duplicates", fields: [] },
	{ id: "unused", label: "Unused definitions", operation: "unused", fields: [] },
	{ id: "outline", label: "File outline", operation: "file_outline", fields: ["Codebase-relative path"] },
	{ id: "definitions", label: "Definitions", operation: "find_definition", fields: ["Symbol"] },
	{ id: "references", label: "References", operation: "find_references", fields: ["Symbol"] },
	{ id: "who-calls", label: "Callers", operation: "who_calls", fields: ["Symbol"] },
	{ id: "implementations", label: "Implementations", operation: "implementations_of", fields: ["Symbol"] },
	{ id: "search-symbols", label: "Search declarations", operation: "search_symbols", fields: ["Declaration query"] },
	{ id: "batch", label: "Batch symbols", operation: "batch_lookup", fields: ["Symbol"] },
	{ id: "symbol-at-position", label: "Symbol at position", operation: "symbol_at_position", fields: ["Codebase-relative path", "Line (1-based)"] },
	{ id: "expand", label: "Expand source context", operation: "expand_chunk", fields: ["Codebase-relative path", "Start line (1-based)", "End line (1-based)"] },
];

/** Read-only argument contract for native graph analyses. */
export function graphArguments(id: string, values: unknown): { operation: Operation; args: string[] } {
	const operation = GRAPH_OPERATIONS.find(item => item.id === id);
	if (!operation) throw new Error("Unknown read-only graph operation.");
	if (!Array.isArray(values) || values.length !== operation.fields.length) throw new Error("Graph argument count does not match this operation.");
	const args = values.map((value, index) => {
		if (typeof value !== "string" || !value.trim() || value.length > 4096 || /[\u0000-\u001f\u007f]/.test(value)) throw new Error("Graph arguments must be nonempty strings of at most 4096 characters without control characters.");
		const argument = value.trim();
		if (operation.fields[index].toLowerCase().includes("line") && (!/^[1-9]\d*$/.test(argument) || Number(argument) > 0xffff_ffff)) throw new Error("Line must be a positive 32-bit integer.");
		return argument;
	});
	if (operation.id === "expand" && Number(args[2]) < Number(args[1])) throw new Error("End line must not precede start line.");
	return { operation, args };
}
const object = (v: unknown): v is Record<string, unknown> => v !== null && typeof v === "object" && !Array.isArray(v);
const clean = (s: string) => s.replace(/[\x00-\x08\x0b-\x1f\x7f-\x9f]/g, "");
const text = (s: string, p: Record<string, unknown> = {}): NativeNode => ({ k: "text", p: { text: clean(s), wrap: "word", ...p } });

/** Match native Windows extended paths without accepting relative or control-bearing checkout identities. */
export function graphRoot(value: string): string | undefined {
	if (!value || /[\u0000-\u001f]/.test(value)) return undefined;
	const ordinary = value.startsWith("\\\\?\\UNC\\") ? `\\\\${value.slice(8)}` : value.startsWith("\\\\?\\") ? value.slice(4) : value;
	if (!path.isAbsolute(ordinary)) return undefined;
	const root = path.resolve(ordinary);
	return process.platform === "win32" ? root.toLowerCase() : root;
}
function within(root: string, cwd: string): boolean {
	const relative = path.relative(root, cwd);
	return relative === "" || (relative !== ".." && !relative.startsWith(`..${path.sep}`) && !path.isAbsolute(relative));
}
export interface GraphOverviewDependencies {
	readDisplay(value: unknown, operation: string, localOverview?: boolean): Display | undefined;
	createInspector(display: Display, requestRender: () => void, rawText: string, presentation?: "overview"): Component;
	readStatusUpdate?(params: unknown, previous?: StatusUpdate): StatusUpdate | undefined;
}
export interface GraphRequestState { loading: boolean; text: string; error?: string; display?: Display }

/** One cancellable read-only request. Generation + cwd checks reject even abort-ignoring executors. */
export class GraphRequest {
	state: GraphRequestState = { loading: false, text: "" };
	private controller: AbortController | undefined;
	private generation = 0;
	constructor(private readonly exec: ExtensionAPI["exec"], private readonly dependencies: GraphOverviewDependencies,
		private readonly changed: () => void) {}
	cancel(): void {
		this.generation++;
		this.controller?.abort();
		this.controller = undefined;
		if (this.state.loading) this.state = { ...this.state, loading: false };
	}
	async run(operation: Operation, args: string[], cwd: string, currentCwd: () => string): Promise<void> {
		this.cancel();
		const root = graphRoot(cwd), generation = this.generation;
		const controller = new AbortController();
		this.controller = controller;
		this.state = { loading: true, text: "" };
		this.changed();
		const current = () => !controller.signal.aborted && this.generation === generation && graphRoot(currentCwd()) === root;
		try {
			if (!root) throw new Error("Current checkout is not an absolute local path.");
			({ operation, args } = graphArguments(operation.id, args));
			const result = await this.exec("semctl", ["graph", "--json", operation.id, "--", ...args], { cwd, signal: controller.signal, timeout: 60_000 });
			if (!current()) return;
			if (result.killed || result.code !== 0) throw new Error([result.stderr.trim(), result.stdout.trim()].filter(Boolean).join("\n") || `semctl exited with code ${result.code}${result.killed ? " (cancelled or timed out)" : ""}`);
			if (Buffer.byteLength(result.stdout, "utf8") > 4 * 1024 * 1024) {
				this.state = { loading: false, text: result.stdout, error: "CLI JSON exceeds the 4 MiB interactive limit; showing original output." };
			} else {
				let value: unknown;
				try { value = JSON.parse(result.stdout); } catch { throw new Error(`semctl did not return valid JSON.\n${[result.stdout, result.stderr].filter(Boolean).join("\n")}`); }
				if (!object(value) || typeof value.text !== "string") throw new Error(`semctl returned an invalid QueryOutput JSON object.\n${result.stdout}`);
				const display = this.dependencies.readDisplay(value.display, operation.operation, operation.id === "overview");
				const displayRoot = display?.identity.localRoot ? graphRoot(display.identity.localRoot) : undefined;
				if (display?.identity.localRoot && (!displayRoot || !within(displayRoot, root))) throw new Error("CLI display belongs to a different checkout; result was not opened.");
				this.state = { loading: false, text: value.text, display,
					error: [value.display && !display ? "Structured preview unavailable or invalid; showing original result." : "", result.stderr.trim()].filter(Boolean).join("\n") || undefined };
				if (operation.id === "overview" && display) setConceptSnapshot(display);
			}
		} catch (error) {
			if (!current()) return;
			this.state = { loading: false, text: "", error: error instanceof Error ? error.message : String(error) };
		} finally {
			if (current()) { this.controller = undefined; this.changed(); }
		}
	}
}

/** Native views share one request engine. Activity comes only from observed tool and producer events. */
export function createGraphOverview(pi: ExtensionAPI, dependencies: GraphOverviewDependencies) {
	let generation = 0;
	let live: { root: string; request: GraphRequest; render: (kind?: "state" | "activity") => void; close: () => void } | undefined;
	let observedCwd: string | undefined;
	let popupGeneration = 0;
	let popup: { root: string; close(): void; render(kind: "state" | "activity"): void } | undefined;
	let catalog: { root: string; request: GraphRequest; settled: boolean; queued: boolean; pending?: Promise<void>; last?: GraphRequestState } | undefined;
	const renderActivity = () => { live?.render("activity"); popup?.render("activity"); };
	const tracker = new GraphActivityTracker(graphRoot, dependencies.readDisplay, renderActivity);
	const statuses = new Map<string, StatusUpdate>();
	const reset = () => {
		generation++;
		popupGeneration++;
		popup?.close(); popup = undefined;
		catalog?.request.cancel(); catalog = undefined;
		resetConcepts();
		tracker.reset();
		live?.request.cancel();
		live?.close();
		live = undefined;
		statuses.clear();
		observedCwd = undefined;
	};
	const activity = (root: string, state: GraphRequestState): GraphActivity => {
		const observation = [...statuses.values()].filter(status => within(status.root, root)).sort((a, b) => b.root.length - a.root.length)[0];
		const producer = observation?.indicator, details = observation?.summary ?? observation?.details;
		return {
			...tracker.snapshot(root, state.display),
			producer: producer ? { text: producer.text.length <= 4096 ? producer.text.replace(/[\r\n\t]+/g, " ") : "Producer observation exceeds the interactive text limit.", tone: producer.tone,
				details: details && details.length > 128 * 1024 ? "Producer details exceed the interactive text limit." : details,
				progress: typeof producer.progress === "number" && Number.isFinite(producer.progress) ? Math.max(0, Math.min(1, producer.progress)) : undefined } : null,
		};
	};
	const warm = (ctx: ExtensionContext, refresh = false): Promise<void> => {
		const root = graphRoot(ctx.cwd);
		if (!ctx.hasUI || !root || typeof pi.exec !== "function" || (observedCwd !== undefined && observedCwd !== root)) return Promise.resolve();
		if (catalog?.root !== root) {
			catalog?.request.cancel();
			const entry: NonNullable<typeof catalog> = {
				root, settled: false, queued: false,
				request: new GraphRequest(pi.exec.bind(pi), dependencies, () => {
					if (catalog !== entry) return;
					if (entry.request.state.display) entry.last = entry.request.state;
					popup?.render("state");
				}),
			};
			catalog = entry;
		}
		const entry = catalog;
		if (entry.pending) { entry.queued ||= refresh; return entry.pending; }
		if (entry.settled && !refresh) return Promise.resolve();
		entry.pending = entry.request.run(GRAPH_OPERATIONS[0], [], ctx.cwd, () => observedCwd ?? ctx.cwd).finally(() => {
			if (catalog !== entry) return;
			entry.pending = undefined; entry.settled = true;
			if (entry.queued) { entry.queued = false; void warm(ctx, true); }
		});
		return entry.pending;
	};
	return {
		reset,
		warm,
		observe(event: unknown, ctx: ExtensionContext): void {
			const currentRoot = graphRoot(ctx.cwd);
			if (observedCwd !== undefined && observedCwd !== currentRoot) reset();
			observedCwd = currentRoot;
			if (!object(event)) return;
			let changed = false;
			if (event.type === "tool_call" && observedCwd) {
				changed = tracker.observeCall(event, observedCwd);
				if (changed && !live && !popup) void warm(ctx);
			} else if (event.type === "tool_result") {
				const display = (catalog && catalog.root === observedCwd ? catalog.request.state.display ?? catalog.last?.display : undefined) ?? live?.request.state.display;
				changed = tracker.observeResult(event, display, live !== undefined || popup !== undefined);
			}
			else if ((event.server === "semctx" || event.server === "semctx:semctx") && event.method === "notifications/semctl/sync_status") {
				const source = object(event.params) && object(event.params.status) ? event.params.status : event.params;
				const reportedRoot = object(source) && typeof source.root === "string" ? graphRoot(source.root) : undefined;
				const previous = reportedRoot ? statuses.get(`${event.server}\0${reportedRoot}`) : undefined;
				const update = dependencies.readStatusUpdate?.(event.params, previous);
				if (update) {
					const root = graphRoot(update.root);
					if (root && within(root, observedCwd ?? "")) {
						const key = `${event.server}\0${root}`, prior = statuses.get(key);
						if (catalog && update.indicator?.tone === "success" && prior?.indicator?.tone !== "success") void warm(ctx, true);
						changed = prior?.indicator?.text !== update.indicator?.text || prior?.indicator?.tone !== update.indicator?.tone ||
							prior?.indicator?.progress !== update.indicator?.progress || prior?.summary !== update.summary || prior?.details !== update.details;
						if (update.indicator) statuses.set(key, update); else statuses.delete(key);
						if (statuses.size > 32) statuses.delete(statuses.keys().next().value!);
					}
				} else if (object(event.params)) {
					const source = object(event.params.status) ? event.params.status : event.params;
					const root = typeof source.root === "string" ? graphRoot(source.root) : undefined;
					if (root) changed = statuses.delete(`${event.server}\0${root}`) && within(root, observedCwd ?? "");
				}
			}
			if (changed) renderActivity();
		},
		async togglePopup(ctx: ExtensionContext): Promise<void> {
			if (!ctx.hasUI) return;
			if (popup) { popup.close(); return; }
			const root = graphRoot(ctx.cwd);
			if (!root || typeof pi.exec !== "function" || typeof ctx.ui.custom !== "function") {
				ctx.ui.notify?.("The live graph popup requires a local checkout, command execution, and custom UI support.", "info"); return;
			}
			if (observedCwd !== undefined && observedCwd !== root) reset();
			observedCwd = root;
			const owner = ++popupGeneration;
			const current = () => owner === popupGeneration && observedCwd === root && graphRoot(ctx.cwd) === root;
			const snapshot = (): LiveGraphState => {
				const entry = catalog?.root === root ? catalog : undefined;
				const state = entry?.request.state ?? { loading: true, text: "" };
				const request = state.display || !entry?.last ? state : { ...state, text: entry.last.text, display: entry.last.display };
				return { request, activity: activity(root, request) };
			};
			let view: LiveGraphPopup | undefined, complete: (() => void) | undefined;
			const close = () => {
				view?.dispose();
				if (owner === popupGeneration) { popup = undefined; popupGeneration++; if (!live) tracker.clearReceipts(); }
				complete?.();
			};
			popup = { root, close, render: () => view?.update(snapshot()) };
			try {
				void ctx.ui.custom<void>((tui, _theme, _keys, done) => {
					if (typeof tui.hasModalOverlay !== "function") throw new Error("The live graph popup requires an OMP build with non-modal overlay support. Use /semctl graph for the overview.");
					void warm(ctx);
					complete = () => done(undefined);
					view = new LiveGraphPopup(snapshot(), () => tui.requestRender(), {
						close,
						refresh: () => { if (current()) void warm(ctx, true); },
						expand: () => { if (current()) void this.open(ctx); },
					});
					if (!current()) close();
					return view;
				}, { overlay: true, overlayOptions: { nonModal: true, anchor: "top-center", width: 44 } }).catch(error => {
					if (!current()) return;
					close();
					ctx.ui.notify?.(error instanceof Error ? error.message : "Live graph popup failed.", "error");
				});
			} catch (error) {
				if (!current()) return;
				close();
				ctx.ui.notify?.(error instanceof Error ? error.message : "Live graph popup failed.", "error");
			}
		},
		async open(ctx: ExtensionContext): Promise<void> {
			if (!ctx.hasUI) return;
			const notify = (message: string) => { if (typeof ctx.ui.notify === "function") ctx.ui.notify(message, "error"); };
			if (typeof ctx.ui.custom !== "function" || typeof pi.exec !== "function") {
				notify("Graph overview needs a host with native custom UI and command execution. Read-only CLI alternative: semctl graph --json overview");
				return;
			}
			const root = graphRoot(ctx.cwd);
			if (!root) { notify("Graph overview requires an absolute local checkout path."); return; }
			if (observedCwd !== undefined && observedCwd !== root) reset();
			live?.request.cancel(); live?.close();
			const owner = ++generation;
			observedCwd = root;
			let operation = GRAPH_OPERATIONS[0], args: string[] = [];
			while (owner === generation && graphRoot(ctx.cwd) === root) {
				let currentRequest: GraphRequest | undefined;
				const choice = await ctx.ui.custom<string | undefined>((tui, _theme, keys, done) => {
					let menu = false, selectedMenu = GRAPH_OPERATIONS.indexOf(operation), scroll: NativeScroll = { by: "start", n: 0 }, offset = 0;
					let inspector: Component | undefined, lastDisplay: Display | undefined;
					const expanded = new Set<string>();
					const request = new GraphRequest(pi.exec.bind(pi), dependencies, () => tui.requestRender());
					currentRequest = request;
					const close = (next?: string) => {
						request.cancel();
						if (live?.request === request) live = undefined;
						if (!popup) tracker.clearReceipts();
						done(next);
					};
					live = { root, request, render: () => tui.requestRender(), close: () => close() };
					void request.run(operation, args, ctx.cwd, () => observedCwd ?? ctx.cwd);
					const currentActivity = () => activity(root, request.state);
					const height = () => Math.max(1, tui.terminal.rows - 7);
					const fallbackRows = (width: number) => {
						const state = request.state;
						return ["semctl graph · " + operation.label, ctx.cwd, ...(menu ? GRAPH_OPERATIONS.map((op, i) => `${i === selectedMenu ? ">" : " "} ${op.label}`) : []),
							...(state.loading ? ["Reading configured semctl CLI…"] : []), ...(state.error ? [state.error] : []), ...clean(state.text).split(/\r?\n/)]
							.flatMap(line => clean(line).split(/\r?\n/))
							.flatMap(line => line.length ? Array.from({ length: Math.ceil(line.length / Math.max(1, width)) }, (_, i) => line.slice(i * width, (i + 1) * width)) : [""]);
					};
					return {
						invalidate() { inspector?.invalidate?.(); },
						render(width: number) { const rows = fallbackRows(width); offset = Math.max(0, Math.min(offset, rows.length - height())); return rows.slice(offset, offset + height()); },
						describe(cx: DescribeContext): NativeNode {
							const state = request.state;
							if (state.display !== lastDisplay) { lastDisplay = state.display; inspector = state.display ? dependencies.createInspector(state.display, () => tui.requestRender(), state.text, "overview") : undefined; }
							if (inspector && "setActivity" in inspector && typeof inspector.setActivity === "function") {
								inspector.setActivity(currentActivity().targets);
							}
							const observed = currentActivity(), active = observed.operations, status = observed.producer;
							const children: NativeNode[] = [
								{ k: "row", p: { gap: "sm", wrap: true }, c: [text(`semctl graph · ${operation.label}`, { grow: 1 }),
									text("Analyses", { actions: { click: "analyses" }, tone: "accent", aria: "Choose graph analysis" }),
									text("Refresh", { actions: { click: "refresh" }, tone: "accent", aria: "Read graph again from CLI" }),
									text("Close", { actions: { click: "close" }, aria: "Close graph overview" })] },
								text(`Read-only · configured semctl CLI · ${ctx.cwd}`, { tone: "muted" }),
							];
							if (menu) children.push(cx.supports("list") ? { k: "list", key: "analyses", p: { selected: GRAPH_OPERATIONS[selectedMenu].id },
								c: GRAPH_OPERATIONS.map(op => ({ k: "item", key: op.id, p: { label: op.label } })) } : { k: "col", c: GRAPH_OPERATIONS.map(op => text(op.label, { actions: { click: `choose:${op.id}` } })) });
							if (state.loading || active.length) children.push({ k: "row", p: { gap: "sm" }, c: [
								...(!cx.reduceMotion && cx.supports("spinner") ? [{ k: "spinner" as const }] : []),
								text([...(state.loading ? [`Reading ${operation.label}…`] : []), ...(active.length ? [`Observed semctx requests: ${active.join(", ")}`] : [])].join(" · "), { tone: "accent" }),
							] });
							if (status) children.push({ k: "row", p: { gap: "sm", align: "center" }, c: [
								text(`Last producer observation: ${status.text}`, { tone: status.tone, grow: 1, wrap: "none", truncate: "end", title: status.text }),
								...(status.progress !== undefined && cx.supports("meter") ? [{ k: "meter" as const, p: { value: status.progress, style: "ring" as const, size: "sm" as const, tone: status.tone, aria: "Producer-reported progress" } }] : []),
							] });
							if (state.error) children.push(text(state.error, { tone: "error", aria: "Graph request error" }));
							children.push({ k: "col", key: "body", scroll, c: inspector ? [inspector] : [text(state.text || (state.loading ? "Waiting for CLI result." : "No result text returned."))] });
							if (inspector) children.push(cx.supports("section") ? { k: "section", key: "original", p: { head: "Original CLI result", collapsible: true, collapsed: !expanded.has("original") }, c: [text(state.text)] } : text(state.text));
							if (status?.details) children.push(cx.supports("section") ? { k: "section", key: "producer", p: { head: "Producer details", collapsible: true, collapsed: !expanded.has("producer") }, c: [text(status.details)] } : text(status.details));
							children.push(text("↑/↓ select nodes · PgUp/PgDn scroll · A analyses · R refresh · Esc close", { tone: "muted" }));
							return { k: "col", p: { gap: "sm" }, c: children };
						},
						handleNativeEvent(event: NativeUiEvent) {
							if (event.type === "toggle") {
								const key = event.key.slice(event.key.lastIndexOf("/") + 1);
								if (key === "original" || key === "producer") {
									if (event.collapsed) expanded.delete(key); else expanded.add(key);
									tui.requestRender();
								} else inspector?.handleNativeEvent?.(event);
							}
							else if (event.type === "select" && menu) { const i = GRAPH_OPERATIONS.findIndex(op => op.id === event.item); if (i >= 0) { selectedMenu = i; tui.requestRender(); } }
							else if (event.type === "activate" && menu && GRAPH_OPERATIONS.some(op => op.id === event.item)) close(event.item);
							else if (event.type === "action") {
								if (event.act === "close") close();
								else if (event.act === "analyses") { menu = !menu; tui.requestRender(); }
								else if (event.act === "refresh") void request.run(operation, args, ctx.cwd, () => observedCwd ?? ctx.cwd);
								else if (event.act.startsWith("choose:") && GRAPH_OPERATIONS.some(op => op.id === event.act.slice(7))) close(event.act.slice(7));
								else inspector?.handleNativeEvent?.(event);
							}
							else inspector?.handleNativeEvent?.(event);
						},
						handleInput(data: string) {
							if (keys.matches(data, "tui.select.cancel")) { if (menu) { menu = false; tui.requestRender(); } else close(); return; }
							if (data === "a" || data === "A") { menu = !menu; tui.requestRender(); return; }
							if ((data === "r" || data === "R") && !menu) { void request.run(operation, args, ctx.cwd, () => observedCwd ?? ctx.cwd); return; }
							if (menu && keys.matches(data, "tui.select.confirm")) { close(GRAPH_OPERATIONS[selectedMenu].id); return; }
							if (keys.matches(data, "tui.select.up") || keys.matches(data, "tui.select.down")) {
								const delta = keys.matches(data, "tui.select.up") ? -1 : 1;
								if (menu) selectedMenu = Math.max(0, Math.min(GRAPH_OPERATIONS.length - 1, selectedMenu + delta));
								else if (inspector && "move" in inspector && typeof inspector.move === "function") inspector.move(delta);
								else { offset += delta; scroll = { by: delta > 0 ? "line-down" : "line-up", n: scroll.n + 1 }; }
							} else if (keys.matches(data, "tui.select.pageUp") || keys.matches(data, "tui.select.pageDown")) {
								const down = keys.matches(data, "tui.select.pageDown"); offset += down ? height() : -height(); scroll = { by: down ? "page-down" : "page-up", n: scroll.n + 1 };
							} else return;
							tui.requestRender();
						},
					};
				}, { overlay: true, overlayOptions: { fullscreen: true } });
				currentRequest?.cancel();
				if (owner !== generation) return;
				live = undefined;
				if (!choice) return;
				const selected = GRAPH_OPERATIONS.find(op => op.id === choice);
				if (!selected) return;
				const values: string[] = [];
				let cancelled = false;
				if (selected.fields.length && typeof ctx.ui.input !== "function") {
					notify(`This host cannot prompt for graph arguments. Read-only CLI alternative: semctl graph --json ${selected.id} ${selected.fields.map(field => `<${field}>`).join(" ")}`);
					continue;
				}
				for (const field of selected.fields) {
					const value = await ctx.ui.input(`${selected.label} · ${field}`, field);
					if (owner !== generation || graphRoot(ctx.cwd) !== root) return;
					if (value === undefined || !value.trim()) { cancelled = true; break; }
					if (field.toLowerCase().includes("line") && (!/^[1-9]\d*$/.test(value.trim()) || Number(value.trim()) > 0xffff_ffff)) { notify("Line must be a positive 32-bit integer."); cancelled = true; break; }
					values.push(value.trim());
				}
				if (selected.id === "expand" && !cancelled && Number(values[2]) < Number(values[1])) { notify("End line must not precede start line."); cancelled = true; }
				if (!cancelled) {
					try { ({ operation, args } = graphArguments(selected.id, values)); }
					catch (error) { notify(error instanceof Error ? error.message : "Invalid graph arguments."); }
				}
			}
		},
	};
}
