import * as path from "node:path";
import type { Component, DescribeContext, NativeNode, NativeScroll, NativeUiEvent } from "@oh-my-pi/pi-tui";
import type { Display } from "./inspector";
import type { GraphActivity } from "./graph-activity";
import type { GraphAppearanceStore } from "./graph-appearance";
import type { GraphAppearanceKeyMatcher } from "./graph-appearance-panel";
import type { GRAPH_OPERATIONS, GraphOverviewDependencies, GraphRequestState } from "./overview";
import { nativeElement, supportsElements } from "./native-elements";
import { graphStatus } from "./graph-status";
import { compactTooltip } from "./tooltip-text";

type Operation = (typeof GRAPH_OPERATIONS)[number];
type Key = "cancel" | "confirm" | "up" | "down" | "pageUp" | "pageDown";
interface PageOptions {
	operation: Operation;
	operations: readonly Operation[];
	cwd: string;
	state(): GraphRequestState;
	activity(): GraphActivity;
	appearance: GraphAppearanceStore;
	appearanceKey?: GraphAppearanceKeyMatcher;
	createInspector: GraphOverviewDependencies["createInspector"];
	requestRender(): void;
	height(): number;
	matches(data: string, key: Key): boolean;
	close(next?: string): void;
	refresh(): void;
	showAppearance(): void;
}
const groups = [
	{ id: "project", label: "Project", ids: ["overview", "projects", "imports", "symbol-edges", "external-links"] },
	{ id: "symbols", label: "Symbols", ids: ["trace", "definitions", "references", "who-calls", "implementations", "search-symbols", "batch"] },
	{ id: "calls", label: "Calls", ids: ["call-graph", "call-path", "cycles"] },
	{ id: "flow", label: "Flow & types", ids: ["reaches", "flows-into", "flows-between", "type-hierarchy"] },
	{ id: "quality", label: "Quality", ids: ["duplicates", "unused"] },
	{ id: "source", label: "Source", ids: ["outline", "symbol-at-position", "expand"] },
] as const;
const clean = (value: string) => value.replace(/[\u0000-\u0008\u000b-\u001f\u007f-\u009f]/g, "");
const text = (value: string, p: Record<string, unknown> = {}): NativeNode => ({ k: "text", p: { text: clean(value), wrap: "word", ...p } });

/** A busy button stays disabled and shows a native spinner. Callers set busy only when motion is allowed. */
function button(cx: DescribeContext, id: string, icon: "diagram" | "repeat" | "settings" | "info" | "x", label: string, hint: string, selected = false, disabled = false, busy = false): NativeNode {
	const actions = disabled ? undefined : { click: id }, title = compactTooltip(hint);
	const spinning = busy && cx.supports("spinner");
	if (supportsElements(cx) && cx.feature("styles")) return nativeElement({ k: "el", key: id, p: {
		tag: "button", class: "semctl-icon-action", actions, title, aria: title,
		attrs: { "aria-pressed": selected, "aria-disabled": disabled, "aria-busy": spinning },
	}, c: [spinning ? { k: "spinner", p: { style: "orbit", aria: "Refreshing" } } : cx.supports("icon") ? { k: "icon", p: { name: icon } } : text(label)] });
	const p = { actions, title, aria: title, tone: selected ? "accent" as const : "muted" as const, shrink: 0 };
	return spinning ? { k: "spinner", key: id, p: { ...p, style: "orbit" } }
		: cx.supports("icon") ? { k: "icon", key: id, p: { ...p, name: icon } } : { ...text(label, p), key: id };
}

