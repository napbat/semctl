import type { Component, DescribeContext, NativeNode, NativeUiEvent } from "@oh-my-pi/pi-tui";
import { conceptRevision, subscribeConcepts } from "./concept-colors";
import { projectSummary, type GraphGroup, type ProjectSummary } from "./graph-projection";
import { graphPreview, type GraphFeedback, type GraphPreviewSize } from "./graph-preview";
import { nativeGraphLayout } from "./graph-native-layout";
import { exactActivity } from "./graph-data/layout.js";
import type { GraphActivity } from "./graph-activity";
import type { Display } from "./inspector";
import type { GraphRequestState } from "./overview";
import { nativeElement, supportsElements } from "./native-elements";
import { indexingCounts, indexingParts, indexingSummary, type GraphIndexingCounts } from "./graph-indexing";
import { DEFAULT_GRAPH_APPEARANCE, type GraphAppearance } from "./graph-appearance";
import { graphColorRevision, lookupGraphColor } from "./graph-colors";
import { nativeGraphColorCue } from "./native-concepts";
import { graphStatus } from "./graph-status";
import { boundedTooltip, compactTooltip, type TooltipLine } from "./tooltip-text";

const clean = (value: string) => value.replace(/[\u0000-\u0008\u000b-\u001f\u007f-\u009f]/g, "");
const text = (value: string, props: Record<string, unknown> = {}): NativeNode => ({ k: "text", p: { text: clean(value), ...props } });
type Overview = Extract<Display, { kind: "graph" }>;

export interface LiveGraphState {
	request: GraphRequestState;
	activity: GraphActivity;
	appearance?: Readonly<GraphAppearance>;
	appearanceError?: string;
}
type PopupSize = GraphAppearance["popup"];
interface Actions { close(): void; refresh(): void; expand(): void; appearance(): void; resize(size: PopupSize): void }

/**
 * Stepped popup sizes, because Tern sends no drag events. Only the preview size reaches the layout,
 * so a size step never moves dots within one preview size.
 */
const POPUP_SIZES: readonly PopupSize[] = ["small", "medium", "large"];
const popupSizes: { readonly [K in PopupSize]: { label: string; hint: string; overlay: "sm" | "md" | "lg"; preview: GraphPreviewSize; chips: number } } = {
	small: { label: "Small", hint: "Click to enlarge · Shift+click for large", overlay: "sm", preview: "compact", chips: 3 },
	medium: { label: "Medium", hint: "Click to enlarge · Shift+click to shrink", overlay: "md", preview: "inline", chips: 4 },
	large: { label: "Large", hint: "Click for small · Shift+click to shrink", overlay: "lg", preview: "overview", chips: 5 },
};

/** Overlay props of the live popup widget. Sheet rules keyed on Tern's `sz-*` class set the width and height. */
export function livePopupOverlay(size: PopupSize, hidden: boolean) {
	return { role: "omp.overlay.semctl-live", modal: false, size: popupSizes[size].overlay, anchor: "top" as const, hidden, min: { w: 0 } };
}

interface ButtonOptions { disabled?: boolean; busy?: boolean; role?: string }
/** A busy button stays disabled and shows a native spinner. Callers set busy only when motion is allowed. */
function actionButton(cx: DescribeContext, id: string, icon: "expand" | "repeat" | "x" | "settings" | "grip", label: string, title: string,
	{ disabled = false, busy = false, role }: ButtonOptions = {}): NativeNode {
	const actions = disabled ? undefined : { click: id };
	const spinning = busy && cx.supports("spinner");
	if (supportsElements(cx) && cx.feature("styles")) return nativeElement({
		k: "el", key: id, p: {
			tag: "button", class: "semctl-icon-action", role, actions, title, aria: title,
			attrs: { "aria-disabled": disabled, "aria-busy": spinning },
		}, c: [spinning ? { k: "spinner", p: { style: "orbit", aria: "Refreshing" } } : cx.supports("icon") ? { k: "icon", p: { name: icon } } : text(label)],
	});
	const props = { role, actions, title, aria: title, tone: "muted" as const, shrink: 0 };
	return spinning ? { k: "spinner", key: id, p: { ...props, style: "orbit" } }
		: cx.supports("icon") ? { k: "icon", key: id, p: { ...props, name: icon } }
			: { ...text(label, props), key: id };
}

