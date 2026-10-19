import * as path from "node:path";
import type { ExtensionAPI, ExtensionContext } from "@oh-my-pi/pi-coding-agent";
import type { Component, DescribeContext, NativeNode, NativeUiEvent } from "@oh-my-pi/pi-tui";
import type { StatusUpdate } from "./status";
import type { Display } from "./inspector";
import { GraphActivityTracker, type GraphActivity } from "./graph-activity";
import { resetConcepts, setConceptSnapshot } from "./concept-colors";
import { LiveGraphPopup, type LiveGraphState } from "./live-graph";
import { NativeSurface } from "./native-surface";
import { GraphAppearanceStore } from "./graph-appearance";
import { GraphAppearancePanel, type GraphAppearanceKeyMatcher } from "./graph-appearance-panel";
import { GraphPage } from "./graph-page";

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
	scheduleRefresh?(callback: () => void, delay: number): () => void;
	appearance?: GraphAppearanceStore;
	appearanceKey?: GraphAppearanceKeyMatcher;
}
export interface GraphRequestState { loading: boolean; text: string; error?: string; display?: Display }

/** One cancellable read-only request. Generation + cwd checks reject even abort-ignoring executors. */
export class GraphRequest {
	state: GraphRequestState = { loading: false, text: "" };
	private controller: AbortController | undefined;
	private generation = 0;
	private requestKey: string | undefined;
	constructor(private readonly exec: ExtensionAPI["exec"], private readonly dependencies: GraphOverviewDependencies,
		private readonly changed: () => void, cached?: GraphRequest) {
		if (cached?.state.display) {
			this.state = { ...cached.state, loading: false };
			this.requestKey = cached.requestKey;
		}
	}
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
		const previous = this.state;
		let retained: GraphRequestState | undefined;
		this.state = { loading: true, text: "" };
		const current = () => !controller.signal.aborted && this.generation === generation && graphRoot(currentCwd()) === root;
		try {
			if (!root) throw new Error("Current checkout is not an absolute local path.");
			({ operation, args } = graphArguments(operation.id, args));
			const key = JSON.stringify([root, operation.id, args]);
			if (key === this.requestKey && previous.display) retained = previous;
			this.requestKey = key;
			this.state = { loading: true, text: retained?.text ?? "", ...(retained?.display ? { display: retained.display } : {}) };
			this.changed();
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
			this.state = { loading: false, text: retained?.text ?? "", ...(retained?.display ? { display: retained.display } : {}),
				error: [error instanceof Error ? error.message : String(error), ...(retained ? ["Showing the last available graph for this request."] : [])].join("\n") };
		} finally {
			if (current()) { this.controller = undefined; this.changed(); }
		}
	}
}

