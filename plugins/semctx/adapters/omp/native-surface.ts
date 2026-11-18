import type { DescribeContext, TUI } from "@oh-my-pi/pi-tui";
import { GRAPH_STYLES } from "./graph-native-styles";
import { CONCEPT_STYLES } from "./native-concepts";
import { supportsElements } from "./native-elements";
import { GRAPH_APPEARANCE_STYLES } from "./graph-appearance-styles";
import { RESULT_STYLES } from "./result-styles";
import { GRAPH_STATUS_STYLES } from "./graph-status";

/** Tern's surface-confined stylesheet message. Stock OMP has no stylesheet node prop. */
export interface NativeStylesheet { name: string; css: string }

/** Tern's dock width rule needs an important override. The height allowance keeps the card below shell chrome and above the composer. */
const POPUP_STYLES: NativeStylesheet = { name: "semctl-popups", css: `
[data-role="semctl.sync.anchor"]{align-self:stretch;padding:0 2px 2px}
.semctl-sync{display:flex;align-items:center;gap:6px;align-self:flex-start;width:76px;height:24px;flex:none;box-sizing:border-box;margin:0;padding:0 8px;border:0;border-radius:var(--r-ctl);background:transparent;color:var(--t2);font-family:var(--sans);font-size:12px;cursor:pointer}
.semctl-sync .sf-text{font-family:var(--sans);font-size:12px}
.semctl-sync [data-role="semctl.sync.progress"]{display:flex;align-items:center;justify-content:center;width:14px;height:14px;flex:none}
.semctl-sync:hover,.semctl-sync:focus-visible{background:var(--l1);color:var(--t1)}
.semctl-sync[data-state=ready],.semctl-sync[data-state=idle]{opacity:.82}
.semctl-sync[data-motion=full]{transition:background-color 120ms ease-out,color 160ms ease-out,opacity 180ms ease-out}
.semctl-sync[data-motion=full][data-state=active] .sf-meter.unknown{animation:semctl-sync-working 2600ms ease-in-out 400ms infinite}
@keyframes semctl-sync-working{0%,100%{opacity:.65}50%{opacity:1}}
@media(prefers-reduced-motion:reduce){.semctl-sync,.semctl-sync .sf-meter.unknown{transition:none;animation:none}}
.sf-overlay[data-role="omp.overlay.semctl-live"]{--semctl-popup-height:min(420px,calc(100vh - 320px));position:absolute;inset:auto 0 calc(100% + 12px) auto;width:min(320px,92vw) !important;max-width:92vw;height:auto;display:block;pointer-events:none;z-index:20}
.sf-overlay[data-role="omp.overlay.semctl-live"].sz-md{--semctl-popup-height:min(560px,calc(100vh - 320px));width:min(480px,92vw) !important}
.sf-overlay[data-role="omp.overlay.semctl-live"].sz-lg{--semctl-popup-height:min(720px,calc(100vh - 320px));width:min(680px,92vw) !important}
.sf-overlay[data-role="omp.overlay.semctl-live"]>.sf-ov-card{width:100%;max-width:100%;max-height:var(--semctl-popup-height);margin:0;pointer-events:auto}
[data-role="omp.overlay.semctl-live"] .sf-ov-body{overflow:hidden}
[data-role="semctl.live.body"]{min-width:0;overflow-x:hidden;overflow-y:auto;max-height:calc(var(--semctl-popup-height) - 120px)}
[data-role="omp.overlay.semctl-live"] .semctl-graph-well .semctl-graph{max-width:none}
.semctl-live-empty{height:176px;display:flex;flex-direction:column;align-items:center;justify-content:center;gap:8px;text-align:center;font-size:12px}
[data-role="omp.overlay.semctl-live"].sz-md .semctl-live-empty{height:188px}
[data-role="omp.overlay.semctl-live"].sz-lg .semctl-live-empty{height:288px}
[data-role="semctl.live.header"]{gap:2px;min-width:0}
[data-role="semctl.live.title"]{font-size:13px;font-weight:600;color:var(--t1);cursor:default;user-select:none}
[data-role="semctl.live.subline"]{font-size:11px;line-height:16px;color:var(--t2);font-variant-numeric:tabular-nums}
[data-role="semctl.live.partial"],[data-role="semctl.live.partial"] .ico{width:12px;height:12px;flex:none}
[data-role="semctl.live.indexing-row"]{padding:4px 0}
.sf-text[data-role="semctl.live.indexing"]{padding:4px 0;font-size:11px;color:var(--t2)}
[data-role="semctl.live.indexing-label"]{font-family:var(--sans);font-size:11px;color:var(--t2);font-variant-numeric:tabular-nums}
[data-role="semctl.live.body"]>.semctl-graph-well{flex:none;margin:2px 0}
[data-role="semctl.live.selection"]{height:66px;min-height:66px;max-height:66px;flex:none;box-sizing:border-box;gap:1px;padding-top:4px;border-top:1px solid var(--l2);overflow:hidden;font-size:11px}
[data-role="semctl.live.selection"]>:first-child{height:20px;min-height:20px;flex:0 0 20px}
[data-role="semctl.live.selection"]>:nth-child(2){height:16px;min-height:16px;flex:0 0 16px}
[data-role="semctl.live.selection"]>:last-child{height:22px;min-height:22px;flex:0 0 22px}
[data-role="semctl.live.selection"] .semctl-icon-action{width:20px;height:20px}
.semctl-live-legend{display:flex;align-items:center;gap:8px;flex:1 1 auto;min-width:0;overflow:hidden;white-space:nowrap;font-size:10.5px;color:var(--sf-muted)}
.semctl-legend-item{display:flex;align-items:center;gap:4px;flex:none;min-width:0}
.semctl-legend-item:last-child{flex:0 1 auto}
.semctl-legend-label{min-width:0;overflow:hidden;text-overflow:ellipsis}
.semctl-legend-size,.semctl-legend-weight,.semctl-legend-tint{flex:none}
.semctl-legend-size{width:6px;height:6px;border-radius:50%;background:var(--t3)}
.semctl-legend-weight{width:12px;height:2px;border-radius:1px;background:var(--sf-muted)}
.semctl-legend-tint{width:8px;height:8px;border-radius:50%;background:color-mix(in oklab, var(--sf-accent, var(--accent)) 25%, transparent)}
.semctl-live-group,.semctl-live-neighbor{display:flex;align-items:center;gap:4px;min-width:0;box-sizing:border-box;border:0;border-radius:var(--r-ctl);background:transparent;color:var(--t2);font:inherit;text-align:left;cursor:pointer}
.semctl-live-group{width:100%;gap:6px;padding:4px 6px;border-radius:6px;font-size:12px}
.semctl-live-neighbor{height:22px;padding:1px 4px;background:var(--l1);font-size:11px}
.semctl-live-group:hover,.semctl-live-group:focus-visible{background:var(--l1);color:var(--t1)}
.semctl-live-group[aria-pressed=true],.semctl-live-neighbor:hover,.semctl-live-neighbor:focus-visible{background:var(--l2);color:var(--t1)}
.semctl-live-group[data-motion=full]{transition:background-color 120ms var(--ease)}
[data-role="semctl.live.group.indexing"]{flex:none}
[data-role="semctl.live.group.status"],[data-role="semctl.live.group.observation"]{padding:1px 4px;border-radius:3px;background:var(--l1);font-family:var(--sans);font-size:10px;white-space:nowrap}
.semctl-live-badge-error{color:var(--sf-p-error,var(--sf-c-error,var(--bad)))}
.semctl-live-badge-success{color:var(--sf-p-success,var(--sf-c-success,var(--ok)))}
.semctl-live-badge-muted{color:var(--t2)}
[data-role="semctl.live.group.count"]{min-width:18px;text-align:right;font-variant-numeric:tabular-nums}
.semctl-icon-action{display:flex;align-items:center;justify-content:center;width:28px;height:28px;flex-shrink:0;padding:0;border:0;border-radius:var(--r-ctl);background:transparent;color:var(--sf-muted);cursor:pointer;transition:background-color 100ms var(--ease),color 100ms var(--ease)}
.semctl-icon-action:hover,.semctl-icon-action:focus-visible{background:var(--l1);color:var(--t1)}
.semctl-icon-action[aria-disabled=true]{opacity:.45;cursor:default;background:transparent}
.semctl-icon-action[aria-busy=true]{opacity:.8}
.semctl-icon-action[aria-pressed=true]{color:var(--t1);background:var(--l1)}
.semctl-icon-action[data-role="semctl.live.resize"]{width:20px;color:var(--t3);cursor:nwse-resize}
.semctl-icon-action[data-role="semctl.live.resize"]:hover,.semctl-icon-action[data-role="semctl.live.resize"]:focus-visible{color:var(--t1)}
@media (prefers-reduced-motion:reduce){.semctl-icon-action,.semctl-live-group[data-motion]{transition:none}}
[data-role="semctl.status.snapshot"],[data-role="semctl.graph.body"]{overflow:auto}
.sf-overlay[data-role="omp.overlay.semctl-graph"]>.sf-ov-card{width:min(800px,calc(100vw - 40px))}
[data-role="semctl.graph.body"]{height:min(448px,calc(100vh - 220px));min-width:0;min-height:120px;max-height:calc(100vh - 180px);overflow-x:hidden}
[data-role="semctl.graph.hint"]{font-size:11px}
[data-role="semctl.graph.title"]{font-size:13px;font-weight:600}
[data-role="semctl.graph.group"]{gap:8px}
` };
const active = new Map<TUI, { references: number; context?: DescribeContext }>();
let encodedStyles: readonly string[] = [];