function operationLabel(operation: string): string {
	const label = clean(operation).replace(/^mcp__semctx_/, "").replace(/[_-]+/g, " ").trim();
	return label ? label[0].toUpperCase() + label.slice(1) : "Request";
}

function groupBadge(cx: DescribeContext, kind: "status" | "observation", label: string, tone: "error" | "success" | "muted", title: string): NativeNode {
	const props = { role: `semctl.live.group.${kind}`, title, shrink: 0 };
	return supportsElements(cx) && cx.feature("styles")
		? nativeElement({ k: "el", key: kind, p: { ...props, tag: "span", text: label, class: `semctl-live-badge-${tone}` } })
		: text(label, { ...props, tone });
}

const plural = (count: number, noun: string) => `${count} ${noun}${count === 1 ? "" : "s"}`;
/** Screen-reader text keeps the scope and caveats that hover text leaves out, as one bounded line. */
const spoken = (...lines: TooltipLine[]) => boundedTooltip(lines, 4).split("\n").join(" · ");
const CATALOG_STATES = "Catalog states · pending is not active work";
/**
 * Shares use reported catalog states only, so absent metadata never lowers the ready share.
 * A labelled meter sits in a row with its ready share as text, because Tern can omit the label of a meter with three or more parts.
 */
function indexingMeter(cx: DescribeContext, key: string, counts: GraphIndexingCounts, props: Record<string, unknown>, labelled = false): NativeNode | undefined {
	const parts = cx.supports("meter") ? indexingParts(counts) : undefined, summary = indexingSummary(counts);
	if (!parts || !summary) return undefined;
	const reported = counts.ready + counts.processing + counts.pending + counts.failed + counts.unknown, title = compactTooltip(summary, CATALOG_STATES);
	const meter: NativeNode = { k: "meter", key, p: { value: counts.ready / reported, style: "bar", size: "sm", parts, title, aria: summary, ...props } };
	return labelled ? { k: "row", key: `${key}-row`, p: { role: "semctl.live.indexing-row", gap: "sm", align: "center" }, c: [meter,
		text(`${Math.floor(counts.ready * 100 / reported)}% ready`, { role: "semctl.live.indexing-label", tone: "muted", shrink: 0, wrap: "none", title })] } : meter;
}
/** Tinted areas in the drawing: doc concept families, or shared identifier terms when no family has enough drawn members. */
type Tint = "family" | "lexical";
const LEGEND = {
	size: ["files", "Dot size = member nodes, mostly files"],
	weight: ["links", "Line weight = links between groups"],
} as const;
const TINT_LEGEND = {
	family: ["concepts", "Tint = doc concept family"],
	lexical: ["shared terms", "Tint = shared identifier terms"],
} as const;
/** Styled hosts draw small glyphs that match the graph marks; other hosts read one plain line. The tint item shows only when the drawing has tinted areas. */
function legend(cx: DescribeContext, tint: Tint | undefined): NativeNode {
	const marks: (readonly [key: string, label: string, title: string])[] = [["size", ...LEGEND.size], ["weight", ...LEGEND.weight]];
	if (tint) marks.push(["tint", ...TINT_LEGEND[tint]]);
	const props = { role: "semctl.live.legend", title: compactTooltip("Dot size = member nodes · line weight = links between groups", tint && TINT_LEGEND[tint][1]) };
	if (!supportsElements(cx) || !cx.feature("styles")) return text(`Dot size = files · line weight = links${tint ? ` · tint = ${TINT_LEGEND[tint][0]}` : ""}`, {
		...props, grow: 1, min: { w: 0 }, tone: "muted", wrap: "none", truncate: "end" });
	return nativeElement({ k: "el", key: "legend", p: { ...props, tag: "div", class: "semctl-live-legend",
		aria: `Legend: dot size shows files, line weight shows links${tint ? `, tint shows ${tint === "family" ? "doc concept families" : "shared identifier terms"}` : ""}` },
	c: marks.map(([key, label, title]) => nativeElement({ k: "el", key, p: { tag: "span", class: "semctl-legend-item", title }, c: [
		nativeElement({ k: "el", key: "glyph", p: { tag: "span", class: `semctl-legend-${key}`, attrs: { "aria-hidden": true } } }),
		nativeElement({ k: "el", key: "label", p: { tag: "span", class: "semctl-legend-label", text: label } }),
	] })) });
}
/** Identity, catalog state and the naming doc. The row, chip or drawing shows the rest. */
function groupTooltip(group: GraphGroup): string {
	const files = group.members.filter(node => node.category === "file").length;
	return compactTooltip(`${group.label} · ${files ? plural(files, "file") : plural(group.members.length, "node")}`,
		indexingSummary(group.indexing), group.source && `From ${group.source}`);
}
function sourceScope(display: Overview): string {
	return display.loading ? "Relationship layers are loading; observed links only."
		: display.truncated ? "Partial indexed map; observed links only." : "Links in the supplied indexed map.";
}
interface GroupNeighbor { group: GraphGroup; incoming: number; outgoing: number }
interface GroupLinks { incoming: number; outgoing: number; internal: number; neighbors: GroupNeighbor[] }
/** Count source edges once, before route limits or aggregate labels can hide evidence. */
function groupLinks(display: Overview, summary: ProjectSummary): Map<string, GroupLinks> {
	const links = new Map(summary.groups.map(group => [group.id, { incoming: 0, outgoing: 0, internal: 0, neighbors: [] } as GroupLinks]));
	const groups = new Map(summary.groups.map(group => [group.id, group]));
	const neighbors = new Map(summary.groups.map(group => [group.id, new Map<string, GroupNeighbor>()]));
	const neighbor = (id: string, other: string): GroupNeighbor => {
		const nearby = neighbors.get(id)!;
		let value = nearby.get(other);
		if (!value) { value = { group: groups.get(other)!, incoming: 0, outgoing: 0 }; nearby.set(other, value); }
		return value;
	};
	for (const edge of display.edges) {
		const from = summary.groupOf.get(edge.from), to = summary.groupOf.get(edge.to);
		if (from === undefined || to === undefined) continue;
		const source = links.get(from)!, target = links.get(to)!;
		if (from === to) { source.internal++; continue; }
		source.outgoing++; target.incoming++;
		neighbor(from, to).outgoing++;
		neighbor(to, from).incoming++;
	}
	for (const [id, nearby] of neighbors) links.get(id)!.neighbors = [...nearby.values()].sort((a, b) =>
		b.incoming + b.outgoing - a.incoming - a.outgoing || (a.group.id < b.group.id ? -1 : a.group.id > b.group.id ? 1 : 0));
	return links;
}