/** Native views share one request engine. Activity comes only from observed tool and producer events. */
export function createGraphOverview(pi: ExtensionAPI, dependencies: GraphOverviewDependencies) {
	const appearance = dependencies.appearance ?? new GraphAppearanceStore();
	let closeAppearance: (() => void) | undefined;
	const showAppearance = async (ctx: ExtensionContext): Promise<void> => {
		if (closeAppearance || typeof ctx.ui.custom !== "function") return;
		let ownedClose: (() => void) | undefined;
		try {
			await ctx.ui.custom<undefined>((tui, _theme, keys, done) => {
				const surface = new NativeSurface(tui);
				let panel: GraphAppearancePanel | undefined, disposed = false;
				const dispose = () => {
					if (disposed) return;
					disposed = true; panel?.dispose(); surface.dispose();
					if (closeAppearance === close) closeAppearance = undefined;
				};
				const close = () => { if (!disposed) { dispose(); done(undefined); } };
				closeAppearance = close;
				ownedClose = close;
				panel = new GraphAppearancePanel(appearance, () => tui.requestRender(), close, (data, key) => {
					if (key === "left" || key === "right") return dependencies.appearanceKey?.(data, key) ?? false;
					const action = { up: "tui.select.up", down: "tui.select.down", enter: "tui.select.confirm", escape: "tui.select.cancel" } as const;
					return keys.matches(data, action[key]);
				});
				const view = panel;
				return {
					nativeOverlay: view.nativeOverlay,
					// Keep keyboard ownership without blurring the graph being adjusted.
					nativeSheet: (cx: DescribeContext) => cx.supports("overlay"),
					describe(cx: DescribeContext): NativeNode {
						surface.prepare(cx);
						const body = view.describe(cx);
						return cx.supports("overlay") ? { k: "overlay", key: "appearance",
							p: { ...view.nativeOverlay, modal: false, anchor: "center" }, c: [body] } : body;
					},
					render: (width: number) => view.render(width),
					handleInput: (data: string) => view.handleInput(data),
					handleNativeEvent: (event: NativeUiEvent) => view.handleNativeEvent(event),
					dispose,
				};
			}, { overlay: true, overlayOptions: { width: 48, maxHeight: "85%" } });
		} catch (error) {
			ownedClose?.();
			ctx.ui.notify?.(error instanceof Error ? error.message : "Graph appearance popup failed.", "error");
		}
	};
	let generation = 0;
	let live: { root: string; request: GraphRequest; render: (kind?: "state" | "activity") => void; close: () => void } | undefined;
	let observedCwd: string | undefined;
	let popupGeneration = 0;
	let popup: { root: string; close(): void; render(kind: "state" | "activity"): void } | undefined;
	let catalog: { root: string; request: GraphRequest; settled: boolean; queued: boolean; pending?: Promise<void>; last?: GraphRequestState } | undefined;
	let cancelLoadingRefresh: (() => void) | undefined;
	const stopLoadingRefresh = () => { cancelLoadingRefresh?.(); cancelLoadingRefresh = undefined; };
	const scheduleRefresh = dependencies.scheduleRefresh ?? ((callback: () => void, delay: number) => {
		const timer = setTimeout(callback, delay);
		return () => clearTimeout(timer);
	});
	const renderActivity = () => { live?.render("activity"); popup?.render("activity"); };
	const tracker = new GraphActivityTracker(graphRoot, dependencies.readDisplay, renderActivity);
	const statuses = new Map<string, StatusUpdate>();
	const reset = () => {
		generation++;
		closeAppearance?.();
		stopLoadingRefresh();
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
				symbol: producer.symbol, details: details && details.length > 128 * 1024 ? "Producer details exceed the interactive text limit." : details,
				diagnostic: observation?.diagnostic,
				progress: typeof producer.progress === "number" && Number.isFinite(producer.progress) ? Math.max(0, Math.min(1, producer.progress)) : undefined } : null,
		};
	};
	const refreshLoadingLayers = (ctx: ExtensionContext, entry: NonNullable<typeof catalog>) => {
		stopLoadingRefresh();
		const state = entry.request.state;
		if (!popup || popup.root !== entry.root || state.error || state.display?.kind !== "graph" || !state.display.loading) return;
		cancelLoadingRefresh = scheduleRefresh(() => {
			cancelLoadingRefresh = undefined;
			if (catalog === entry && popup?.root === entry.root && graphRoot(ctx.cwd) === entry.root) void warm(ctx, true);
		}, 10_000);
	};
	const warm = (ctx: ExtensionContext, refresh = false): Promise<void> => {
		const root = graphRoot(ctx.cwd);
		if (!ctx.hasUI || !root || typeof pi.exec !== "function" || (observedCwd !== undefined && observedCwd !== root)) return Promise.resolve();
		if (catalog?.root !== root) {
			stopLoadingRefresh();
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
		if (entry.settled && !refresh && !entry.request.state.error) {
			refreshLoadingLayers(ctx, entry);
			return Promise.resolve();
		}
		stopLoadingRefresh();
		entry.pending = entry.request.run(GRAPH_OPERATIONS[0], [], ctx.cwd, () => observedCwd ?? ctx.cwd).finally(() => {
			if (catalog !== entry) return;
			entry.pending = undefined; entry.settled = true;
			if (entry.queued) { entry.queued = false; void warm(ctx, true); }
			else refreshLoadingLayers(ctx, entry);
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
						const jobChanged = update.graphRevision !== undefined && update.graphRevision !== prior?.graphRevision;
						if (catalog && (update.indicator?.tone === "success" && prior?.indicator?.tone !== "success" || popup && jobChanged)) void warm(ctx, true);
						changed = prior?.indicator?.text !== update.indicator?.text || prior?.indicator?.tone !== update.indicator?.tone ||
							prior?.indicator?.symbol !== update.indicator?.symbol || prior?.indicator?.progress !== update.indicator?.progress ||
							prior?.summary !== update.summary || prior?.details !== update.details;
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
			if (!root || typeof pi.exec !== "function" || typeof ctx.ui.setWidget !== "function") {
				ctx.ui.notify?.("The live graph popup requires a local checkout, command execution, and extension widgets.", "info"); return;
			}
			if (observedCwd !== undefined && observedCwd !== root) reset();
			observedCwd = root;
			const owner = ++popupGeneration;
			const current = () => owner === popupGeneration && observedCwd === root && graphRoot(ctx.cwd) === root;
			const snapshot = (): LiveGraphState => {
				const entry = catalog?.root === root ? catalog : undefined;
				const state = entry?.request.state ?? { loading: true, text: "" };
				const request = state.display || !entry?.last ? state : { ...state, text: entry.last.text, display: entry.last.display };
				return { request, activity: activity(root, request), appearance: appearance.state.value, appearanceError: appearance.state.error };
			};
			let view: LiveGraphPopup | undefined, surface: NativeSurface | undefined, closed = false;
			let unsubscribeAppearance: (() => void) | undefined;
			const dispose = () => {
				if (closed) return;
				closed = true;
				unsubscribeAppearance?.();
				view?.dispose();
				surface?.dispose();
				if (owner === popupGeneration) { closeAppearance?.(); stopLoadingRefresh(); popup = undefined; popupGeneration++; if (!live) tracker.clearReceipts(); }
			};
			const close = () => {
				if (closed) return;
				const ownsWidget = owner === popupGeneration;
				dispose();
				if (ownsWidget) ctx.ui.setWidget("semctl.live", undefined);
			};
			popup = { root, close, render: () => { if (!closed) view?.update(snapshot()); } };
			unsubscribeAppearance = appearance.subscribe(() => { if (current() && !closed) view?.update(snapshot()); });
			void appearance.load();
			try {
				ctx.ui.setWidget("semctl.live", tui => {
					surface = new NativeSurface(tui);
					view = new LiveGraphPopup(snapshot(), () => tui.requestRender(), {
						close,
						refresh: () => { if (current()) void warm(ctx, true); },
						expand: () => { if (current()) void this.open(ctx); },
						appearance: () => { if (current()) void showAppearance(ctx); },
					});
					void warm(ctx);
					if (!current()) dispose();
					return {
						render: width => closed ? [] : view!.render(width),
						describe(cx) {
							surface?.prepare(cx);
							return { k: "overlay", key: "semctl-live", p: {
								role: "omp.overlay.semctl-live", modal: false, size: "sm", anchor: "top", hidden: closed,
								min: { w: 0 }, max: { w: "40ch" },
							}, c: view ? [view] : [] };
						},
						dispose,
					};
				}, { placement: "aboveEditor" });
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
			closeAppearance?.();
			if (observedCwd !== undefined && observedCwd !== root) reset();
			live?.request.cancel(); live?.close();
			const owner = ++generation;
			observedCwd = root;
			let operation = GRAPH_OPERATIONS[0], args: string[] = [];
			while (owner === generation && graphRoot(ctx.cwd) === root) {
				let currentRequest: GraphRequest | undefined;
				const choice = await ctx.ui.custom<string | undefined>((tui, _theme, keys, done) => {
					const surface = new NativeSurface(tui);
					let page: GraphPage | undefined, disposed = false;
					const request = new GraphRequest(pi.exec.bind(pi), dependencies, () => tui.requestRender(),
						operation.id === "overview" && catalog?.root === root ? catalog.request : undefined);
					currentRequest = request;
					const dispose = () => {
						if (disposed) return;
						disposed = true;
						page?.dispose();
						request.cancel();
						surface.dispose();
					};
					const close = (next?: string) => {
						closeAppearance?.();
						dispose();
						if (live?.request === request) live = undefined;
						if (!popup) tracker.clearReceipts();
						done(next);
					};
					page = new GraphPage({
						operation, operations: GRAPH_OPERATIONS, cwd: ctx.cwd, state: () => request.state,
						activity: () => activity(root, request.state), appearance, appearanceKey: dependencies.appearanceKey,
						createInspector: dependencies.createInspector, requestRender: () => tui.requestRender(),
						height: () => Math.max(1, tui.terminal.rows - 7),
						matches: (data, key) => keys.matches(data, `tui.select.${key}`),
						close, refresh: () => { void request.run(operation, args, ctx.cwd, () => observedCwd ?? ctx.cwd); },
						showAppearance: () => { void showAppearance(ctx); },
					});
					const view = page;
					live = { root, request, render: () => tui.requestRender(), close: () => close() };
					void request.run(operation, args, ctx.cwd, () => observedCwd ?? ctx.cwd);
					return {
						nativeOverlay: view.nativeOverlay,
						dispose,
						invalidate: () => view.invalidate(),
						render: (width: number) => view.render(width),
						describe(cx: DescribeContext): NativeNode { surface.prepare(cx); return view.describe(cx); },
						handleNativeEvent: (event: NativeUiEvent) => view.handleNativeEvent(event),
						handleInput: (data: string) => view.handleInput(data),
					};
				}, { overlay: true, overlayOptions: { width: "88%", maxHeight: "90%" } });
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
