import type { Component, DescribeContext, NativeNode, NativeUiEvent } from "@oh-my-pi/pi-tui";
import { GraphAppearanceStore, type GraphAppearance } from "./graph-appearance";
import { nativeElement, supportsElements } from "./native-elements";

/** Public matchesKey KeyIds, not KeybindingsManager action names. Inject OMP's parser for Kitty/remapped input. */
export type GraphAppearanceKeyMatcher = (data: string, key: "up" | "down" | "left" | "right" | "enter" | "escape") => boolean;

// A standalone panel still understands standard ANSI; the production host injects its stock key parser.
const ansiKeys: Record<Parameters<GraphAppearanceKeyMatcher>[1], readonly string[]> = {
	up: ["\x1b[A", "\x1bOA"], down: ["\x1b[B", "\x1bOB"], left: ["\x1b[D", "\x1bOD"],
	right: ["\x1b[C", "\x1bOC"], enter: ["\r", "\n"], escape: ["\x1b"],
};
const standardInput: GraphAppearanceKeyMatcher = (data, key) => ansiKeys[key].includes(data);

type AppearanceRow = { [K in keyof GraphAppearance]: {
	key: K; label: string; hint: string;
	options: readonly { value: GraphAppearance[K]; label: string; hint: string }[];
} }[keyof GraphAppearance];
const rows: readonly AppearanceRow[] = [
	{ key: "layout", label: "Layout", hint: "Choose a stable arrangement; activity and color do not move nodes.", options: [
		{ value: "force", label: "Force", hint: "Balance recorded links. Nearby nodes do not imply extra relationships." },
		{ value: "radial", label: "Radial", hint: "Rings show undirected hops from roots, or a high-degree node. Disconnected sets stay separate." },
		{ value: "layers", label: "Layers", hint: "Recorded direction runs left to right. Cycles share a column. Ordering reduces link crossings." },
		{ value: "components", label: "Components", hint: "Separate connected sets into blocks. Rows within a block do not imply direction or a hierarchy." },
	] },
	{ key: "color", label: "Color", hint: "Shared across all graph views. Colors identify categories, not similarity, confidence, or indexing status.", options: [
		{ value: "concepts", label: "Shared", hint: "One codebase palette: verified concepts, then source-directory colors." },
		{ value: "structure", label: "Structure", hint: "Source-directory colors shared across graph views. Mixed groups stay neutral." },
		{ value: "neutral", label: "Neutral", hint: "Quiet neutral nodes; status and selection remain visible." },
	] },
	{ key: "detail", label: "Detail", hint: "Overview maps only. Folded groups keep every indexed node reachable; graph evidence is unchanged.", options: [
		{ value: "calm", label: "Calm", hint: "Overview maps: up to 12 visible groups and 24 link routes. Exact node navigation stays available." },
		{ value: "balanced", label: "Balanced", hint: "Overview maps: up to 16 visible groups and 48 link routes. Exact node navigation stays available." },
		{ value: "detailed", label: "Detailed", hint: "Overview maps: up to 24 visible groups and 72 link routes. Exact node navigation stays available." },
	] },
	{ key: "labels", label: "Labels", hint: "Names remain available in node tooltips and group exploration.", options: [
		{ value: "key", label: "Key", hint: "Show the most useful labels without crowding the map." },
		{ value: "more", label: "More", hint: "Show more visible group names." },
		{ value: "hover", label: "On hover", hint: "Keep the map quiet; inspect names by hovering or selecting." },
	] },
	{ key: "links", label: "Links", hint: "Change contrast, never connections or direction.", options: [
		{ value: "soft", label: "Soft", hint: "Subtle relationship lines with preserved direction cues." },
		{ value: "clear", label: "Clear", hint: "Higher-contrast relationship lines with the same direction cues." },
	] },
	{ key: "motion", label: "Motion", hint: "System respects Reduce Motion; Off disables graph animation.", options: [
		{ value: "system", label: "System", hint: "Follow the terminal's reduced-motion preference." },
		{ value: "off", label: "Off", hint: "Disable graph animation regardless of the system preference." },
	] },
];

