import type { Component, DescribeContext, NativeNode, NativeUiEvent } from "@oh-my-pi/pi-tui";
import { conceptRevision, subscribeConcepts } from "./concept-colors";
import { projectSummary, type GraphGroup, type ProjectSummary } from "./graph-projection";
import { graphPreview, type GraphFeedback } from "./graph-preview";
import { exactActivity } from "./graph-data/layout.js";
import type { GraphActivity } from "./graph-activity";
import type { Display } from "./inspector";
import type { GraphRequestState } from "./overview";
import { nativeElement, supportsElements } from "./native-elements";
import { indexingCounts, indexingSummary } from "./graph-indexing";
import { DEFAULT_GRAPH_APPEARANCE, type GraphAppearance } from "./graph-appearance";
import { graphColorRevision, lookupGraphColor } from "./graph-colors";
import { nativeGraphColorCue } from "./native-concepts";
import { graphStatus } from "./graph-status";

const clean = (value: string) => value.replace(/[\u0000-\u0008\u000b-\u001f\u007f-\u009f]/g, "");
const text = (value: string, props: Record<string, unknown> = {}): NativeNode => ({ k: "text", p: { text: clean(value), ...props } });
type Overview = Extract<Display, { kind: "graph" }>;

export interface LiveGraphState {
	request: GraphRequestState;
	activity: GraphActivity;
	appearance?: Readonly<GraphAppearance>;
	appearanceError?: string;
}
interface Actions { close(): void; refresh(): void; expand(): void; appearance(): void }

