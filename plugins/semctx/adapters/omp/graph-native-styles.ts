import type { NativeStylesheet } from "./native-surface";

// A single surface-lifetime sheet, independent of graph identities, labels and history.
// Coarse quarter-percent classes plus 1/32-percent residuals keep the shared sheet
// bounded while avoiding visible stair steps at the largest (760px) surface.
export const POSITION_STEPS = 400;
export const ANGLE_STEPS = 720;
const SUBPIXEL_STEPS = 8;
const PERCENT_STEP = 100 / (POSITION_STEPS * SUBPIXEL_STEPS);
const position = (value: number) => Math.max(0, Math.min(POSITION_STEPS * SUBPIXEL_STEPS, Math.round(value * POSITION_STEPS * SUBPIXEL_STEPS)));
export function positionClasses(x: number, y: number, width: number, height: number): string {
	const px = position(x / width), py = position(y / height);
	return `sg-x${Math.floor(px / SUBPIXEL_STEPS)} sg-y${Math.floor(py / SUBPIXEL_STEPS)} sg-fx${px % SUBPIXEL_STEPS} sg-fy${py % SUBPIXEL_STEPS}`;
}
export interface QuantizedGraphSegment {
	/** Percentages of surface width/height; angle in degrees. */
	x: number;
	y: number;
	length: number;
	angle: number;
}
export function quantizedSegment(from: readonly number[], to: readonly number[], width: number, height: number): QuantizedGraphSegment {
	const angle = (Math.round(Math.atan2(to[1] - from[1], to[0] - from[0]) * 360 / Math.PI) + ANGLE_STEPS) % ANGLE_STEPS;
	const length = Math.round(Math.hypot(to[0] - from[0], to[1] - from[1]) / width * POSITION_STEPS * SUBPIXEL_STEPS);
	return { x: position(from[0] / width) * PERCENT_STEP, y: position(from[1] / height) * PERCENT_STEP, length: Math.min(568 * SUBPIXEL_STEPS, length) * PERCENT_STEP, angle: angle / 2 };
}

export interface NativeGraphRouteClasses {
	readonly total: string;
	readonly segments: readonly string[];
}

/** One route-wide strip span, with each chord sampling its cumulative quantized arclength. */
export function routeStrokeClasses(points: readonly (readonly number[])[], width: number, height: number): NativeGraphRouteClasses {
	const segments: string[] = [];
	let arc = 0;
	for (let index = 1; index < points.length; index++) {
		const segment = quantizedSegment(points[index - 1], points[index], width, height);
		const x = Math.round(segment.x / PERCENT_STEP), y = Math.round(segment.y / PERCENT_STEP), length = Math.round(segment.length / PERCENT_STEP);
		const coarseArc = Math.floor(arc / SUBPIXEL_STEPS);
		segments.push(`sg-x${Math.floor(x / SUBPIXEL_STEPS)} sg-y${Math.floor(y / SUBPIXEL_STEPS)} sg-l${Math.floor(length / SUBPIXEL_STEPS)} sg-a${segment.angle * 2} sg-fx${x % SUBPIXEL_STEPS} sg-fy${y % SUBPIXEL_STEPS} sg-fl${length % SUBPIXEL_STEPS} sg-c${coarseArc % POSITION_STEPS} sg-cc${Math.floor(coarseArc / POSITION_STEPS)} sg-fc${arc % SUBPIXEL_STEPS}`);
		arc += length;
	}
	const coarseTotal = Math.floor(arc / SUBPIXEL_STEPS);
	return { total: `sg-t${coarseTotal % POSITION_STEPS} sg-tc${Math.floor(coarseTotal / POSITION_STEPS)} sg-ft${arc % SUBPIXEL_STEPS}`, segments };
}
const geometry: string[] = [];
for (let step = 0; step <= POSITION_STEPS; step++) {
	geometry.push(`.semctl-graph .sg-x${step}{left:calc(${step / 4}% + var(--sg-fx,0%))}.semctl-graph .sg-y${step}{top:calc(${step / 4}% + var(--sg-fy,0%))}`);
}
for (let step = 0; step <= 568; step++) geometry.push(`.semctl-graph .sg-l${step}{--sg-span-base:${step / 4}%}`);
for (let step = 0; step < ANGLE_STEPS; step++) geometry.push(`.semctl-graph .sg-a${step}{transform:rotate(${step / 2}deg)}`);
for (let step = 0; step < SUBPIXEL_STEPS; step++) geometry.push(`.semctl-graph .sg-fx${step}{--sg-fx:${step * PERCENT_STEP}%}.semctl-graph .sg-fy${step}{--sg-fy:${step * PERCENT_STEP}%}.semctl-graph .sg-fl${step}{--sg-span-fine:${step * PERCENT_STEP}%}.semctl-graph .sg-fc${step}{--sg-arc-fine:${step / (POSITION_STEPS * SUBPIXEL_STEPS)}}.semctl-graph .sg-ft${step}{--sg-total-fine:${step / (POSITION_STEPS * SUBPIXEL_STEPS)}}`);
for (let step = 0; step < POSITION_STEPS; step++) geometry.push(`.semctl-graph .sg-c${step}{--sg-arc-base:${step / POSITION_STEPS}}.semctl-graph .sg-t${step}{--sg-total-base:${step / POSITION_STEPS}}`);
for (let step = 0; step <= 8; step++) geometry.push(`.semctl-graph .sg-cc${step}{--sg-arc-carry:${step}}.semctl-graph .sg-tc${step}{--sg-total-carry:${step}}`);

