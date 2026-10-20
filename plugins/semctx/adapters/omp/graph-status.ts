import type { DescribeContext, NativeNode } from "@oh-my-pi/pi-tui";
import type { GraphActivity } from "./graph-activity";
import type { GraphAppearance } from "./graph-appearance";
import type { GraphRequestState } from "./overview";
import { nativeElement, supportsElements } from "./native-elements";
import { progressTooltip } from "./status-tooltip";
import { boundedTooltip, compactTooltip } from "./tooltip-text";

interface GraphStatusInput {
	request: GraphRequestState;
	activity: GraphActivity;
	motion?: GraphAppearance["motion"];
	presentation?: "label" | "icon";
}
const clean = (value: string) => value.replace(/[\u0000-\u0008\u000b-\u001f\u007f-\u009f]/g, "");
const text = (value: string, props: Record<string, unknown> = {}): NativeNode => ({ k: "text", p: { text: clean(value), ...props } });
const operationLabel = (value: string) => clean(value).replace(/^mcp__semctx_/, "").replace(/[_-]+/g, " ");

/** Current evidence wins over a previous receipt. A queued observation never implies active work. */
export function graphStatusModel({ request, activity }: GraphStatusInput) {
	const { producer, requests } = activity;
	const active = activity.operations.length;
	const latest = (activity.recent ?? []).reduce<NonNullable<GraphActivity["recent"]>[number] | undefined>(
		(previous, receipt) => !previous || receipt.id > previous.id ? receipt : previous, undefined);
	const layersLoading = request.display?.kind === "graph" && request.display.loading;
	const mode = request.error ? "failed" : producer?.symbol === "status.error" || producer?.tone === "error" ? "failed"
		: producer?.symbol === "status.running" ? "working" : producer?.symbol === "status.pending" ? "pending"
		: producer?.symbol === "status.success" ? "ready" : request.loading || layersLoading ? "loading"
		: active ? "working" : producer?.tone === "warning" ? "warning"
		: latest?.outcome === "failed" ? "failed" : latest?.outcome === "returned" ? "returned" : "idle";
	const label = request.error ? request.display ? "Map refresh failed" : "Map unavailable"
		: producer ? producer.symbol === "status.success" ? "Current job embedded" : producer.text
		: request.loading ? request.display ? "Refreshing project map" : "Reading project map"
		: layersLoading ? "Relationship layers loading"
		: active ? `${active} active ${active === 1 ? "request" : "requests"}`
		: latest ? `${operationLabel(latest.operation)} · ${latest.outcome}` : "No active requests";
	const tone = request.error ? "error" as const : producer?.tone ?? (mode === "failed" ? "error" as const
		: mode === "working" || mode === "returned" ? "success" as const : "muted" as const);
	return { mode, label, tone, progress: request.error ? undefined : producer?.progress, active, requests };
}