const sameTargets = (a: readonly string[], b: readonly string[]) => a === b || a.length === b.length && a.every((target, index) => target === b[index]);
function sameObservations(a: GraphActivity, b: GraphActivity): boolean {
	if (!sameTargets(a.targets, b.targets)) return false;
	const before = a.recent ?? [], after = b.recent ?? [];
	return before === after || before.length === after.length && before.every((receipt, index) => {
		const next = after[index];
		return receipt.id === next.id && receipt.outcome === next.outcome && sameTargets(receipt.targets, next.targets);
	});
}

/** Passive native UI. The renderer owns motion; this component never animates or takes keyboard focus. */
export class LiveGraphPopup implements Component {
	private display?: Overview;
	private summary?: ProjectSummary;
	private links = new Map<string, GroupLinks>();
	private revision = -1;
	private detail: GraphAppearance["detail"] | undefined;
	private groupsOpen = false;
	private diagnosticsOpen = false;
	private selected: string | undefined;
	private activityDirty = true;
	private readonly active = new Set<string>();
	private readonly feedback = new Map<string, GraphFeedback>();
	private observationKey = "";
	private diagramKey: string | undefined;
	private diagram: NativeNode | undefined;
	private tint: Tint | undefined;
	private readonly unsubscribe: () => void;
	private readonly diagramAction = (id: string): string | undefined =>
		this.summary?.groups.some(group => group.id === id) ? this.groupAction(id) : undefined;
	private groupAction(id: string): string { return `group:${JSON.stringify([this.display?.identity.codebaseId, id])}`; }
	private synchronizeSummary(): void {
		const display = this.state.request.display?.kind === "graph" ? this.state.request.display : undefined;
		const revision = conceptRevision(), detail = (this.state.appearance ?? DEFAULT_GRAPH_APPEARANCE).detail;
		if (display === this.display && this.revision === revision && this.detail === detail) return;
		if (display?.identity.codebaseId !== this.display?.identity.codebaseId) this.selected = undefined;
		this.display = display; this.revision = revision; this.detail = detail;
		this.summary = display ? projectSummary(display, display.identity.codebaseId, detail) : undefined;
		this.links = display && this.summary ? groupLinks(display, this.summary) : new Map();
		if (!this.summary?.groups.some(group => group.id === this.selected)) this.selected = undefined;
		this.activityDirty = true;
		this.diagramKey = undefined;
	}
	constructor(private state: LiveGraphState, private readonly requestRender: () => void, private readonly actions: Actions) {
		this.unsubscribe = subscribeConcepts(this, popup => popup.requestRender());
	}
	update(state: LiveGraphState): void {
		this.activityDirty ||= !sameObservations(this.state.activity, state.activity);
		this.state = state;
		this.requestRender();
	}
	dispose(): void { this.unsubscribe(); }
	handleNativeEvent(event: NativeUiEvent): void {
		if (event.type === "toggle" && event.key.endsWith("groups")) { this.groupsOpen = !event.collapsed; this.requestRender(); }
		else if (event.type === "toggle" && event.key.endsWith("diagnostics")) { this.diagnosticsOpen = !event.collapsed; this.requestRender(); }
		else if (event.type === "action") {
			if (event.act === "close") this.actions.close();
			else if (event.act === "refresh") this.actions.refresh();
			else if (event.act === "expand") this.actions.expand();
			else if (event.act === "appearance") this.actions.appearance();
			else if (event.act === "resize" || event.act === "resize:toggle") {
				// Click steps up and Shift+click steps down, both wrapping; a title double-click toggles small and large.
				const size = (this.state.appearance ?? DEFAULT_GRAPH_APPEARANCE).popup;
				const step = Array.isArray(event.mods) && event.mods.includes("shift") ? -1 : 1;
				this.actions.resize(event.act === "resize:toggle" ? size === "large" ? "small" : "large"
					: POPUP_SIZES[(POPUP_SIZES.indexOf(size) + step + POPUP_SIZES.length) % POPUP_SIZES.length]);
			} else if (event.act.startsWith("group:")) {
				this.synchronizeSummary();
				let identity: unknown;
				try { identity = JSON.parse(event.act.slice(6)); } catch { return; }
				if (!Array.isArray(identity) || identity.length !== 2 || identity[0] !== this.display?.identity.codebaseId || typeof identity[1] !== "string") return;
				const id: string = identity[1], group = this.summary?.groups.find(group => group.id === id);
				if (group) { this.selected = this.selected === group.id ? undefined : group.id; this.requestRender(); }
			} else if (event.act === "clear-group") { this.selected = undefined; this.requestRender(); }
		}
	}
	private groupCue(cx: DescribeContext, group: GraphGroup): NativeNode | undefined {
		const node = this.summary?.graph.nodes.find(node => node.id === group.id);
		const color = node && lookupGraphColor(this.display?.identity.codebaseId ?? null, node, (this.state.appearance ?? DEFAULT_GRAPH_APPEARANCE).color);
		const cue = color && nativeGraphColorCue(color, cx, undefined, `group-color-${group.id}`);
		return cue ? nativeElement({ k: "el", key: cue.key, p: { ...cue.p, title: compactTooltip(cue.p.title) } }) : undefined;
	}
	/** A compact group control shared by the selection's neighbors and the busiest groups. Busiest chips show only the last path segment. */
	private groupChip(cx: DescribeContext, kind: "neighbor" | "busiest", group: GraphGroup, title: string, aria: string, count?: number): NativeNode {
		const cue = this.groupCue(cx, group), key = `${kind}-${group.id}`;
		const props = { role: `semctl.live.${kind}`, actions: { click: this.groupAction(group.id) }, title, aria,
			grow: 1, basis: 0, min: { w: 0 }, gap: "xs" as const, align: "center" as const };
		const label = kind === "busiest" ? group.label.split("/").filter(Boolean).at(-1) ?? group.label : group.label;
		const children = [...(cue ? [cue] : []), text(label, { grow: 1, min: { w: 0 }, wrap: "none", truncate: "end" }),
			...(count !== undefined ? [text(String(count), { tone: "muted", shrink: 0 })] : [])];
		return supportsElements(cx) && cx.feature("styles")
			? nativeElement({ k: "el", key, p: { ...props, tag: "button", class: "semctl-live-neighbor" }, c: children })
			: { k: "row", key, p: props, c: children };
	}
	/** `chips` bounds the neighbor and busiest chips; wider popups show more. */
	private selectionInspector(cx: DescribeContext, display: Overview, tint: Tint | undefined, chips: number): NativeNode {
		const selected = this.summary?.groups.find(group => group.id === this.selected);
		const links = selected && this.links.get(selected.id), cue = selected && this.groupCue(cx, selected);
		const evidence = links && compactTooltip("Source links counted once, including parallel links and routes not drawn");
		const evidenceAria = links && spoken(`${links.incoming} incoming links · ${links.outgoing} outgoing links · ${links.internal} internal links`,
			sourceScope(display), ...display.caveats.slice(0, 2));
		const neighbors = links?.neighbors.slice(0, chips).map(neighbor => this.groupChip(cx, "neighbor", neighbor.group,
			compactTooltip(neighbor.group.label, `${neighbor.incoming} into selected · ${neighbor.outgoing} from selected`),
			`Inspect ${clean(neighbor.group.label).slice(0, 160)} · ${neighbor.incoming} incoming links · ${neighbor.outgoing} outgoing links relative to selected group`,
			neighbor.incoming + neighbor.outgoing)) ?? [];
		// Without a selection, the groups with the most cross-group links fill the fixed third row.
		const busiest = selected ? [] : (this.summary?.groups ?? []).filter(group => group.id !== "other-groups").flatMap(group => {
			const { incoming, outgoing } = this.links.get(group.id)!;
			return incoming + outgoing ? [{ group, incoming, outgoing }] : [];
		}).sort((a, b) => b.incoming + b.outgoing - a.incoming - a.outgoing || (a.group.id < b.group.id ? -1 : a.group.id > b.group.id ? 1 : 0)).slice(0, chips)
			.map(({ group, incoming, outgoing }) => this.groupChip(cx, "busiest", group,
				compactTooltip(group.label, `${incoming + outgoing} cross-group links · ${incoming} incoming · ${outgoing} outgoing`),
				`Inspect ${clean(group.label).slice(0, 160)} · ${incoming + outgoing} cross-group links · ${incoming} incoming links · ${outgoing} outgoing links`));
		return { k: "col", key: "selection", p: { role: "semctl.live.selection", gap: "xs", min: { h: "3.75lines" } }, c: [
			{ k: "row", key: "identity", p: { gap: "xs", align: "center", min: { h: "1.25lines" } }, c: [
				...(cue ? [cue] : []),
				text(selected?.label ?? "Select a group to inspect links", {
					grow: 1, min: { w: 0 }, tone: selected ? "accent" : "muted", wrap: "none", truncate: "end",
					title: selected ? groupTooltip(selected) : compactTooltip("Select a dot, chip or group row; select it again to clear"),
				}),
				...(selected ? [text(`${selected.members.length} nodes`, { tone: "muted", shrink: 0, aria: `${selected.members.length} member nodes`, title: groupTooltip(selected) }),
					actionButton(cx, "clear-group", "x", "Clear", "Clear highlighted graph group")] : []),
			] },
			{ k: "row", key: "links", p: { gap: "xs", align: "center", min: { h: "1lines" } }, c: [
				links ? text(`${links.incoming} in · ${links.outgoing} out · ${links.internal} internal`, {
					role: "semctl.live.links", grow: 1, min: { w: 0 }, tone: "muted", wrap: "none", truncate: "end", title: evidence, aria: evidenceAria,
				}) : legend(cx, tint),
				...(selected && (display.loading || display.truncated) ? [text(display.loading ? "loading" : "partial", {
					tone: display.loading ? "muted" : "warning", shrink: 0, title: compactTooltip(sourceScope(display), display.caveats[0]),
					aria: spoken(sourceScope(display), ...display.caveats.slice(0, 2)),
				})] : []),
			] },
			{ k: "row", key: "neighbors", p: { gap: "xs", align: "center", min: { h: "1.25lines" } }, c: selected ? [
				...neighbors,
				...(!neighbors.length ? [text(display.loading ? "No neighboring groups observed yet"
					: display.truncated ? "No neighboring groups in this partial map" : "No neighboring groups in this map", {
						tone: "muted", grow: 1, min: { w: 0 }, wrap: "none", truncate: "end", title: evidence,
					})] : []),
				...(links && links.neighbors.length > neighbors.length ? [text(`+${links.neighbors.length - neighbors.length}`, {
					tone: "muted", shrink: 0, title: compactTooltip(plural(links.neighbors.length, "neighboring group"), "Open Overview for all relationships"),
				})] : []),
			] : busiest.length ? [{ ...text("Busiest:", { tone: "muted", shrink: 0,
				title: compactTooltip("Groups with the most cross-group links, counted once") }), key: "busiest-label" }, ...busiest]
				: [text(display.loading ? "No cross-group links observed yet"
					: display.truncated ? "No cross-group links in this partial map" : "No cross-group links in this map", {
						tone: "muted", grow: 1, min: { w: 0 }, wrap: "none", truncate: "end", title: compactTooltip(display.caveats[0]) || undefined,
					})] },
		] };
	}
	private groupRow(cx: DescribeContext, group: GraphGroup, motion: boolean): NativeNode {
		const observation = this.active.has(group.id) ? "active" : this.feedback.get(group.id)?.outcome;
		const indexing = indexingSummary(group.indexing), cue = this.groupCue(cx, group);
		const meter = indexingMeter(cx, "indexing", group.indexing, { role: "semctl.live.group.indexing", min: { w: "6ch" }, max: { w: "6ch" }, shrink: 0 });
		const status = (["failed", "processing", "pending", "unknown", "ready"] as const).find(state => group.indexing[state] > 0);
		const props = { role: "semctl.live.group", gap: "xs" as const, align: "center" as const, actions: { click: this.groupAction(group.id) },
			aria: `${this.selected === group.id ? "Selected: " : "Inspect "}${clean(group.label).slice(0, 160)}, ${group.members.length} nodes${indexing ? `, ${indexing}` : ""}${observation ? `, ${observation}` : ""}`,
			title: groupTooltip(group) };
		const children = [...(cue ? [cue] : []), text(group.label, {
			grow: 1, min: { w: 0 }, tone: this.selected === group.id ? "accent" : "muted", truncate: "end", wrap: "none",
		}), ...(meter ? [meter] : status ? [groupBadge(cx, "status", `${group.indexing[status]} ${status}`, status === "failed" ? "error" : "muted",
			compactTooltip(indexing, CATALOG_STATES))] : []),
		...(observation ? [groupBadge(cx, "observation", observation, observation === "failed" ? "error" : "success",
			compactTooltip(observation === "active" ? "An active request names a member of this group." : `A recent request naming a group member ${observation}.`))] : []),
		text(String(group.members.length), { role: "semctl.live.group.count", tone: "muted", shrink: 0, aria: `${group.members.length} nodes` })];
		return supportsElements(cx) && cx.feature("styles")
			? nativeElement({ k: "el", key: `group-${group.id}`, p: { ...props, tag: "button", class: "semctl-live-group",
				attrs: { "aria-pressed": this.selected === group.id, "data-motion": motion ? "full" : "reduced" } }, c: children })
			: { k: "row", key: `group-${group.id}`, p: props, c: children };
	}
	describe(cx: DescribeContext): NativeNode {
		const { request, activity } = this.state;
		const appearance = this.state.appearance ?? DEFAULT_GRAPH_APPEARANCE, motion = appearance.motion !== "off" && !cx.reduceMotion;
		const size = popupSizes[appearance.popup];
		this.synchronizeSummary();
		const display = this.display;
		if (this.activityDirty) {
			this.active.clear(); this.feedback.clear(); this.activityDirty = false;
			if (display && this.summary) {
				const targets = new Set(activity.targets);
				const receipts = (activity.recent ?? []).map(receipt => ({ receipt, targets: new Set(receipt.targets) }));
				for (const node of display.nodes) {
					const group = this.summary.groupOf.get(node.id);
					if (group === undefined) continue;
					if (exactActivity(node, targets, display.identity.codebaseId)) this.active.add(group);
					for (const { receipt, targets } of receipts) {
						if ((this.feedback.get(group)?.id ?? -1) >= receipt.id || !exactActivity(node, targets, display.identity.codebaseId)) continue;
						this.feedback.set(group, { id: receipt.id, outcome: receipt.outcome });
					}
				}
			}
			this.observationKey = JSON.stringify([[...this.active].sort(), [...this.feedback].sort(([a], [b]) => a.localeCompare(b))]);
		}
		const caveats = this.summary?.graph.caveats ?? [];
		const partial = display?.truncated && !display.loading ? { tone: "warning" as const, shrink: 0,
			title: compactTooltip("Partial indexed map", caveats[0]), aria: spoken("Partial indexed map", ...caveats) } : undefined;
		const subline: NativeNode[] = display && this.summary ? [{ k: "row", key: "subline", p: { gap: "xs", align: "center" }, c: [
			text(`${plural(this.summary.files, "file")} · ${plural(this.summary.groups.length, "group")}`, {
				role: "semctl.live.subline", tone: "muted", min: { w: 0 }, wrap: "none", truncate: "end",
				title: compactTooltip(`${display.nodes.length} indexed nodes · ${this.summary.groups.length} groups`,
					`${display.edges.length} links · ${this.summary.drawnLinks} cross-group links drawn`),
			}),
			...(partial ? [cx.supports("icon") ? { k: "icon" as const, key: "partial", p: { ...partial, role: "semctl.live.partial", name: "warn" } }
				: { ...text("Partial map", partial), key: "partial" }] : []),
		] }] : [];
		// The grip sits at the top-left corner, the corner that moves: the popup is pinned at its bottom right edge.
		const header: NativeNode = { k: "col", key: "header", p: { role: "semctl.live.header" }, c: [
			{ k: "row", key: "toolbar", p: { role: "semctl.live.toolbar", gap: "sm", align: "center" }, c: [
				actionButton(cx, "resize", "grip", "Size", compactTooltip(`Popup size: ${size.label}`, size.hint), { role: "semctl.live.resize" }),
				text("Project map", { role: "semctl.live.title", grow: 1, min: { w: 0 }, wrap: "none", truncate: "end",
					actions: { dblclick: "resize:toggle" }, title: appearance.popup === "large" ? "Double-click for the small popup" : "Double-click for the large popup" }),
				{ k: "row", p: { gap: "xs", shrink: 0, align: "center" }, c: [
					actionButton(cx, "expand", "expand", "Overview", "Open full project overview"),
					actionButton(cx, "appearance", "settings", "Appearance",
						this.state.appearanceError ? compactTooltip("Graph appearance · preferences not saved", this.state.appearanceError) : "Graph appearance"),
					actionButton(cx, "refresh", "repeat", "Refresh", request.loading ? "Refreshing indexed project map" : "Refresh indexed project map",
						{ disabled: request.loading, busy: request.loading && motion }),
					actionButton(cx, "close", "x", "Close", "Close live overview"),
				] },
			] },
			...subline,
		] };
		const children: NativeNode[] = [];
		if (display && this.summary) {
			const counts = indexingCounts(display.nodes), indexing = indexingSummary(counts);
			const meter = indexingMeter(cx, "indexing", counts, { role: "semctl.live.indexing", grow: 1, min: { w: 0 } }, true);
			if (meter) children.push(meter);
			else if (indexing) children.push(text(indexing.replace(/^Indexing: /, ""), {
				role: "semctl.live.indexing", tone: "muted", wrap: "none", truncate: "end", title: compactTooltip(indexing, CATALOG_STATES),
			}));
			const diagramKey = JSON.stringify([this.observationKey, this.selected, cx.dark, cx.reduceMotion,
				supportsElements(cx), cx.feature("styles"), appearance, graphColorRevision()]);
			if (this.diagramKey !== diagramKey) {
				this.diagramKey = diagramKey;
				const preview = graphPreview(this.summary.graph, cx, {
					codebaseId: display.identity.codebaseId, activeIds: this.active, feedback: this.feedback,
					size: size.preview, selected: this.selected, nodeAction: this.diagramAction, appearance,
				});
				// Previews exist only on styled element hosts, so the well needs no fallback.
				this.diagram = preview && nativeElement({ k: "el", key: "graph-well", p: { tag: "div", class: "semctl-graph-well" }, c: [preview] });
				// The legend names a tint only when the drawing has tinted areas. The layout is cached, so this reads the drawn geometry.
				const territories = preview ? nativeGraphLayout(this.summary.graph, size.preview, appearance.layout).territories : [];
				this.tint = territories.some(territory => territory.kind === "family") ? "family" : territories.length ? "lexical" : undefined;
			}
			if (this.diagram) children.push(this.diagram);
			children.push(this.selectionInspector(cx, display, this.tint, size.chips));
			const groups = this.groupsOpen || !cx.supports("section") ? this.summary.groups.map(group => this.groupRow(cx, group, motion)) : [];
			if (cx.supports("section")) children.push({ k: "section", key: "groups", p: {
				head: `Explore ${this.summary.groups.length} groups`, collapsible: true, collapsed: !this.groupsOpen,
			}, c: this.groupsOpen ? groups : [] });
			else children.push(...groups);
		} else {
			const emptyText = request.error ? "The indexed map is unavailable" : request.loading ? "Reading the indexed map" : "No indexed map available";
			const reading = request.loading && !request.error && motion;
			const empty: NativeNode[] = [
				reading && cx.supports("spinner") ? { k: "spinner", key: "reading", p: { style: "dots", tone: "muted", aria: emptyText } }
					: cx.supports("icon") ? { k: "icon", p: { name: request.error ? "warn" : "diagram", tone: request.error ? "warning" : "muted" } } : text("○", { tone: "muted" }),
				reading && cx.supports("shimmer") ? { k: "shimmer", key: "reading-text", p: { text: emptyText, tone: "muted" } }
					: text(emptyText, { tone: "muted", title: request.error && compactTooltip(request.error) }),
				text(request.error ? "Refresh to try again" : "Available files appear before embedding completes", { tone: "muted" }),
			];
			children.push(supportsElements(cx) && cx.feature("styles")
				? nativeElement({ k: "el", key: "empty", p: { tag: "div", class: "semctl-live-empty" }, c: empty })
				: { k: "col", key: "empty", p: { min: { h: "9lines" }, gap: "sm" }, c: empty });
		}
		if (request.error) {
			const diagnostics = text(request.error, { role: "semctl.live.diagnostic-text", wrap: "word", tone: "muted" });
			children.push(cx.supports("section")
				? { k: "section", key: "diagnostics", p: { role: "semctl.live.diagnostics", head: "Diagnostics", collapsible: true, collapsed: !this.diagnosticsOpen },
					c: this.diagnosticsOpen ? [diagnostics] : [] }
				: diagnostics);
		}
		return { k: "col", p: { gap: "sm" }, c: [
			header, { k: "col", key: "body", p: { role: "semctl.live.body", gap: "sm" }, c: children },
			graphStatus(cx, { request, activity, motion: appearance.motion, presentation: "icon" }),
		] };
	}
	render(width: number): readonly string[] {
		this.synchronizeSummary();
		const selected = this.summary?.groups.find(group => group.id === this.selected), links = selected && this.links.get(selected.id);
		return ["semctl · Live project overview",
			...(this.summary ? [`${this.summary.files} files · ${this.summary.groups.length} groups`,
				selected ? `${selected.label} · ${selected.members.length} nodes` : "Select a group to inspect links"] : []),
			...(links && this.display ? [`${links.incoming} in · ${links.outgoing} out · ${links.internal} internal`, sourceScope(this.display),
				...links.neighbors.slice(0, 3).map(neighbor => `${neighbor.group.label} · ${neighbor.incoming} in · ${neighbor.outgoing} out`)] : []),
			this.state.activity.producer?.text ?? "No producer observation", ...this.state.activity.operations.map(operationLabel)]
			.map(line => clean(line).slice(0, Math.max(0, width)));
	}
}