export const GRAPH_STYLES: readonly NativeStylesheet[] = Object.freeze([Object.freeze({
	name: "semctl-native-graph",
	css: `
.semctl-graph{position:relative;width:100%;max-width:560px;color:var(--sg-edge);--sg-fg:var(--sf-p-text,var(--t1));--sg-edge:var(--sf-muted);--sg-accent:var(--sf-accent,var(--accent));--sg-positive:var(--sf-p-success,var(--sf-c-success,var(--ok)));--sg-negative:var(--sf-p-error,var(--sf-c-error,var(--bad)));--sg-bg:var(--panel);isolation:isolate}
.semctl-graph.sg-compact{max-width:280px}
.semctl-graph.sg-overview{max-width:760px}
.semctl-graph .sg-ratio{height:0;padding-bottom:47.142857%}
.semctl-graph.sg-small .sg-ratio{padding-bottom:27.857143%}
.semctl-graph.sg-medium .sg-ratio{padding-bottom:38.571429%}
.semctl-graph.sg-compact .sg-ratio{padding-bottom:62.857143%}
.semctl-graph.sg-overview .sg-ratio{padding-bottom:42.105263%}
.semctl-graph .sg-plane,.semctl-graph .sg-route,.semctl-graph .sg-node,.semctl-graph .sg-hover-links{position:absolute;inset:0;pointer-events:none}
.semctl-graph .sg-plane{overflow:hidden}
.semctl-graph .sg-route{color:var(--sg-edge);opacity:.38}
.semctl-graph.sg-focused .sg-route{opacity:.12}
.semctl-graph .sg-route.sg-related{color:var(--sg-route-ink,var(--sg-accent));opacity:.86}
.semctl-graph.sg-links-soft .sg-route{opacity:.45}
.semctl-graph.sg-links-clear .sg-route{opacity:.65}
.semctl-graph.sg-links-soft.sg-focused .sg-route{opacity:.16}
.semctl-graph.sg-links-clear.sg-focused .sg-route{opacity:.22}
.semctl-graph.sg-links-soft .sg-route.sg-related,.semctl-graph.sg-links-clear .sg-route.sg-related{color:var(--sg-route-ink,var(--sg-accent));opacity:.95}
.semctl-graph .sg-route.sg-observed,.semctl-graph.sg-focused .sg-route.sg-observed{--sg-route-ink:var(--sg-positive);color:var(--sg-positive);opacity:.8}
.semctl-graph .sg-route{--sg-total:calc(var(--sg-total-carry,0) + var(--sg-total-base,0) + var(--sg-total-fine,0))}
.semctl-graph .sg-segment{--sg-span:calc(var(--sg-span-base,0%) + var(--sg-span-fine,0%));--sg-arc:calc(var(--sg-arc-carry,0) + var(--sg-arc-base,0) + var(--sg-arc-fine,0));position:absolute;width:100%;height:6px;margin-top:-3px;transform-origin:0 3px;pointer-events:none}
.semctl-graph .sg-hit{position:absolute;left:0;top:0;width:var(--sg-span);height:6px;pointer-events:auto}
.semctl-graph .sg-stroke{position:absolute;top:2px;left:0;width:100%;height:2px;clip-path:inset(0 calc(100% - var(--sg-span) - .5px) 0 0 round 1px);mask-image:linear-gradient(to bottom,transparent,#fff .35px,#fff 1.65px,transparent 2px);mask-size:100% 100%;mask-repeat:no-repeat;pointer-events:none}
.semctl-graph .sg-ink{position:absolute;left:calc(-100% * var(--sg-arc));top:0;width:calc(100% * var(--sg-total));height:100%;background:currentColor;pointer-events:none}
.semctl-graph .sg-import{--sg-dash:max(8px,calc(8px / var(--sf-zoom,1)));--sg-gap:max(6px,calc(6px / var(--sf-zoom,1)))}
.semctl-graph .sg-association{--sg-dash:max(4px,calc(4px / var(--sf-zoom,1)));--sg-gap:max(8px,calc(8px / var(--sf-zoom,1)))}
.semctl-graph .sg-import .sg-ink,.semctl-graph .sg-association .sg-ink{--sg-period:calc(var(--sg-dash) + var(--sg-gap));mask-image:linear-gradient(to right,#fff 0 var(--sg-dash),transparent var(--sg-dash) var(--sg-period));mask-size:var(--sg-period) 100%;mask-position:0 0;mask-repeat:space no-repeat}
.semctl-graph .sg-arrow{position:absolute;left:calc(var(--sg-span) - 4px);top:1px;width:4px;height:4px;border-top:1px solid currentColor;border-right:1px solid currentColor;transform:rotate(45deg);pointer-events:none}
.semctl-graph .sg-target{position:absolute;width:24px;height:24px;transform:translate(-50%,-50%);padding:0;border:0;border-radius:50%;background:transparent;pointer-events:auto;z-index:2}
.semctl-graph .sg-target.sg-action{cursor:pointer}
.semctl-graph.sg-neutral .sg-target{color:var(--sg-fg)}
.semctl-graph .sg-dot{position:absolute;left:50%;top:50%;width:7px;height:7px;transform:translate(-50%,-50%);border-radius:50%;background:currentColor;pointer-events:none}
.semctl-graph .sg-dot.sg-large{width:10px;height:10px}
.semctl-graph .sg-root .sg-dot{outline:1px solid var(--sg-accent);outline-offset:1px}
.semctl-graph .sg-halo{position:absolute;left:50%;top:50%;width:17px;height:17px;transform:translate(-50%,-50%);border:1px solid currentColor;border-radius:50%;opacity:0;pointer-events:none}
.semctl-graph .sg-active .sg-halo{color:var(--sg-positive);opacity:.65}
.semctl-graph .sg-feedback{position:absolute;left:50%;top:50%;width:18px;height:18px;transform:translate(-50%,-50%);border:2px solid currentColor;border-radius:50%;opacity:.75;pointer-events:none}
.semctl-graph .sg-feedback.sg-returned{color:var(--sg-positive)}
.semctl-graph .sg-feedback.sg-failed{color:var(--sg-negative)}
.semctl-graph .sg-indexing-state{position:absolute;left:50%;top:50%;width:23px;height:23px;transform:translate(-50%,-50%);border:1px solid var(--sg-edge);border-radius:50%;opacity:.75;pointer-events:none}
.semctl-graph .sg-indexing-pending .sg-indexing-state{opacity:.4}
.semctl-graph .sg-indexing-processing .sg-indexing-state{border-color:var(--sg-accent);border-width:1.5px}
.semctl-graph .sg-indexing-failed .sg-indexing-state{border-color:var(--sg-negative);border-width:1.5px;opacity:.65}
.semctl-graph.sg-compact .sg-indexing-state{box-sizing:border-box;width:17px;height:17px}
.semctl-graph .sg-selected .sg-dot,.semctl-graph .sg-target:hover .sg-dot,.semctl-graph .sg-target:focus-visible .sg-dot{outline:2px solid var(--sg-accent);outline-offset:3px}
.semctl-graph .sg-muted .sg-dot{opacity:.4}
.semctl-graph .sg-label{position:absolute;left:21px;top:-4px;max-width:180px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis;font:450 11px/14px var(--sans,system-ui,sans-serif);color:var(--sg-fg);filter:drop-shadow(1px 0 1px var(--sg-bg)) drop-shadow(-1px 0 1px var(--sg-bg));pointer-events:none;text-align:left}
.semctl-graph .sg-label.sg-label-left{left:auto;right:21px;text-align:right}
.semctl-graph .sg-label.sg-label-below{top:19px}
.semctl-graph .sg-selected .sg-label{font-weight:600}
.semctl-graph .sg-label.sg-label-transient{visibility:hidden}
.semctl-graph .sg-selected .sg-label-transient,.semctl-graph .sg-target:hover .sg-label-transient,.semctl-graph .sg-target:focus-visible .sg-label-transient{visibility:visible}
.semctl-graph .sg-target:hover,.semctl-graph .sg-target:focus-visible,.semctl-graph .sg-target.sg-selected{z-index:3}
.semctl-graph .sg-hover-links{opacity:0;z-index:1}
.semctl-graph .sg-hover-links .sg-route{opacity:.9;color:var(--sg-route-ink,var(--sg-accent))}
.semctl-graph.sg-links-soft .sg-hover-links .sg-route,.semctl-graph.sg-links-clear .sg-hover-links .sg-route{opacity:.95;color:var(--sg-route-ink,var(--sg-accent))}
.semctl-graph .sg-hover-links .sg-hit{pointer-events:none}
.semctl-graph .sg-node:hover .sg-hover-links,.semctl-graph .sg-node:focus-within .sg-hover-links{opacity:1}
.semctl-graph[data-motion=full] .sg-halo{transition:opacity 180ms ease-out}
.semctl-graph[data-motion=full] .sg-active .sg-halo{animation:semctl-graph-active 3200ms ease-in-out 400ms infinite}
.semctl-graph[data-motion=full] .sg-hover-links{transition:opacity 100ms ease-out}
.semctl-graph[data-motion=full] .sg-route{transition:opacity 140ms ease-out}
.semctl-graph[data-motion=full] .sg-plane{animation:semctl-graph-arrive 160ms ease-out 1}
.semctl-graph[data-motion=full] .sg-feedback{animation:semctl-graph-receipt 1000ms ease-out 1 both}
.semctl-graph[data-motion=full] .sg-indexing-failed .sg-indexing-state{animation:semctl-graph-failure 180ms ease-out 1}
@keyframes semctl-graph-arrive{from{opacity:.35}to{opacity:1}}
@keyframes semctl-graph-failure{from{opacity:.2}to{opacity:.65}}
@keyframes semctl-graph-receipt{from{opacity:.75}to{opacity:0}}
@keyframes semctl-graph-active{0%,100%{opacity:.65}50%{opacity:.5}}
@media (prefers-reduced-motion:reduce){.semctl-graph[data-motion=full] .sg-plane,.semctl-graph[data-motion=full] .sg-route,.semctl-graph[data-motion=full] .sg-halo,.semctl-graph[data-motion=full] .sg-active .sg-halo,.semctl-graph[data-motion=full] .sg-hover-links,.semctl-graph[data-motion=full] .sg-feedback,.semctl-graph[data-motion=full] .sg-indexing-failed .sg-indexing-state{transition:none;animation:none}}
${geometry.join("")}`,
})]);