/** Compact native popup. The host owns its NativeSurface and calls dispose when the popup closes. */
export class GraphAppearancePanel implements Component {
	readonly nativeOverlay = { role: "omp.overlay.semctl-appearance", size: "sm" as const, head: "Graph appearance" };
	private selectedRow = 0;
	private reveal = 0;
	private disposed = false;
	private readonly unsubscribe: () => void;

	constructor(private readonly store: GraphAppearanceStore, private readonly requestRender: () => void,
		private readonly close: () => void, private readonly matches: GraphAppearanceKeyMatcher = standardInput) {
		this.unsubscribe = store.subscribe(() => { if (!this.disposed) this.requestRender(); });
		void store.load();
	}
	dispose(): void {
		if (this.disposed) return;
		this.disposed = true;
		this.unsubscribe();
	}

	private choose(index: number, value: string): void {
		const row = rows[index];
		if (!row || !row.options.some(option => option.value === value)) return;
		this.selectedRow = index;
		void this.store.update({ [row.key]: value } as Partial<GraphAppearance>);
	}
	private actions(): readonly string[] {
		return this.store.state.error && !this.store.state.saving ? ["retry", "reset", "close"] : ["reset", "close"];
	}
	private activate(action: string): void {
		if (action === "close") this.close();
		else if (action === "reset") { this.selectedRow = 0; void this.store.reset(); }
		else if (action === "retry") void this.store.retry();
		else {
			const match = /^appearance:([a-z]+):([a-z]+)$/.exec(action);
			if (match) this.choose(rows.findIndex(row => row.key === match[1]), match[2]);
		}
	}
	handleNativeEvent(event: NativeUiEvent): void {
		if (this.disposed) return;
		if (event.type === "action") this.activate(event.act);
		else if (event.type === "activate") this.activate(event.item);
	}
	handleInput(data: string): void {
		if (this.disposed) return;
		if (this.matches(data, "escape")) { this.close(); return; }
		const actions = this.actions();
		this.selectedRow = Math.min(this.selectedRow, rows.length + actions.length - 1);
		if (this.matches(data, "up") || this.matches(data, "down")) {
			this.selectedRow = Math.max(0, Math.min(rows.length + actions.length - 1,
				this.selectedRow + (this.matches(data, "up") ? -1 : 1)));
			this.reveal++;
			this.requestRender();
			return;
		}
		const enter = this.matches(data, "enter");
		if (this.selectedRow >= rows.length) {
			if (enter) this.activate(actions[this.selectedRow - rows.length]);
			return;
		}
		if (!enter && !this.matches(data, "left") && !this.matches(data, "right")) return;
		const row = rows[this.selectedRow];
		const current = row.options.findIndex(option => option.value === this.store.state.value[row.key]);
		const delta = this.matches(data, "left") ? -1 : 1;
		this.choose(this.selectedRow, row.options[(current + delta + row.options.length) % row.options.length].value);
	}

	private help(): string {
		const row = rows[this.selectedRow];
		if (row) {
			const option = row.options.find(option => option.value === this.store.state.value[row.key])!;
			return `${row.label} · ${option.label}: ${option.hint}\n${row.hint}`;
		}
		const action = this.actions()[this.selectedRow - rows.length];
		return action === "reset" ? "Reset all graph appearance preferences to defaults."
			: action === "retry" ? "Retry loading or saving local appearance preferences." : "Close graph appearance. Current choices stay active.";
	}