/** Full project page state belongs to this view, not to preference or activity updates. */
export class GraphPage implements Component {
	readonly nativeOverlay = { role: "omp.overlay.semctl-graph", size: "lg" as const };
	private menu = false;
	private group: number;
	private selectedMenu: string;
	private menuRevision = 0;
	private diagnostics = false;
	private readonly expanded = new Set<string>();
	private scroll: NativeScroll = { by: "start", n: 0 };
	private offset = 0;
	private inspector: Component | undefined;
	private lastDisplay: Display | undefined;
	private disposed = false;
	private readonly unsubscribe: () => void;
	constructor(private readonly options: PageOptions) {
		this.group = Math.max(0, groups.findIndex(group => group.ids.some(id => id === options.operation.id)));
		this.selectedMenu = options.operation.id;
		this.unsubscribe = options.appearance.subscribe(() => { if (!this.disposed) options.requestRender(); });
		void options.appearance.load();
	}
	dispose(): void {
		if (this.disposed) return;
		this.disposed = true;
		this.unsubscribe();
		this.inspector?.dispose?.();
	}
	invalidate(): void { this.inspector?.invalidate?.(); }
	private togglePanel(panel: "menu" | "diagnostics"): void {
		const open = !this[panel];
		this.menu = panel === "menu" && open;
		this.diagnostics = panel === "diagnostics" && open;
		this.scroll = { by: "start", n: this.scroll.n + 1 };
		this.options.requestRender();
	}
	private groupOperations(): readonly Operation[] {
		return groups[this.group].ids.flatMap(id => this.options.operations.filter(operation => operation.id === id));
	}
	private chooseGroup(index: number): void {
		if (!groups[index]) return;
		this.group = index;
		if (!this.groupOperations().some(operation => operation.id === this.selectedMenu)) this.selectedMenu = this.groupOperations()[0].id;
		this.menuRevision++;
		this.options.requestRender();
	}
	private chooser(cx: DescribeContext): NativeNode {
		const operations = this.groupOperations();
		return { k: "col", key: "chooser", p: { gap: "xs", aria: "Choose read-only graph analysis" }, c: [
			cx.supports("tabs") ? { k: "tabs", key: "analysis-groups", p: {
				active: groups[this.group].id, items: groups.map(group => ({ id: group.id, label: group.label })), aria: "Analysis groups",
			} } : { k: "row", key: "analysis-groups", p: { gap: "sm", wrap: true, aria: "Analysis groups" }, c: groups.map((group, index) =>
				({ ...text(group.label, { tone: this.group === index ? "accent" : "muted", actions: { click: `analysis-group:${index}` },
					mark: this.group === index ? "pick" : undefined, aria: `${group.label} analyses${this.group === index ? ", selected" : ""}` }), key: group.id })) },
			cx.supports("list") ? { k: "list", key: "analyses", p: { selected: this.selectedMenu }, c: operations.map(operation => ({
				k: "item", key: operation.id, reveal: operation.id === this.selectedMenu ? { at: "nearest", n: this.menuRevision } : undefined,
				p: { label: operation.label, title: compactTooltip(operation.fields.length ? `Arguments: ${operation.fields.join(", ")}` : "No arguments") },
			})) } : { k: "col", c: operations.map(operation => ({ ...text(operation.label, {
				tone: this.selectedMenu === operation.id ? "accent" : "muted", actions: { click: `choose:${operation.id}` }, aria: `Run ${operation.label}`,
			}), key: operation.id })) },
		] };
	}
	private syncInspector(): void {
		const state = this.options.state();
		if (state.display !== this.lastDisplay) {
			this.inspector?.dispose?.();
			this.lastDisplay = state.display;
			this.inspector = state.display ? this.options.createInspector(state.display, this.options.requestRender, state.text, "overview") : undefined;
		}
		if (this.inspector && "setAppearance" in this.inspector && typeof this.inspector.setAppearance === "function") this.inspector.setAppearance(this.options.appearance.state.value);
		if (this.inspector && "setActivity" in this.inspector && typeof this.inspector.setActivity === "function") this.inspector.setActivity(this.options.activity().targets);
	}
	private section(cx: DescribeContext, key: string, head: string, body: string): NativeNode {
		const open = this.expanded.has(key);
		return cx.supports("section") ? { k: "section", key, p: { head, collapsible: true, collapsed: !open }, c: open ? [text(body)] : [] }
			: { k: "col", key, c: [text(head, { tone: "muted", actions: { click: `details:${key}` } }), ...(open ? [text(body)] : [])] };
	}
	describe(cx: DescribeContext): NativeNode {
		this.syncInspector();
		const { operation, cwd, appearance } = this.options, request = this.options.state(), activity = this.options.activity();
		const motion = appearance.state.value.motion !== "off" && !cx.reduceMotion;
		const out: NativeNode[] = [{ k: "row", key: "toolbar", p: { role: "semctl.graph.toolbar", gap: "sm", align: "center" }, c: [
			{ k: "col", key: "heading", p: { grow: 1, min: { w: 0 } }, c: [
				text(operation.label, { role: "semctl.graph.title", truncate: "end", wrap: "none", title: compactTooltip(`Read-only · configured semctl CLI · ${cwd}`) }),
				...(operation.id === "overview" ? [text(path.basename(cwd), {
					role: "semctl.graph.hint", tone: "muted", truncate: "end", wrap: "none", title: compactTooltip(cwd),
				})] : []),
			] },
			{ k: "row", p: { gap: "xs", shrink: 0, align: "center" }, c: [
				button(cx, "analyses", "diagram", "Analyses", "Choose graph analysis (A)", this.menu),
				button(cx, "appearance", "settings", "Appearance", appearance.state.error ? "Graph appearance (S)\nLocal preferences need attention · open for details" : "Graph appearance (S)"),
				button(cx, "refresh", "repeat", "Refresh", request.loading ? "Refreshing graph" : "Refresh graph (R)", false, request.loading, request.loading && motion),
				button(cx, "info", "info", "Details", "Graph details (I)\nProvenance and diagnostics", this.diagnostics),
				button(cx, "close", "x", "Close", "Close graph (Esc)"),
			] },
		] }];
		const reading = request.loading && !request.text && motion && cx.supports("spinner") && cx.supports("shimmer");
		const body: (NativeNode | Component)[] = this.menu ? [this.chooser(cx)] : this.diagnostics ? [] : this.inspector ? [this.inspector] :
			reading ? [{ k: "row", key: "reading", p: { role: "semctl.graph.loading", gap: "sm", align: "center" }, c: [
				{ k: "spinner", p: { style: "dots", tone: "muted", aria: "Reading indexed project map" } },
				{ k: "shimmer", p: { text: "Reading indexed project map", tone: "muted" } },
			] }]
			: [text(request.text || (request.loading ? "Reading indexed project map…" : "No result text returned."), { tone: request.loading ? "muted" : undefined })];
		if (this.diagnostics) {
			body.push(text(`Read-only · configured semctl CLI · ${cwd}`, { tone: "muted" }));
			if (activity.operations.length) body.push(text(`Observed semctx requests: ${activity.operations.join(", ")}`, { tone: "muted" }));
			if (request.error) body.push(text(request.error, { tone: "error" }));
			if (this.inspector) body.push(this.section(cx, "original", "Original CLI result", request.text));
			if (activity.producer) body.push(this.section(cx, "producer", "Producer observation", [activity.producer.text, activity.producer.details].filter(Boolean).join("\n")));
		}
		out.push({ k: "col", key: "body", p: { role: "semctl.graph.body", min: { h: "12lines" }, max: { h: "28lines" } }, scroll: this.scroll, c: body });
		out.push(graphStatus(cx, { request, activity, motion: appearance.state.value.motion }));
		out.push(text(this.menu ? "←/→ groups · ↑/↓ analyses · Enter run · Esc return to map"
			: this.diagnostics ? "PgUp/PgDn scroll · I or Esc return to map"
				: "↑/↓ nodes · PgUp/PgDn scroll · A analyses · S appearance · I details · R refresh · Esc close",
		{ role: "semctl.graph.hint", tone: "muted" }));
		return { k: "col", p: { gap: "xs" }, c: out };
	}
	render(width: number): readonly string[] {
		const state = this.options.state(), rows = [this.options.operation.label,
			...(this.menu ? [groups[this.group].label, ...this.groupOperations().map(operation => `${operation.id === this.selectedMenu ? ">" : " "} ${operation.label}`)] : []),
			...(state.loading ? [state.display ? "Refreshing · last available map retained" : "Reading indexed project map…"] : []),
			...(state.error ? [state.error] : []), ...clean(state.text).split(/\r?\n/),
			...(this.diagnostics ? [`Read-only · configured semctl CLI · ${this.options.cwd}`] : []),
		].flatMap(line => clean(line).split(/\r?\n/)).flatMap(line => line.length ? Array.from({ length: Math.ceil(line.length / Math.max(1, width)) }, (_, index) => line.slice(index * Math.max(1, width), (index + 1) * Math.max(1, width))) : [""]);
		this.offset = Math.max(0, Math.min(this.offset, rows.length - this.options.height()));
		return rows.slice(this.offset, this.offset + this.options.height());
	}
	handleNativeEvent(event: NativeUiEvent): void {
		if (this.disposed) return;
		if (event.type === "toggle") {
			const key = event.key.slice(event.key.lastIndexOf("/") + 1);
			if (key === "original" || key === "producer") { if (event.collapsed) this.expanded.delete(key); else this.expanded.add(key); this.options.requestRender(); return; }
		} else if (event.type === "select" && this.menu && event.key.endsWith("analysis-groups")) {
			this.chooseGroup(groups.findIndex(group => group.id === event.item));
			return;
		} else if ((event.type === "select" || event.type === "activate") && this.menu && this.options.operations.some(operation => operation.id === event.item)) {
			this.selectedMenu = event.item;
			if (event.type === "activate") this.options.close(event.item); else this.options.requestRender();
			return;
		} else if (event.type === "action") {
			if (event.act === "close") { this.options.close(); return; }
			if (event.act === "analyses") { this.togglePanel("menu"); return; }
			if (event.act === "appearance") { this.options.showAppearance(); return; }
			if (event.act === "info") { this.togglePanel("diagnostics"); return; }
			if (event.act === "refresh") { if (!this.options.state().loading) this.options.refresh(); return; }
			if (event.act.startsWith("analysis-group:") && this.menu) { this.chooseGroup(Number(event.act.slice(15))); return; }
			if (event.act.startsWith("choose:") && this.menu && this.options.operations.some(operation => operation.id === event.act.slice(7))) { this.options.close(event.act.slice(7)); return; }
			if (event.act.startsWith("details:")) { const key = event.act.slice(8); if (this.expanded.has(key)) this.expanded.delete(key); else this.expanded.add(key); this.options.requestRender(); return; }
		}
		this.inspector?.handleNativeEvent?.(event);
	}
	handleInput(data: string): void {
		if (this.disposed) return;
		const { matches, requestRender } = this.options;
		if (matches(data, "cancel")) {
			if (this.menu) this.togglePanel("menu");
			else if (this.diagnostics) this.togglePanel("diagnostics");
			else this.options.close();
			return;
		}
		if (data === "a" || data === "A") { this.togglePanel("menu"); return; }
		if (data === "s" || data === "S") { this.options.showAppearance(); return; }
		if (data === "i" || data === "I") { this.togglePanel("diagnostics"); return; }
		if ((data === "r" || data === "R") && !this.menu) { if (!this.options.state().loading) this.options.refresh(); return; }
		if (this.menu && matches(data, "confirm")) { this.options.close(this.selectedMenu); return; }
		if (this.menu && (this.options.appearanceKey?.(data, "left") || data === "\x1b[D" || this.options.appearanceKey?.(data, "right") || data === "\x1b[C" || data === "\t")) {
			const left = this.options.appearanceKey?.(data, "left") || data === "\x1b[D";
			this.chooseGroup((this.group + (left ? -1 : 1) + groups.length) % groups.length); return;
		}
		if (matches(data, "up") || matches(data, "down")) {
			const delta = matches(data, "up") ? -1 : 1;
			if (this.menu) {
				const operations = this.groupOperations(), at = operations.findIndex(operation => operation.id === this.selectedMenu);
				this.selectedMenu = operations[Math.max(0, Math.min(operations.length - 1, at + delta))].id;
				this.menuRevision++;
			} else if (!this.diagnostics && this.inspector && "move" in this.inspector && typeof this.inspector.move === "function") this.inspector.move(delta);
			else { this.offset += delta; this.scroll = { by: delta > 0 ? "line-down" : "line-up", n: this.scroll.n + 1 }; }
		} else if (matches(data, "pageUp") || matches(data, "pageDown")) {
			const down = matches(data, "pageDown"); this.offset += down ? this.options.height() : -this.options.height(); this.scroll = { by: down ? "page-down" : "page-up", n: this.scroll.n + 1 };
		} else return;
		requestRender();
	}
}