function actionButton(cx: DescribeContext, id: string, icon: "expand" | "repeat" | "x" | "settings", label: string, title: string, disabled = false): NativeNode {
	const actions = disabled ? undefined : { click: id };
	if (supportsElements(cx) && cx.feature("styles")) return nativeElement({
		k: "el", key: id, p: {
			tag: "button", class: "semctl-icon-action", actions, title, aria: title,
			attrs: { "aria-disabled": disabled },
		}, c: [cx.supports("icon") ? { k: "icon", p: { name: icon } } : text(label)],
	});
	const props = { actions, title, aria: title, tone: "muted" as const, shrink: 0 };
	return cx.supports("icon")
		? { k: "icon", key: id, p: { ...props, name: icon } }
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

const tooltip = (...lines: (string | undefined)[]): string => lines.filter((line): line is string => !!line)
	.map(line => clean(line).slice(0, 160)).join("\n").slice(0, 720);
function groupTooltip(group: GraphGroup): string {
	return tooltip(group.label, `${group.members.length} nodes · ${group.internal} internal links`, indexingSummary(group.indexing),
		...group.members.slice(0, 3).map(node => node.path ?? node.label),
		group.members.length > 3 ? `${group.members.length - 3} more · open Overview to explore` : undefined);
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
	private selected: string | undefined;
	private activityDirty = true;
	private readonly active = new Set<string>();
	private readonly feedback = new Map<string, GraphFeedback>();
	private observationKey = "";
	private diagramKey: string | undefined;
	private diagram: NativeNode | undefined;
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
		else if (event.type === "action") {
			if (event.act === "close") this.actions.close();
			else if (event.act === "refresh") this.actions.refresh();
			else if (event.act === "expand") this.actions.expand();
			else if (event.act === "appearance") this.actions.appearance();
			else if (event.act.startsWith("group:")) {
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
		return cue ? nativeElement({ k: "el", key: cue.key, p: { ...cue.p,
			title: tooltip(...(cue.p.title ?? "").split("\n")), aria: tooltip(...(cue.p.aria ?? "").split("\n")),
		} }) : undefined;
	}
	private selectionInspector(cx: DescribeContext, display: Overview): NativeNode {
		const selected = this.summary?.groups.find(group => group.id === this.selected);
		const links = selected && this.links.get(selected.id), cue = selected && this.groupCue(cx, selected);
		const evidence = links ? tooltip(`${links.incoming} incoming links · ${links.outgoing} outgoing links`, `${links.internal} internal links, counted once.`,
			sourceScope(display), "Includes parallel links and routes not drawn.", ...display.caveats.slice(0, 2)) : undefined;
		const neighbors = links?.neighbors.slice(0, 3).map(neighbor => {
			const cue = this.groupCue(cx, neighbor.group), count = neighbor.incoming + neighbor.outgoing;
			const title = tooltip(neighbor.group.label, `${neighbor.incoming} incoming to selected · ${neighbor.outgoing} outgoing from selected`,
				sourceScope(display));
			const props = { role: "semctl.live.neighbor", actions: { click: this.groupAction(neighbor.group.id) }, title,
				aria: `Inspect ${clean(neighbor.group.label).slice(0, 160)} · ${neighbor.incoming} incoming links · ${neighbor.outgoing} outgoing links relative to selected group`,
				grow: 1, basis: 0, min: { w: 0 }, gap: "xs" as const, align: "center" as const };
			const children = [...(cue ? [cue] : []), text(neighbor.group.label, { grow: 1, min: { w: 0 }, wrap: "none", truncate: "end" }),
				text(String(count), { tone: "muted", shrink: 0 })];
			return supportsElements(cx) && cx.feature("styles")
				? nativeElement({ k: "el", key: `neighbor-${neighbor.group.id}`, p: { ...props, tag: "button", class: "semctl-live-neighbor" }, c: children })
				: { k: "row" as const, key: `neighbor-${neighbor.group.id}`, p: props, c: children };
		}) ?? [];
		return { k: "col", key: "selection", p: { role: "semctl.live.selection", gap: "xs", min: { h: "3.75lines" } }, c: [
			{ k: "row", key: "identity", p: { gap: "xs", align: "center", min: { h: "1.25lines" } }, c: [
				...(cue ? [cue] : []),
				text(selected?.label ?? "Select a dot or group to inspect links", {
					grow: 1, min: { w: 0 }, tone: selected ? "accent" : "muted", wrap: "none", truncate: "end",
					title: selected ? groupTooltip(selected) : tooltip("Select again or clear to remove the highlight.", sourceScope(display), ...display.caveats.slice(0, 2)),
				}),
				...(selected ? [text(`${selected.members.length} nodes`, { tone: "muted", shrink: 0, aria: `${selected.members.length} member nodes`, title: groupTooltip(selected) }),
					actionButton(cx, "clear-group", "x", "Clear", "Clear highlighted graph group")] : []),
			] },
			{ k: "row", key: "links", p: { gap: "xs", align: "center", min: { h: "1lines" } }, c: [
				text(links ? `${links.incoming} in · ${links.outgoing} out · ${links.internal} internal` : "", {
					role: "semctl.live.links", grow: 1, min: { w: 0 }, tone: "muted", wrap: "none", truncate: "end", title: evidence, aria: evidence,
				}),
				...(selected && (display.loading || display.truncated) ? [text(display.loading ? "loading" : "partial", {
					tone: display.loading ? "muted" : "warning", shrink: 0, title: tooltip(sourceScope(display), ...display.caveats.slice(0, 2)),
				})] : []),
			] },
			{ k: "row", key: "neighbors", p: { gap: "xs", align: "center", min: { h: "1.25lines" } }, c: [
				...neighbors,
				...(selected && !neighbors.length ? [text(display.loading ? "No neighboring groups observed yet"
					: display.truncated ? "No neighboring groups in this partial map" : "No neighboring groups in this map", {
						tone: "muted", grow: 1, min: { w: 0 }, wrap: "none", truncate: "end", title: evidence,
					})] : []),
				...(links && links.neighbors.length > neighbors.length ? [text(`+${links.neighbors.length - neighbors.length}`, {
					tone: "muted", shrink: 0, title: `${links.neighbors.length} neighboring groups in the supplied map; open Overview for all relationships.`,
				})] : []),
			] },
		] };
	}
	private groupRow(cx: DescribeContext, group: GraphGroup): NativeNode {
		const observation = this.active.has(group.id) ? "active" : this.feedback.get(group.id)?.outcome;
		const indexing = indexingSummary(group.indexing), cue = this.groupCue(cx, group);
		const status = (["failed", "processing", "pending", "unknown", "ready"] as const).find(state => group.indexing[state] > 0);
		const props = { role: "semctl.live.group", gap: "xs" as const, align: "center" as const, actions: { click: this.groupAction(group.id) },
			aria: `${this.selected === group.id ? "Selected: " : "Inspect "}${clean(group.label).slice(0, 160)}, ${group.members.length} nodes${indexing ? `, ${indexing}` : ""}${observation ? `, ${observation}` : ""}`,
			title: groupTooltip(group) };
		const children = [...(cue ? [cue] : []), text(group.label, {
			grow: 1, min: { w: 0 }, tone: this.selected === group.id ? "accent" : "muted", truncate: "end", wrap: "none",
		}), ...(status ? [groupBadge(cx, "status", `${group.indexing[status]} ${status}`, status === "failed" ? "error" : "muted",
			tooltip(indexing, "Catalog states · pending is not active work"))] : []),
		...(observation ? [groupBadge(cx, "observation", observation, observation === "failed" ? "error" : "success",
			observation === "active" ? "An active request names a member of this group." : `A recent request naming a group member ${observation}.`)] : []),
		text(String(group.members.length), { role: "semctl.live.group.count", tone: "muted", shrink: 0, aria: `${group.members.length} nodes` })];
		return supportsElements(cx) && cx.feature("styles")
			? nativeElement({ k: "el", key: `group-${group.id}`, p: { ...props, tag: "button", class: "semctl-live-group", attrs: { "aria-pressed": this.selected === group.id } }, c: children })
			: { k: "row", key: `group-${group.id}`, p: props, c: children };
	}
	describe(cx: DescribeContext): NativeNode {
		const { request, activity } = this.state;
		const appearance = this.state.appearance ?? DEFAULT_GRAPH_APPEARANCE;
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
		const toolbar: NativeNode = { k: "row", key: "toolbar", p: { role: "semctl.live.toolbar", gap: "sm", align: "center" }, c: [
			text(this.summary ? `${this.summary.files} ${this.summary.files === 1 ? "file" : "files"}` : "Project map", {
				grow: 1, min: { w: 0 }, wrap: "none", truncate: "end", tone: "muted",
				title: display && this.summary ? `${display.nodes.length} indexed nodes · ${this.summary.groups.length} groups\n${display.edges.length} links · ${this.summary.drawnLinks} cross-group links drawn` : "Indexed project map",
			}),
			...(display?.truncated && !display.loading && cx.supports("icon") ? [{ k: "icon" as const, key: "partial", p: {
				name: "warn", tone: "warning" as const, shrink: 0, title: `Partial indexed map\n${this.summary?.graph.caveats.join("\n") ?? ""}`,
				aria: `Partial indexed map. ${this.summary?.graph.caveats.join(" ") ?? ""}`,
			} }] : []),
			{ k: "row", p: { gap: "xs", shrink: 0, align: "center" }, c: [
				actionButton(cx, "expand", "expand", "Overview", "Open full project overview"),
				actionButton(cx, "appearance", "settings", "Appearance", this.state.appearanceError ? `Graph appearance · preferences not saved\n${this.state.appearanceError}` : "Graph appearance"),
				actionButton(cx, "refresh", "repeat", "Refresh", request.loading ? "Refreshing indexed project map" : "Refresh indexed project map", request.loading),
				actionButton(cx, "close", "x", "Close", "Close live overview"),
			] },
		] };
		const children: NativeNode[] = [];
		if (display && this.summary) {
			const indexing = indexingSummary(indexingCounts(display.nodes));
			if (indexing) children.push(text(indexing.replace(/^Indexing: /, ""), {
				role: "semctl.live.indexing", tone: "muted", wrap: "none", truncate: "end",
				title: `${indexing}\nCatalog states · pending is not active work`,
			}));
			const diagramKey = JSON.stringify([this.observationKey, this.selected, cx.dark, cx.reduceMotion,
				supportsElements(cx), cx.feature("styles"), appearance, graphColorRevision()]);
			if (this.diagramKey !== diagramKey) {
				this.diagramKey = diagramKey;
				this.diagram = graphPreview(this.summary.graph, cx, {
					codebaseId: display.identity.codebaseId, activeIds: this.active, feedback: this.feedback,
					size: "compact", selected: this.selected, nodeAction: this.diagramAction, appearance,
				});
			}
			if (this.diagram) children.push(this.diagram);
			children.push(this.selectionInspector(cx, display));
			if (display.truncated && !display.loading && !cx.supports("icon")) children.push(text("Partial indexed map", {
				tone: "warning", title: this.summary.graph.caveats.join("\n"),
			}));
			const groups = this.groupsOpen || !cx.supports("section") ? this.summary.groups.map(group => this.groupRow(cx, group)) : [];
			if (cx.supports("section")) children.push({ k: "section", key: "groups", p: {
				head: `Explore ${this.summary.groups.length} groups`, collapsible: true, collapsed: !this.groupsOpen,
			}, c: this.groupsOpen ? groups : [] });
			else children.push(...groups);
		} else {
			const emptyText = request.error ? "The indexed map is unavailable" : request.loading ? "Reading the indexed map" : "No indexed map available";
			const empty: NativeNode[] = [
				cx.supports("icon") ? { k: "icon", p: { name: request.error ? "warn" : "diagram", tone: request.error ? "warning" : "muted" } } : text("○", { tone: "muted" }),
				text(emptyText, { tone: "muted", title: request.error }),
				text(request.error ? "Refresh to try again" : "Available files appear before embedding completes", { tone: "muted" }),
			];
			children.push(supportsElements(cx) && cx.feature("styles")
				? nativeElement({ k: "el", key: "empty", p: { tag: "div", class: "semctl-live-empty" }, c: empty })
				: { k: "col", key: "empty", p: { min: { h: "9lines" }, gap: "sm" }, c: empty });
		}
		return { k: "col", p: { gap: "sm" }, c: [
			toolbar, { k: "col", key: "body", p: { role: "semctl.live.body", gap: "sm" }, c: children },
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
