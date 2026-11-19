import type { Component, DescribeContext, NativeNode, NativeUiEvent } from "@oh-my-pi/pi-tui";
import { conceptRevision, subscribeConcepts } from "./concept-colors";
import { projectSummary, REMAINDER_GROUP, type GraphGroup, type GraphRegion, type ProjectSummary } from "./graph-projection";
import { graphPreview, type GraphFeedback, type GraphPreviewSize } from "./graph-preview";
import { nativeGraphLayout, type NativeGraphTerritory } from "./graph-native-layout";
import { exactActivity } from "./graph-data/layout.js";
import { conceptPalette } from "./graph-data/palette.js";
import type { GraphActivity } from "./graph-activity";
import type { Display } from "./inspector";
import type { GraphRequestState } from "./overview";
import { nativeElement, supportsElements } from "./native-elements";
import { indexingCounts, indexingParts, indexingSummary, indexingUnsettled, type GraphIndexingCounts } from "./graph-indexing";
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
function actionButton(cx: DescribeContext, id: string, icon: "expand" | "repeat" | "x" | "settings" | "grip" | undefined, label: string, title: string,
	{ disabled = false, busy = false, role }: ButtonOptions = {}): NativeNode {
	const actions = disabled ? undefined : { click: id };
	const spinning = busy && cx.supports("spinner");
	if (supportsElements(cx) && cx.feature("styles")) return nativeElement({
		k: "el", key: id, p: {
			tag: "button", class: "semctl-icon-action", role, actions, title, aria: title,
			attrs: { "aria-disabled": disabled, "aria-busy": spinning },
		}, c: [spinning ? { k: "spinner", p: { style: "orbit", aria: "Refreshing" } } : icon && cx.supports("icon") ? { k: "icon", p: { name: icon } } : text(label)],
	});
	const props = { role, actions, title, aria: title, tone: "muted" as const, shrink: 0 };
	return spinning ? { k: "spinner", key: id, p: { ...props, style: "orbit" } }
		: icon && cx.supports("icon") ? { k: "icon", key: id, p: { ...props, name: icon } }
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
/** Tinted areas in the drawing: doc concept families, folder regions, or shared identifier terms. */
type Tint = NativeGraphTerritory["kind"];
// Each dot is one selectable group; its size counts member files.
const LEGEND = {
	size: ["group", "Dot = one group · size = member nodes, mostly files"],
	weight: ["links", "Line weight = links between groups"],
} as const;
const TINT_LEGEND: { readonly [K in Tint]: readonly [label: string, title: string, spoken: string] } = {
	family: ["concepts", "Tint = doc concept family", "doc concept families"],
	module: ["folders", "Tint = groups that share a home folder", "groups that share a home folder"],
	lexical: ["shared terms", "Tint = shared identifier terms", "shared identifier terms"],
};
/** The tint item shows only when the drawing has tinted areas. */
function legend(tints: readonly Tint[]): NativeNode {
	const tint = tints.length === 1 ? TINT_LEGEND[tints[0]][0] : tints.length ? "regions" : undefined;
	const tintTitle = tints.map(kind => TINT_LEGEND[kind][1]).join("\n");
	const marks: (readonly [key: string, label: string, title: string])[] = [["size", ...LEGEND.size], ["weight", ...LEGEND.weight]];
	if (tint) marks.push(["tint", tint, tintTitle]);
	const props = { role: "semctl.live.legend", title: compactTooltip("Dot = one group, sized by member nodes · line weight = links between groups", tintTitle) };
	return nativeElement({ k: "el", key: "legend", p: { ...props, tag: "div", class: "semctl-live-legend",
		aria: `Legend: each dot is a group sized by its files, line weight shows links${tints.length ? `, tint shows ${tints.map(kind => TINT_LEGEND[kind][2]).join(" or ")}` : ""}` },
	c: marks.map(([key, label, title]) => nativeElement({ k: "el", key, p: { tag: "span", class: "semctl-legend-item", title }, c: [
		nativeElement({ k: "el", key: "glyph", p: { tag: "span", class: `semctl-legend-${key}`, attrs: { "aria-hidden": true } } }),
		nativeElement({ k: "el", key: "label", p: { tag: "span", class: "semctl-legend-label", text: label } }),
	] })) });
}
const termText = (group: GraphGroup) => group.terms.join(" · ");
/** Identity, key terms, catalog state and the naming doc. The row, chip or drawing shows the rest. */
function groupTooltip(group: GraphGroup): string {
	const files = group.members.filter(node => node.category === "file").length;
	return compactTooltip(`${group.label} · ${files ? plural(files, "file") : plural(group.members.length, "node")}`,
		[group.terms.length ? `Key terms: ${termText(group)}` : undefined, group.source && `From ${group.source}`, group.region && `Region: ${group.region.label}`].filter(Boolean).join(" · "),
		indexingSummary(group.indexing));
}
/** Region label and size; the kind explains why the groups share the region. */
function regionTooltip(region: GraphRegion): string {
	return compactTooltip(`${region.label} · ${plural(region.groups.length, "group")}`,
		region.kind === "doc" ? "Documentation family: the naming docs share a folder"
			: region.kind === "path" ? `Most member files are under ${region.label}/`
				: "Recorded links and shared identifier terms",
		"A map region, not a dependency boundary");
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

const lastNeighborPage = (count: number, chips: number) => Math.max(0, Math.ceil(count / chips) - 1);

/** Passive native UI. The renderer owns motion; this component never animates or takes keyboard focus. */
export class LiveGraphPopup implements Component {
	private display?: Overview;
	private counts?: GraphIndexingCounts;
	private summary?: ProjectSummary;
	private links = new Map<string, GroupLinks>();
	private revision = -1;
	private detail: GraphAppearance["detail"] | undefined;
	private groupsOpen: boolean | undefined;
	private diagnosticsOpen = false;
	private selected: string | undefined;
	private selectionRevision = 0;
	private neighborPage = 0;
	private activityDirty = true;
	private readonly active = new Set<string>();
	private readonly feedback = new Map<string, GraphFeedback>();
	private observationKey = "";
	private diagramKey: string | undefined;
	private diagram: NativeNode | undefined;
	private tints: Tint[] = [];
	private readonly unsubscribe: () => void;
	/** Set by a refresh click. Background reads leave the refresh button enabled and static. */
	private userRefresh = false;
	private readonly diagramAction = (id: string): string | undefined =>
		this.summary?.groups.some(group => group.id === id) ? this.groupAction(id) : undefined;
	private groupAction(id: string): string { return `group:${JSON.stringify([this.display?.identity.codebaseId, id])}`; }
	private selectGroup(id: string | undefined): void {
		this.selected = id;
		if (id !== undefined) this.selectionRevision++;
		this.neighborPage = 0;
	}
	private synchronizeSummary(): void {
		const display = this.state.request.display?.kind === "graph" ? this.state.request.display : undefined;
		const revision = conceptRevision(), detail = (this.state.appearance ?? DEFAULT_GRAPH_APPEARANCE).detail;
		if (display === this.display && this.revision === revision && this.detail === detail) return;
		if (display?.identity.codebaseId !== this.display?.identity.codebaseId) this.selectGroup(undefined);
		if (display !== this.display) this.counts = display && indexingCounts(display.nodes);
		this.display = display; this.revision = revision; this.detail = detail;
		this.summary = display ? projectSummary(display, display.identity.codebaseId, detail) : undefined;
		this.links = display && this.summary ? groupLinks(display, this.summary) : new Map();
		if (this.selected !== undefined && !this.summary?.groups.some(group => group.id === this.selected)) this.selectGroup(undefined);
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
			else if (event.act === "refresh") { this.userRefresh = true; this.actions.refresh(); }
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
				if (group) { this.selectGroup(this.selected === group.id ? undefined : group.id); this.requestRender(); }
			} else if (event.act === "clear-group") {
				this.selectGroup(undefined);
				this.requestRender();
			} else if (event.act === "previous-neighbors" || event.act === "next-neighbors") {
				this.synchronizeSummary();
				const links = this.selected === undefined ? undefined : this.links.get(this.selected);
				if (!links) return;
				const chips = popupSizes[(this.state.appearance ?? DEFAULT_GRAPH_APPEARANCE).popup].chips;
				const last = lastNeighborPage(links.neighbors.length, chips);
				const current = Math.min(last, this.neighborPage);
				const page = Math.max(0, Math.min(last, current + (event.act === "next-neighbors" ? 1 : -1)));
				if (page !== this.neighborPage) { this.neighborPage = page; this.requestRender(); }
			}
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
			grow: 1, basis: "8ch" as const, min: { w: 0 }, gap: "xs" as const, align: "center" as const };
		const label = kind === "busiest" ? group.label.split("/").filter(Boolean).at(-1) ?? group.label : group.label;
		const children = [...(cue ? [cue] : []), text(label, { grow: 1, min: { w: 0 }, wrap: "none", truncate: "end" }),
			...(count !== undefined ? [text(String(count), { tone: "muted", shrink: 0 })] : [])];
		return supportsElements(cx) && cx.feature("styles")
			? nativeElement({ k: "el", key, p: { ...props, tag: "button", class: "semctl-live-neighbor" }, c: children })
			: { k: "row", key, p: props, c: children };
	}
	/** `chips` bounds the neighbor chips; wider popups show more. `linked` bounds the most-linked chips; 0 hides them. `mapShown` says whether the drawing is visible. */
	private selectionInspector(cx: DescribeContext, display: Overview, tints: readonly Tint[], chips: number, linked: number, mapShown: boolean): NativeNode {
		const selected = this.summary?.groups.find(group => group.id === this.selected);
		const links = selected && this.links.get(selected.id), cue = selected && this.groupCue(cx, selected);
		const evidence = links && compactTooltip("Source links counted once, including parallel links and routes not drawn");
		const evidenceAria = links && spoken(`${links.incoming} incoming links · ${links.outgoing} outgoing links · ${links.internal} internal links`,
			sourceScope(display), ...display.caveats.slice(0, 2));
		const count = links?.neighbors.length ?? 0, last = lastNeighborPage(count, chips);
		this.neighborPage = Math.min(this.neighborPage, last);
		const offset = this.neighborPage * chips;
		const neighbors = links?.neighbors.slice(offset, offset + chips).map(neighbor => this.groupChip(cx, "neighbor", neighbor.group,
			compactTooltip(neighbor.group.label, `${neighbor.incoming} into selected · ${neighbor.outgoing} from selected`),
			`Inspect ${clean(neighbor.group.label).slice(0, 160)} · ${neighbor.incoming} incoming links · ${neighbor.outgoing} outgoing links relative to selected group`,
			neighbor.incoming + neighbor.outgoing)) ?? [];
		const range = `Neighbors ${offset + 1}–${offset + neighbors.length} of ${count}`;
		// Without a selection, the groups with the most cross-group links fill the fixed third row.
		const busiest = selected || !linked ? [] : (this.summary?.groups ?? []).filter(group => group.id !== REMAINDER_GROUP).flatMap(group => {
			const { incoming, outgoing } = this.links.get(group.id)!;
			return incoming + outgoing ? [{ group, incoming, outgoing }] : [];
		}).sort((a, b) => b.incoming + b.outgoing - a.incoming - a.outgoing || (a.group.id < b.group.id ? -1 : a.group.id > b.group.id ? 1 : 0)).slice(0, linked)
			.map(({ group, incoming, outgoing }) => this.groupChip(cx, "busiest", group,
				compactTooltip(group.label, `${incoming + outgoing} cross-group links · ${incoming} incoming · ${outgoing} outgoing`),
				`Inspect ${clean(group.label).slice(0, 160)} · ${incoming + outgoing} cross-group links · ${incoming} incoming links · ${outgoing} outgoing links`));
		return { k: "col", key: "selection", reveal: selected ? { at: "nearest", n: this.selectionRevision } : undefined,
			p: { role: "semctl.live.selection", gap: "xs", min: { h: "3.75lines" } }, c: [
			{ k: "row", key: "identity", p: { gap: "xs", align: "center", min: { h: "1.25lines" } }, c: [
				...(cue ? [cue] : []),
				text(selected?.label ?? "Select a group to inspect links", {
					grow: 1, min: { w: 0 }, tone: selected ? "accent" : "muted", wrap: "none", truncate: "end",
					title: selected ? groupTooltip(selected) : compactTooltip(mapShown
						? "Select a dot, chip or group row; select it again to clear"
						: "Select a group row or chip; select it again to clear"),
				}),
				...(selected ? [text(`${selected.members.length} nodes`, { tone: "muted", shrink: 0, aria: `${selected.members.length} member nodes`, title: groupTooltip(selected) }),
					actionButton(cx, "clear-group", "x", "Clear", "Clear highlighted graph group")] : []),
			] },
			{ k: "row", key: "links", p: { gap: "xs", align: "center", min: { h: "1lines" } }, c: [
				links ? text(`${links.incoming} in · ${links.outgoing} out · ${links.internal} internal`, {
					role: "semctl.live.links", shrink: 0, min: { w: 0 }, tone: "muted", wrap: "none", truncate: "end", title: evidence, aria: evidenceAria,
				}) : mapShown ? legend(tints) : text("Use the group list to explore connections", { tone: "muted", wrap: "none", truncate: "end" }),
				// Key terms fill the rest of the row and give way first.
				...(selected?.terms.length ? [text(termText(selected), { role: "semctl.live.terms", grow: 1, basis: 0, min: { w: 0 }, tone: "muted", wrap: "none",
					truncate: "end", title: compactTooltip(`Key terms: ${termText(selected)}`, "Distinctive identifier terms of the member files, not a summary") })] : []),
				// The footer already reports loading layers; only a partial map needs a mark beside the counts.
				...(selected && display.truncated && !display.loading ? [text("partial", {
					tone: "warning", shrink: 0, title: compactTooltip(sourceScope(display), display.caveats[0]),
					aria: spoken(sourceScope(display), ...display.caveats.slice(0, 2)),
				})] : []),
			] },
			{ k: "row", key: "neighbors", p: { gap: "xs", align: "center", wrap: true, min: { h: "1.25lines" } }, c: selected ? [
				// The label names the chip counts: links in both directions between that group and the selected one.
				...(neighbors.length ? [{ ...text("Linked:", { tone: "muted", shrink: 0, title: compactTooltip("Neighboring groups, with links in both directions") }), key: "neighbor-label" }] : []),
				...neighbors,
				...(!neighbors.length ? [text(display.loading ? "No neighboring groups observed yet"
					: display.truncated ? "No neighboring groups in this partial map" : "No neighboring groups in this map", {
						tone: "muted", grow: 1, min: { w: 0 }, wrap: "none", truncate: "end", title: evidence,
					})] : []),
				...(last > 0 ? [
					actionButton(cx, "previous-neighbors", undefined, "‹", compactTooltip("Previous neighbors", range), { disabled: this.neighborPage === 0 }),
					actionButton(cx, "next-neighbors", undefined, "›", compactTooltip("Next neighbors", range), { disabled: this.neighborPage === last }),
				] : []),
			] : busiest.length ? [{ ...text("Most links:", { tone: "muted", shrink: 0,
				title: compactTooltip("Groups with the most cross-group links, counted once") }), key: "busiest-label" }, ...busiest]
				: linked ? [text(display.loading ? "No cross-group links observed yet"
					: display.truncated ? "No cross-group links in this partial map" : "No cross-group links in this map", {
						tone: "muted", grow: 1, min: { w: 0 }, wrap: "none", truncate: "end", title: compactTooltip(display.caveats[0]) || undefined,
					})] : [] },
		] };
	}
	private groupRow(cx: DescribeContext, group: GraphGroup, motion: boolean, terms: boolean): NativeNode {
		const observation = this.active.has(group.id) ? "active" : this.feedback.get(group.id)?.outcome;
		const indexing = indexingSummary(group.indexing), cue = this.groupCue(cx, group);
		const meter = indexingUnsettled(group.indexing) ? indexingMeter(cx, "indexing", group.indexing, { role: "semctl.live.group.indexing", min: { w: "6ch" }, max: { w: "6ch" }, shrink: 0 }) : undefined;
		const status = (["failed", "processing", "pending", "unknown"] as const).find(state => group.indexing[state] > 0);
		const props = { role: "semctl.live.group", gap: "xs" as const, align: "center" as const, actions: { click: this.groupAction(group.id) },
			aria: `${this.selected === group.id ? "Selected: " : "Inspect "}${clean(group.label).slice(0, 160)}, ${group.members.length} nodes${indexing ? `, ${indexing}` : ""}${observation ? `, ${observation}` : ""}`,
			title: groupTooltip(group) };
		const children = [...(cue ? [cue] : []), text(group.label, {
			grow: 1, min: { w: "12ch" }, tone: this.selected === group.id ? "accent" : "muted", truncate: "end", wrap: "none",
		}),
		// Terms take the remaining width after the label and truncate before it.
		...(terms && group.terms.length ? [text(termText(group), { role: "semctl.live.group.terms", shrink: 2, min: { w: 0 }, max: { w: "24ch" }, tone: "muted", truncate: "end", wrap: "none" })] : []),
		...(meter ? [meter] : status ? [groupBadge(cx, "status", `${group.indexing[status]} ${status}`, status === "failed" ? "error" : "muted",
			compactTooltip(indexing, CATALOG_STATES))] : []),
		...(observation ? [groupBadge(cx, "observation", observation, observation === "failed" ? "error" : "success",
			compactTooltip(observation === "active" ? "An active request names a member of this group." : `A recent request naming a group member ${observation}.`))] : []),
		text(String(group.members.length), { role: "semctl.live.group.count", tone: "muted", shrink: 0, aria: `${group.members.length} nodes` })];
		return supportsElements(cx) && cx.feature("styles")
			? nativeElement({ k: "el", key: `group-${group.id}`, p: { ...props, tag: "button", class: "semctl-live-group",
				attrs: { "aria-pressed": this.selected === group.id, "data-motion": motion ? "full" : "reduced" } }, c: children })
			: { k: "row", key: `group-${group.id}`, p: props, c: children };
	}
	/** A non-interactive heading. Its swatch uses the tint of the region's drawn territory. */
	private regionHeader(cx: DescribeContext, key: string, label: string, count: number, title: string, tint?: string): NativeNode {
		const props = { role: "semctl.live.region", title, aria: `${clean(label).slice(0, 160)}, ${plural(count, "group")}` };
		if (!supportsElements(cx) || !cx.feature("styles")) return { k: "row", key, p: { ...props, gap: "xs", align: "center" }, c: [
			text(label, { tone: "muted", grow: 1, min: { w: 0 }, wrap: "none", truncate: "end" }), text(String(count), { tone: "muted", shrink: 0 })] };
		return nativeElement({ k: "el", key, p: { ...props, tag: "div", class: "semctl-live-region" }, c: [
			...(tint ? [nativeElement({ k: "el", key: "swatch", p: { tag: "span", class: `semctl-region-swatch sg-tint-${tint}`, attrs: { "aria-hidden": true } } })] : []),
			nativeElement({ k: "el", key: "label", p: { tag: "span", class: "semctl-live-region-label", text: clean(label) } }),
			nativeElement({ k: "el", key: "count", p: { tag: "span", class: "semctl-live-region-count", text: String(count) } }),
		] });
	}
	/** Regions first, in map order, each with its groups by size. Groups outside every region follow; the folded remainder stays last. */
	private groupList(cx: DescribeContext, motion: boolean, terms: boolean): NativeNode[] {
		const summary = this.summary!, byId = new Map(summary.groups.map(group => [group.id, group])), rows: NativeNode[] = [], listed = new Set<string>();
		for (const region of summary.regions) {
			const tint = conceptPalette(region.key)[cx.dark ? "dark" : "light"].slice(1);
			rows.push(this.regionHeader(cx, `region-${region.key}`, region.label, region.groups.length, regionTooltip(region), tint));
			for (const group of region.groups.map(id => byId.get(id)!).sort((a, b) => b.members.length - a.members.length || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0))) {
				rows.push(this.groupRow(cx, group, motion, terms)); listed.add(group.id);
			}
		}
		const rest = summary.groups.filter(group => !listed.has(group.id));
		if (listed.size && rest.length) rows.push(this.regionHeader(cx, "region-outside", "Outside regions", rest.length,
			compactTooltip(`${plural(rest.length, "group")} outside every region`, "No shared doc family, home folder, or linked terms with another drawn group")));
		rows.push(...rest.map(group => this.groupRow(cx, group, motion, terms)));
		return rows;
	}
	describe(cx: DescribeContext): NativeNode {
		const { request, activity } = this.state;
		const appearance = this.state.appearance ?? DEFAULT_GRAPH_APPEARANCE, motion = appearance.motion !== "off" && !cx.reduceMotion;
		const size = popupSizes[appearance.popup];
		if (!request.loading) this.userRefresh = false;
		const refreshing = this.userRefresh;
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
		const counts = this.counts;
		const meter = counts && indexingUnsettled(counts) ? indexingMeter(cx, "indexing", counts, { role: "semctl.live.indexing", grow: 1, min: { w: 0 } }, true) : undefined;
		const indexing = counts && !meter && indexingUnsettled(counts) ? indexingSummary(counts) : undefined;
		const subline: NativeNode[] = display && this.summary ? [{ k: "row", key: "subline", p: { gap: "xs", align: "center" }, c: [
			text(`${plural(this.summary.files, "file")} · ${plural(this.summary.groups.length, "group")}`, {
				role: "semctl.live.subline", tone: "muted", min: { w: 0 }, wrap: "none", truncate: "end",
				title: compactTooltip(`${display.nodes.length} indexed nodes · ${this.summary.groups.length} groups`,
					`${display.edges.length} links · ${this.summary.drawnLinks} cross-group links drawn`),
			}),
			...(partial ? [cx.supports("icon") ? { k: "icon" as const, key: "partial", p: { ...partial, role: "semctl.live.partial", name: "warn" } }
				: { ...text("Partial map", partial), key: "partial" }] : []),
			...(meter ? [meter] : indexing ? [text(indexing.replace(/^Indexing: /, ""), { role: "semctl.live.indexing", tone: "muted",
				min: { w: 0 }, wrap: "none", truncate: "end", title: compactTooltip(indexing, CATALOG_STATES) })] : []),
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
					actionButton(cx, "refresh", "repeat", "Refresh", refreshing ? "Refreshing indexed project map" : "Refresh indexed project map",
						{ disabled: refreshing, busy: refreshing && motion }),
					actionButton(cx, "close", "x", "Close", "Close live overview"),
				] },
			] },
			...subline,
		] };
		const children: NativeNode[] = [];
		const warnings = request.warnings ?? [];
		const sections = cx.supports("section");
		if (warnings.length && sections) children.push({ ...text(`Warning: ${warnings[0]}`, {
			role: "semctl.live.query-warning", tone: "warning", wrap: "none", truncate: "end",
			title: compactTooltip(...warnings.slice(0, 2), "Open Diagnostics for all warnings"),
		}), key: "query-warning" });
		if (display && this.summary) {
			const diagramKey = JSON.stringify([this.observationKey, this.selected, cx.dark, cx.reduceMotion,
				supportsElements(cx), cx.feature("styles"), appearance, graphColorRevision()]);
			if (this.diagramKey !== diagramKey) {
				this.diagramKey = diagramKey;
				const preview = graphPreview(this.summary.graph, cx, {
					codebaseId: display.identity.codebaseId, activeIds: this.active, feedback: this.feedback,
					size: size.preview, selected: this.selected, nodeAction: this.diagramAction, appearance,
					labelWidth: appearance.popup === "small" ? 200 : 280,
				});
				// Previews exist only on styled element hosts, so the well needs no fallback.
				this.diagram = preview && nativeElement({ k: "el", key: "graph-well", p: { tag: "div", class: "semctl-graph-well" }, c: [preview] });
				// The legend names the tint kinds that the drawing shows. The layout is cached, so this reads the drawn geometry.
				const territories = preview ? nativeGraphLayout(this.summary.graph, size.preview, appearance.layout).territories : [];
				this.tints = (["family", "module", "lexical"] as const).filter(kind => territories.some(territory => territory.kind === kind));
			}
			// Small and medium popups stack the list below the drawing, so an open list replaces the drawing: Map and Groups are two views.
			const groupsOpen = this.groupsOpen ?? !this.diagram, listView = groupsOpen && sections && !!this.diagram && appearance.popup !== "large";
			// An open list already names every group, and narrow chips truncate to fragments, so fewer chips show.
			const linked = groupsOpen ? 0 : Math.min(size.chips, appearance.popup === "small" ? 2 : 3);
			const details = [this.selectionInspector(cx, display, this.tints, size.chips, linked, !!this.diagram && !listView)];
			// Small popups keep the row for names, counts and states; wider popups also show key terms.
			const groups = groupsOpen || !sections ? this.groupList(cx, motion, size.chips > 3) : [];
			const regions = this.summary.regions.length;
			if (sections) details.push({ k: "section", key: "groups", p: {
				head: listView ? `Back to map · ${plural(this.summary.groups.length, "group")}` : `Explore ${plural(this.summary.groups.length, "group")}${regions ? ` in ${plural(regions, "region")}` : ""}`,
				collapsible: true, collapsed: !groupsOpen,
			}, c: groups });
			else details.push(...groups);
			if (this.diagram && !listView) children.push({ k: "col", key: "workspace", p: { role: "semctl.live.workspace", gap: "sm" }, c: [
				this.diagram, { k: "col", key: "details", p: { role: "semctl.live.details", gap: "xs" }, c: details },
			] });
			else children.push(...details);
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
		if (request.error || warnings.length) {
			const diagnostics = [
				...warnings.map(warning => text(`Warning: ${warning}`, { role: "semctl.live.query-warning", wrap: "word", tone: "warning" })),
				...(request.error ? [text(request.error, { role: "semctl.live.diagnostic-text", wrap: "word", tone: "muted" })] : []),
			];
			if (sections) children.push({ k: "section", key: "diagnostics", p: {
				role: "semctl.live.diagnostics", head: "Diagnostics", collapsible: true, collapsed: !this.diagnosticsOpen,
			}, c: this.diagnosticsOpen ? diagnostics : [] });
			else children.push(...diagnostics);
		}
		return { k: "col", p: { gap: "sm" }, c: [
			header, { k: "col", key: "body", p: { role: "semctl.live.body", gap: "sm" }, c: children },
			graphStatus(cx, { request, activity, motion: appearance.motion, presentation: "icon", counts }),
		] };
	}
	render(width: number): readonly string[] {
		this.synchronizeSummary();
		const selected = this.summary?.groups.find(group => group.id === this.selected), links = selected && this.links.get(selected.id);
		return ["semctl · Live project overview",
			...(this.summary ? [`${this.summary.files} files · ${this.summary.groups.length} groups`,
				selected ? `${selected.label} · ${selected.members.length} nodes${selected.terms.length ? ` · ${termText(selected)}` : ""}` : "Open /semctl graph for keyboard navigation"] : []),
			...(links && this.display ? [`${links.incoming} in · ${links.outgoing} out · ${links.internal} internal`, sourceScope(this.display),
				...links.neighbors.slice(0, 3).map(neighbor => `${neighbor.group.label} · ${neighbor.incoming} in · ${neighbor.outgoing} out`)] : []),
			...(this.state.request.warnings?.flatMap(warning => `Warning: ${warning}`.split(/\r?\n/)) ?? []),
			this.state.activity.producer?.text ?? "No producer observation", ...this.state.activity.operations.map(operationLabel)]
			.map(line => clean(line).slice(0, Math.max(0, width)));
	}
}