	private button(cx: DescribeContext, action: string, label: string, title: string, selected = false, active = false): NativeNode {
		const props = { title: selected ? undefined : title, aria: title, actions: { click: action }, grow: 1, min: { w: 0 } };
		if (supportsElements(cx)) return nativeElement({ k: "el", key: action, p: {
			...props, tag: "button", text: label, class: "semctl-appearance-option",
			attrs: { "aria-pressed": selected, "data-active": active, "data-selected": selected },
		} });
		return { k: "item", key: action, p: { ...props, label: `${selected ? "✓ " : ""}${label}`, tone: selected || active ? "accent" : "muted" } };
	}
	describe(cx: DescribeContext): NativeNode {
		const { value, ready, saving, error } = this.store.state;
		const actions = this.actions();
		this.selectedRow = Math.min(this.selectedRow, rows.length + actions.length - 1);
		const controls = rows.map((row, index): NativeNode => ({
			k: "col", key: row.key, reveal: this.selectedRow === index ? { at: "nearest", n: this.reveal } : undefined,
			p: { role: "semctl.appearance.row", gap: "xs", aria: row.label }, c: [
				{ k: "text", p: { role: "semctl.appearance.label", text: row.label, tone: this.selectedRow === index ? "accent" : "muted" } },
				{ k: "row", p: { role: row.key === "layout" ? "semctl.appearance.layouts" : "semctl.appearance.options", gap: "xs", wrap: true, aria: row.label }, c: row.options.map(option => this.button(cx,
					`appearance:${row.key}:${option.value}`, option.label, `${row.label} · ${option.label}\n${option.hint}`,
					value[row.key] === option.value, this.selectedRow === index && value[row.key] === option.value)) },
			],
		}));
		const status = !ready ? "Loading saved preferences…" : saving ? "Saving appearance…" : error ? "Using in-memory appearance"
			: !this.store.persistent ? "In memory for this session" : this.store.saved ? "Saved locally" : "Defaults · saved when changed";
		return { k: "col", p: { role: "semctl.appearance", gap: "sm" }, c: [
			{ k: "text", p: { text: "Live + project graph · shared color in all graphs", tone: "muted", wrap: "word" } },
			{ k: "col", key: "body", p: { role: "semctl.appearance.body", gap: "sm", max: { h: "22lines" } }, c: [
				...controls,
				{ k: "text", p: { text: this.store.persistent ? "This OMP profile · no server changes" : "This session · no server changes", tone: "muted", wrap: "word" } },
				...(error ? [{ k: "text" as const, key: "error", p: { role: "semctl.appearance.error", text: error, tone: "error" as const, wrap: "word" as const, aria: error } }] : []),
			] },
			{ k: "text", key: "help", p: { role: "semctl.appearance.help", text: this.help(), tone: "muted", wrap: "word", aria: this.help() } },
			{ k: "text", key: "save-state", p: { role: "semctl.appearance.status", text: status, tone: error ? "error" : "muted", aria: status } },
			{ k: "row", key: "actions", p: { gap: "xs", wrap: true }, c: actions.map((action, index) => ({
				...this.button(cx, action, action === "reset" ? "Reset defaults" : action === "close" ? "Close" : "Retry",
					action === "reset" ? "Reset all graph appearance preferences to defaults" : action === "close" ? "Close graph appearance" : "Retry loading or saving local appearance preferences",
					false, this.selectedRow === rows.length + index),
				reveal: this.selectedRow === rows.length + index ? { at: "nearest" as const, n: this.reveal } : undefined,
			})) },
			{ k: "text", p: { text: "↑↓ rows · ←→ / Enter choices · Esc close", tone: "muted", wrap: "word" } },
		] };
	}
	render(width: number): readonly string[] {
		const actions = this.actions();
		return ["Graph appearance", ...rows.map((row, index) => `${this.selectedRow === index ? "> " : "  "}${row.label}: ${row.options.find(option => option.value === this.store.state.value[row.key])!.label}`),
			...(this.store.state.error ? [this.store.state.error] : []),
			...actions.map((action, index) => `${this.selectedRow === rows.length + index ? "> " : "  "}${action === "reset" ? "Reset defaults" : action === "close" ? "Close" : "Retry"}`),
			this.help(),
			"↑↓ rows · ←→ / Enter choices · Esc close"].map(line => line.slice(0, Math.max(0, width)));
	}
}
