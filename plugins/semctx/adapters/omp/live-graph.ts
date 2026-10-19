import type { Component, DescribeContext, NativeNode, NativeUiEvent } from "@oh-my-pi/pi-tui";
import { conceptRevision, subscribeConcepts } from "./concept-colors";
import { projectSummary, type ProjectSummary } from "./graph-projection";
import { graphPreview, type GraphFeedback } from "./graph-preview";
import { exactActivity } from "./graph-data/layout.js";
import type { GraphActivity } from "./graph-activity";
import type { Display } from "./inspector";
import type { GraphRequestState } from "./overview";

const clean = (value: string) => value.replace(/[\u0000-\u0008\u000b-\u001f\u007f-\u009f]/g, "");
const text = (value: string, props: Record<string, unknown> = {}): NativeNode => ({ k: "text", p: { text: clean(value), ...props } });
type Overview = Extract<Display, { kind: "graph" }>;

export interface LiveGraphState { request: GraphRequestState; activity: GraphActivity }
interface Actions { close(): void; refresh(): void; expand(): void }

function operationLabel(operation: string): string {
	const label = clean(operation).replace(/^mcp__semctx_/, "").replace(/[_-]+/g, " ").trim();
	return label ? label[0].toUpperCase() + label.slice(1) : "Request";
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

/** Passive native UI. The renderer owns motion; this component has no animation timer or keyboard focus. */
export class LiveGraphPopup implements Component {
	readonly nativeOverlay = { role: "omp.overlay.semctl-live", size: "sm" as const, anchor: "top" as const };
	private display?: Overview;
	private summary?: ProjectSummary;
	private revision = -1;
	private groupsOpen = false;
	private selected: string | undefined;
	private activityDirty = true;
	private readonly active = new Set<string>();
	private readonly feedback = new Map<string, GraphFeedback>();
	private observationKey = "";
	private diagramKey: string | undefined;
	private diagram: NativeNode | undefined;
	private readonly unsubscribe: () => void;
	private readonly diagramAction = (id: string): string | undefined => {
		const index = this.summary?.groups.findIndex(group => group.id === id);
		return index === undefined || index < 0 ? undefined : `group:${index}`;
	};
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
			else if (event.act.startsWith("group:")) {
				const group = this.summary?.groups[Number(event.act.slice(6))];
				if (group) { this.selected = this.selected === group.id ? undefined : group.id; this.requestRender(); }
			} else if (event.act === "clear-group") { this.selected = undefined; this.requestRender(); }
		}
	}
	describe(cx: DescribeContext): NativeNode {
		const { request, activity } = this.state;
		const display = request.display?.kind === "graph" ? request.display : undefined, revision = conceptRevision();
		if (display !== this.display || this.revision !== revision) {
			if (display?.identity.codebaseId !== this.display?.identity.codebaseId) this.selected = undefined;
			this.display = display; this.revision = revision;
			this.summary = display ? projectSummary(display, display.identity.codebaseId) : undefined;
			if (!this.summary?.groups.some(group => group.id === this.selected)) this.selected = undefined;
			this.activityDirty = true;
			this.diagramKey = undefined;
		}
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
		const children: NativeNode[] = [{ k: "row", p: { gap: "sm", align: "center" }, c: [
			text("semctl", { grow: 1, title: "Live project overview. Only reported activity is shown." }),
			text("Overview", { tone: "accent", actions: { click: "expand" }, aria: "Open full project overview" }),
			text("Refresh", { tone: "muted", actions: { click: "refresh" }, aria: "Refresh indexed project map" }),
			text("Close", { tone: "muted", actions: { click: "close" }, aria: "Close live graph popup" }),
		] }];
		if (display && this.summary) {
			children.push(text(`${this.summary.files} ${this.summary.files === 1 ? "file" : "files"} · ${display.edges.length} links`, {
				tone: "muted", title: `${display.nodes.length} indexed nodes in ${this.summary.groups.length} displayed groups. ${this.summary.drawnLinks} cross-group links drawn. Internal links are counted in group tooltips. ${this.summary.graph.caveats.join(" ")}`,
			}));
			const diagramKey = JSON.stringify([this.observationKey, this.selected, cx.dark, cx.reduceMotion,
				cx.stylesheets === true, cx.supports("el"), cx.feature("styles")]);
			if (this.diagramKey !== diagramKey) {
				this.diagramKey = diagramKey;
				this.diagram = graphPreview(this.summary.graph, cx, {
					codebaseId: display.identity.codebaseId, activeIds: this.active, feedback: this.feedback,
					size: "compact", selected: this.selected, nodeAction: this.diagramAction,
				});
			}
			if (this.diagram) children.push(this.diagram);
			const selected = this.summary.groups.find(group => group.id === this.selected);
			children.push({ k: "row", key: "selection", p: { gap: "sm", align: "center" }, c: [
				text(selected ? `${selected.label} · ${selected.members.length} nodes · ${selected.internal} internal links` : "Select a dot or explore groups", {
					grow: 1, basis: 0, min: { w: 0 }, tone: selected ? "accent" : "muted", wrap: "none", truncate: "end",
					title: selected ? `${selected.members.length} indexed nodes grouped by reported concept or source structure; not a processing stage.\n${selected.internal} internal links\n${selected.members.slice(0, 12).map(node => node.path ?? node.label).join("\n")}${selected.members.length > 12 ? `\n${selected.members.length - 12} more indexed nodes` : ""}` : "Select a graph dot or a group below to highlight its reported relationships. Select it again to clear.",
				}),
				...(selected ? [text("Clear", { tone: "muted", shrink: 0, wrap: "none", actions: { click: "clear-group" }, aria: "Clear highlighted graph group" })] : []),
			] });
			if (display.truncated) children.push(text("Partial indexed map", { tone: "warning", title: this.summary.graph.caveats.join("\n") }));
			const groups = this.groupsOpen || !cx.supports("section") ? this.summary.groups.map((group, index): NativeNode => {
				const observation = this.active.has(group.id) ? "active" : this.feedback.get(group.id)?.outcome;
				return text(`${this.selected === group.id ? "Selected · " : ""}${group.label} · ${group.members.length}${observation ? ` · ${observation}` : ""}`, {
					tone: this.selected === group.id || observation === "active" ? "accent" : observation === "failed" ? "error" : "muted",
					truncate: "end", wrap: "none", actions: { click: `group:${index}` },
					aria: `${this.selected === group.id ? "Selected: " : "Highlight "}${clean(group.label)}${observation ? `, ${observation}` : ""}`,
					title: `${group.internal} internal links\n${group.members.slice(0, 12).map(node => node.path ?? node.label).join("\n")}${group.members.length > 12 ? `\n${group.members.length - 12} more indexed nodes` : ""}`,
				});
			}) : [];
			if (cx.supports("section")) children.push({ k: "section", key: "groups", p: {
				head: `Explore ${this.summary.groups.length} groups`, collapsible: true, collapsed: !this.groupsOpen,
			}, c: this.groupsOpen ? groups : [] });
			else children.push(...groups);
		} else children.push(text(request.loading ? "Reading indexed project map…" : "No indexed map available", { tone: "muted" }));
		const producer = activity.producer, counts = activity.requests;
		const operations = [...new Set(activity.operations)], latest = (activity.recent ?? []).reduce<(NonNullable<GraphActivity["recent"]>)[number] | undefined>(
			(newest, receipt) => !newest || receipt.id > newest.id ? receipt : newest, undefined);
		const observation = operations.length ? `${operations.map(operationLabel).join(" · ")} · active`
			: latest ? `${operationLabel(latest.operation)} · ${latest.outcome}` : undefined;
		const mapStatus = request.error ? "Map refresh failed" : request.loading ? display ? "Refreshing project map…" : "Reading project map…" : undefined;
		const detail = [
			producer?.details ?? producer?.text,
			operations.length ? `Observed active requests: ${operations.join(", ")}` : latest ? `Observed ${latest.outcome} request: ${latest.operation}` : undefined,
			counts ? `Session traffic: ${counts.sent} sent · ${counts.received} returned · ${counts.failed} failed` : undefined,
			request.error,
			"Only observed requests and aggregate producer reports are shown. Internal pipeline stages are not inferred.",
		].filter(Boolean).join("\n");
		const primary = request.error ? "Map refresh failed" : producer?.text ?? mapStatus ?? observation ?? "No active requests";
		const tone = request.error ? "error" : producer?.tone ?? (latest?.outcome === "failed" && !operations.length ? "error" : observation ? "accent" : "muted");
		const progress: NativeNode = producer?.progress !== undefined && cx.supports("meter")
			? { k: "meter", key: "progress", p: { value: producer.progress, style: "ring", size: "sm", tone: producer.tone, title: detail, aria: "Producer-reported embedding and sync progress" } }
			: cx.supports("icon") ? { k: "icon", p: { name: request.error || latest?.outcome === "failed" ? "warn" : "info", tone, title: detail, aria: detail } }
				: text("·", { tone, title: detail });
		children.push({ k: "row", key: "activity", p: { gap: "sm", align: "center", min: { h: "1.5lines" } }, c: [
			{ k: "row", key: "progress-slot", p: { basis: "content", min: { w: "2.5ch" }, max: { w: "2.5ch" }, grow: 0, shrink: 0, align: "center" }, c: [progress] },
			text(primary, { grow: 1, tone, wrap: "none", truncate: "end", title: detail }),
			...(producer && observation && !request.error ? [text(operations.length ? `${activity.operations.length} active` : latest!.outcome === "failed" ? "Request failed" : "Returned", {
				tone: latest?.outcome === "failed" && !operations.length ? "error" : operations.length ? "accent" : "muted",
				wrap: "none", shrink: 0, title: detail,
			})] : []),
			...(counts?.failed ? [text(`${counts.failed} failed`, { tone: "error", wrap: "none", shrink: 0, title: detail, aria: `${counts.failed} observed requests failed in this session` })] : []),
		] });
		return { k: "col", p: { gap: "sm" }, c: children };
	}
	render(width: number): readonly string[] {
		return ["semctl · Live project overview", this.state.activity.producer?.text ?? "No producer observation", ...this.state.activity.operations.map(operationLabel)]
			.map(line => clean(line).slice(0, Math.max(0, width)));
	}
}