/** The entry point supplies OMP's public TSP encoder, shared with its bundled runtime. */
export function configureNativeStyles(encode: (sheet: NativeStylesheet) => string): void {
	// Subagents bind the same extension factory. Do not reinstall CSS in the parent.
	if (encodedStyles.length > 0) return;
	encodedStyles = [...GRAPH_STYLES, ...CONCEPT_STYLES, ...GRAPH_APPEARANCE_STYLES, ...RESULT_STYLES, GRAPH_STATUS_STYLES, POPUP_STYLES].map(encode);
}

/** Concept updates can arrive after native event dispatch has already repainted. */
export function requestNativeRender(): void {
	for (const tui of active.keys()) tui.requestRender();
}

/** Mount only on OMP's inline surface. Native modal cards and widgets share that surface. */
export class NativeSurface {
	private disposed = false;
	constructor(private readonly tui: TUI) {
		const state = active.get(tui);
		if (state) state.references++; else active.set(tui, { references: 1 });
	}
	prepare(cx: DescribeContext): void {
		const state = active.get(this.tui);
		if (this.disposed || !state || state.context === cx || !supportsElements(cx) || !cx.feature("styles") || encodedStyles.length === 0) return;
		// OMP opens the native surface before it describes its components. Omitting sf
		// targets that live surface. A new hello supplies a new context after reconnect.
		for (const message of encodedStyles) this.tui.terminal.write(message);
		state.context = cx;
	}
	dispose(): void {
		if (this.disposed) return;
		this.disposed = true;
		const state = active.get(this.tui);
		if (state && --state.references === 0) active.delete(this.tui);
	}
}