/** Fixed slots and keyed children avoid remounting progress or replaying motion on counter updates. */
export function graphStatus(cx: DescribeContext, input: GraphStatusInput): NativeNode {
	const model = graphStatusModel(input), { mode, tone, requests } = model;
	const compact = input.presentation === "icon";
	// Map refresh errors do not invalidate separately reported producer progress.
	const progress = compact ? input.activity.producer?.progress : model.progress;
	const warning = mode === "failed" || mode === "warning" || input.activity.producer?.tone === "warning";
	const { producer, operations } = input.activity;
	const loadingNotes = input.request.display?.kind === "graph" && input.request.display.loading ? input.request.display.caveats.join("\n") : undefined;
	const detail = progressTooltip(model.label, progress, input.request.error ?? producer?.diagnostic ?? (warning ? producer?.details : loadingNotes),
		"Overview → Diagnostics for the full error");
	const traffic = requests ? `${requests.sent} sent · ${requests.received} returned · ${requests.failed} failed` : "";
	const activityTitle = compactTooltip(traffic, operations.length ? `Active: ${[...new Set(operations)].join(", ")}` : undefined);
	const styled = supportsElements(cx) && cx.feature("styles");
	const motion = input.motion !== "off" && !cx.reduceMotion;
	const mark: NativeNode = (compact || progress !== undefined) && cx.supports("meter")
		? { k: "meter", key: "progress", p: { value: progress ?? null, style: "ring", size: "sm",
			tone: compact ? progress === undefined ? "muted" : progress === 1 ? "success" : "accent" : tone,
			title: detail, aria: "Producer-reported embedding and sync progress" } }
		: !compact && cx.supports("icon") && (mode === "failed" || mode === "warning")
			? { k: "icon", key: "warning", p: { name: "warn", tone, title: detail, aria: detail } }
			: !compact && motion && (mode === "loading" || mode === "working") && cx.supports("spinner")
				? { k: "spinner", key: "spinner", p: { style: "orbit", tone: "accent", title: detail, aria: detail } }
				: styled ? nativeElement({ k: "el", key: "state", p: { tag: "span", class: "semctl-state-ring", attrs: { "aria-hidden": true } } })
					: text(!compact && mode === "failed" ? "!" : "○", { tone: compact ? "muted" : tone, title: detail });
	// The warning has its own slot after the spacer, so it never covers the ring.
	const alert: NativeNode[] = compact && warning ? [cx.supports("icon")
		? { k: "icon", key: "warning", p: { name: "warn", role: "semctl.live.warning", tone: "warning", shrink: 0, title: detail, aria: detail } }
		: { ...text("!", { role: "semctl.live.warning", tone: "warning", shrink: 0, title: detail, aria: detail }), key: "warning" }] : [];
	const slot: NativeNode = compact && styled
		? nativeElement({ k: "el", key: "status-slot", p: { tag: "div", class: "semctl-live-progress", title: detail }, c: [mark] })
		: { k: "row", key: "status-slot", p: {
			role: "semctl.graph.status-slot", min: { w: "2.5ch" }, max: { w: "2.5ch" }, grow: 0, shrink: 0, align: "center",
		}, c: [mark] };
	const children: NativeNode[] = [slot,
		text(compact ? progress === undefined ? "—" : `${Math.floor(progress * 100)}%` : model.label, {
			role: compact ? "semctl.live.progress-value" : "semctl.graph.status-label",
			...compact ? { shrink: 0, min: { w: "4ch" } } : { grow: 1, basis: 0, min: { w: 0 } }, tone: compact ? "muted" : tone,
			wrap: "none", truncate: "end", title: boundedTooltip([detail, progress === undefined ? "Progress unknown" : undefined], 4),
		}),
		...(compact ? [{ k: "row" as const, key: "spacer", p: { grow: 1, basis: 0, min: { w: 0 } } }, ...alert] : []),
		...(requests && requests.sent > 0 ? [{ k: "row" as const, p: {
			role: "semctl.live.traffic", gap: "sm" as const, shrink: 0, align: "center" as const, title: activityTitle,
			aria: `${requests.sent} observed requests sent, ${requests.received} returned${requests.failed ? `, ${requests.failed} failed` : ""}`,
		}, c: [text(`↑ ${requests.sent}`, { tone: "muted", wrap: "none" }), text(`↓ ${requests.received}`, { tone: "muted", wrap: "none" })] }] : []),
		...(!compact && requests?.failed ? [cx.supports("icon") ? { k: "icon" as const, key: "traffic-failed", p: { name: "warn", tone: "error" as const,
			title: activityTitle, aria: `${requests.failed} observed requests failed in this session`, shrink: 0 } }
			: text(`${requests.failed} failed`, { tone: "error", title: activityTitle, shrink: 0 })] : []),
	];
	return styled ? nativeElement({ k: "el", key: "activity", p: { tag: "div", class: `semctl-graph-status is-${mode}`,
		attrs: { "data-motion": motion ? "full" : "reduced", "data-presentation": compact ? "icon" : "label", "data-status": mode }, aria: detail,
	}, c: children }) : { k: "row", key: "activity", p: { gap: "sm", align: "center", min: { h: "1.5lines" } }, c: children };
}

export const GRAPH_STATUS_STYLES = { name: "semctl-graph-status", css: `
.semctl-graph-status{display:flex;align-items:center;gap:6px;min-width:0;min-height:26px;color:var(--sf-muted);border-top:1px solid var(--l2);padding-top:6px}
.semctl-graph-status[data-presentation=icon]{box-sizing:content-box;height:22px;min-height:22px;padding-top:4px}
.semctl-live-progress{display:flex;align-items:center;justify-content:center;width:20px;height:20px;flex:0 0 20px}
[data-role="semctl.live.progress-value"]{font-size:11px;font-variant-numeric:tabular-nums}
.semctl-graph-status[data-motion=full] [data-role="semctl.live.progress-value"]{-stencil-text-effect:numeric;-stencil-text-duration:320ms}
[data-role="semctl.live.warning"]{display:flex;align-items:center;justify-content:center;flex:none;width:12px;height:12px}
[data-role="semctl.live.warning"] .ico{width:12px;height:12px}
[data-role="semctl.live.traffic"],[data-role="semctl.live.traffic"] .sf-text{font-size:11px;font-variant-numeric:tabular-nums}
.semctl-graph-status .semctl-state-ring{display:block;width:12px;height:12px;border:1.5px solid currentColor;border-radius:50%;opacity:.6;box-sizing:border-box}
.semctl-graph-status.is-working .semctl-state-ring{color:var(--sf-accent,var(--accent));border-right-color:transparent}
.semctl-graph-status.is-loading .semctl-state-ring{border-right-color:transparent}
.semctl-graph-status.is-pending .semctl-state-ring{opacity:.45}
.semctl-graph-status.is-ready .semctl-state-ring,.semctl-graph-status.is-returned .semctl-state-ring{color:var(--sf-p-success,var(--ok));opacity:.9}
.semctl-graph-status[data-motion=full] .semctl-state-ring{transition:color 160ms ease-out,opacity 160ms ease-out}
.semctl-graph-status[data-motion=full][data-presentation=label].is-working .semctl-state-ring,.semctl-graph-status[data-motion=full][data-presentation=label].is-loading .semctl-state-ring{animation:semctl-state-breathe 2200ms ease-in-out 240ms infinite}
@keyframes semctl-state-breathe{0%,100%{opacity:.45}50%{opacity:.85}}
@media(prefers-reduced-motion:reduce){.semctl-graph-status .semctl-state-ring{transition:none;animation:none}.semctl-graph-status[data-motion] [data-role="semctl.live.progress-value"]{-stencil-text-effect:none}}
` };
