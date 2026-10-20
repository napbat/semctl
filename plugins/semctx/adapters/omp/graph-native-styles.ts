import { conceptSwatches } from "./graph-data/palette.js";
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
for (let step = 7; step <= 19; step++) geometry.push(`.semctl-graph .sg-target.sg-s${step}{--sg-d:${step}px}`);
for (let step = 0; step <= 200; step++) geometry.push(`.semctl-graph .sg-tw${step}{width:${step / 2}%}.semctl-graph .sg-th${step}{height:${step / 2}%}`);
for (const swatch of conceptSwatches) for (const tint of [swatch.light, swatch.dark]) geometry.push(`.semctl-graph .sg-tint-${tint.slice(1)}{--sg-tint:${tint}}`);
for (const [index, scale] of [1, 1.35, 1.8, 2.4].entries()) geometry.push(`.semctl-graph .sg-route.sg-w${index + 1}{--sg-weight-opacity:${[.55, .85, 1.1, 1.4][index]}}.semctl-graph .sg-w${index + 1} .sg-stroke{transform:scaleY(${scale})}`);
for (let step = 0; step < 8; step++) geometry.push(`.semctl-graph[data-motion=full] .sg-d${step} .sg-dot{animation-delay:${step * 40}ms}.semctl-graph[data-motion=full] .sg-d${step} .sg-label{animation-delay:${step * 40 + 380}ms}`);

export const GRAPH_STYLES: readonly NativeStylesheet[] = Object.freeze([Object.freeze({
	name: "semctl-native-graph",
	css: `
.semctl-graph-well{--sg-well:color-mix(in oklab, var(--panel) 95%, var(--t1));--sg-bg:var(--sg-well);display:flex;justify-content:center;border-radius:8px;background:var(--sg-well);padding:6px;border:0}
.semctl-graph{position:relative;width:100%;max-width:560px;color:var(--sg-node);--sg-fg:var(--sf-p-text,var(--t1));--sg-node:color-mix(in oklab, var(--sg-fg) 45%, var(--sg-bg));--sg-edge:var(--sf-muted);--sg-accent:var(--sf-accent,var(--accent));--sg-positive:var(--sf-p-success,var(--sf-c-success,var(--ok)));--sg-negative:var(--sf-p-error,var(--sf-c-error,var(--bad)));--sg-bg:var(--sg-well,var(--panel));isolation:isolate}
.semctl-graph-well .semctl-graph{flex:1 1 auto;width:100%}
.semctl-graph.sg-compact{max-width:280px}
.semctl-graph.sg-overview{max-width:760px}
.semctl-graph .sg-ratio{height:0;padding-bottom:47.142857%}
.semctl-graph.sg-small .sg-ratio{padding-bottom:27.857143%}
.semctl-graph.sg-medium .sg-ratio{padding-bottom:38.571429%}
.semctl-graph.sg-compact .sg-ratio{padding-bottom:62.857143%}
.semctl-graph.sg-overview .sg-ratio{padding-bottom:42.105263%}
.semctl-graph .sg-plane,.semctl-graph .sg-territories,.semctl-graph .sg-routes,.semctl-graph .sg-route,.semctl-graph .sg-node,.semctl-graph .sg-hover-links{position:absolute;inset:0;pointer-events:none}
.semctl-graph .sg-plane{overflow:hidden}
.semctl-graph .sg-territory{position:absolute;transform:translate(-50%,-50%);box-sizing:border-box;border-radius:50%;background:radial-gradient(closest-side,rgb(from var(--sg-tint) r g b / 12%),rgb(from var(--sg-tint) r g b / 5%) 72%,transparent);border:1px solid rgb(from var(--sg-tint) r g b / 16%);pointer-events:none}
.semctl-graph.sg-compact .sg-territory{background:radial-gradient(closest-side,rgb(from var(--sg-tint) r g b / 9%),rgb(from var(--sg-tint) r g b / 4%) 72%,transparent)}
.semctl-graph .sg-territory-label{position:absolute;left:50%;top:0;transform:translate(-50%,-50%);box-sizing:border-box;height:14px;max-width:160px;padding:0 5px;border:1px solid rgb(from var(--sg-tint) r g b / 16%);border-radius:999px;background:color-mix(in oklab, var(--sg-bg) 85%, transparent);color:var(--sg-tint);font:600 10px/12px var(--sans,system-ui,sans-serif);white-space:nowrap;overflow:hidden;text-overflow:ellipsis;pointer-events:auto}
.semctl-graph .sg-territory-label.sg-territory-label-bottom{top:100%}
.semctl-graph .sg-territory.sg-territory-family{background:radial-gradient(closest-side,rgb(from var(--sg-tint) r g b / 16%),rgb(from var(--sg-tint) r g b / 8%) 74%,rgb(from var(--sg-tint) r g b / 3%));border:1px solid rgb(from var(--sg-tint) r g b / 26%)}
.semctl-graph.sg-compact .sg-territory.sg-territory-family{background:radial-gradient(closest-side,rgb(from var(--sg-tint) r g b / 12%),rgb(from var(--sg-tint) r g b / 6%) 74%,rgb(from var(--sg-tint) r g b / 2%))}
.semctl-graph .sg-territory-family .sg-territory-label{border:1px solid rgb(from var(--sg-tint) r g b / 30%);background:color-mix(in oklab, var(--sg-tint) 10%, var(--sg-bg))}
.semctl-graph.sg-neutral .sg-territory{--sg-tint:var(--sg-edge)}
.semctl-graph.sg-focused .sg-territories{--sg-layer-opacity:.3;opacity:.3}
.semctl-graph .sg-route{color:var(--sg-edge);opacity:calc(.28 * var(--sg-weight-opacity,1))}
.semctl-graph.sg-links-soft .sg-route{opacity:calc(.28 * var(--sg-weight-opacity,1))}
.semctl-graph.sg-links-clear .sg-route{opacity:calc(.42 * var(--sg-weight-opacity,1))}
.semctl-graph.sg-compact .sg-route{opacity:calc(.2 * var(--sg-weight-opacity,1))}
.semctl-graph .sg-route.sg-containment,.semctl-graph .sg-route.sg-association{opacity:calc(.08 * var(--sg-weight-opacity,1))}
.semctl-graph.sg-focused .sg-route{opacity:.06}
.semctl-graph .sg-route.sg-related{color:var(--sg-route-ink,var(--sg-accent));opacity:.9;z-index:1}
.semctl-graph .sg-route.sg-observed{--sg-route-ink:var(--sg-positive);color:var(--sg-positive)}
.semctl-graph .sg-route{--sg-total:calc(var(--sg-total-carry,0) + var(--sg-total-base,0) + var(--sg-total-fine,0))}
.semctl-graph .sg-segment{--sg-span:calc(var(--sg-span-base,0%) + var(--sg-span-fine,0%));--sg-arc:calc(var(--sg-arc-carry,0) + var(--sg-arc-base,0) + var(--sg-arc-fine,0));position:absolute;width:100%;height:6px;margin-top:-3px;transform-origin:0 3px;pointer-events:none}
.semctl-graph .sg-hit{position:absolute;left:0;top:0;width:var(--sg-span);height:6px;pointer-events:auto}
.semctl-graph .sg-stroke{position:absolute;top:2.4px;left:0;width:100%;height:1.2px;clip-path:inset(0 calc(100% - var(--sg-span) - .5px) 0 0 round 1px);mask-image:linear-gradient(to bottom,transparent,#fff .1px,#fff 1.1px,transparent 1.2px);mask-size:100% 100%;mask-repeat:no-repeat;pointer-events:none}
.semctl-graph .sg-ink{position:absolute;left:calc(-100% * var(--sg-arc));top:0;width:calc(100% * var(--sg-total));height:100%;background:currentColor;pointer-events:none}
.semctl-graph .sg-import{--sg-dash:max(8px,calc(8px / var(--sf-zoom,1)));--sg-gap:max(6px,calc(6px / var(--sf-zoom,1)))}
.semctl-graph .sg-association{--sg-dash:max(4px,calc(4px / var(--sf-zoom,1)));--sg-gap:max(8px,calc(8px / var(--sf-zoom,1)))}
.semctl-graph .sg-import .sg-ink,.semctl-graph .sg-association .sg-ink{--sg-period:calc(var(--sg-dash) + var(--sg-gap));mask-image:linear-gradient(to right,#fff 0 var(--sg-dash),transparent var(--sg-dash) var(--sg-period));mask-size:var(--sg-period) 100%;mask-position:0 0;mask-repeat:space no-repeat}
.semctl-graph .sg-arrow{position:absolute;left:calc(var(--sg-span) - 3px);top:1.5px;width:3px;height:3px;border-top:1px solid currentColor;border-right:1px solid currentColor;transform:rotate(45deg);pointer-events:none}
.semctl-graph.sg-compact .sg-arrow{visibility:hidden}
.semctl-graph .sg-route.sg-related .sg-arrow{visibility:visible}
.semctl-graph .sg-target{--sg-d:7px;position:absolute;width:max(24px,calc(var(--sg-d) + 8px));height:max(24px,calc(var(--sg-d) + 8px));transform:translate(-50%,-50%);padding:0;border:0;border-radius:50%;background:transparent;pointer-events:auto;z-index:2}
.semctl-graph .sg-target.sg-action{cursor:pointer}
.semctl-graph.sg-neutral .sg-target{color:var(--sg-node)}
.semctl-graph .sg-dot{position:absolute;left:50%;top:50%;width:var(--sg-d);height:var(--sg-d);transform:translate(-50%,-50%) scale(var(--sg-dot-scale,1));border-radius:50%;background:currentColor;box-shadow:0 0 0 1.5px var(--sg-bg);pointer-events:none}
.semctl-graph .sg-root .sg-dot{outline:1px solid var(--sg-accent);outline-offset:1px}
.semctl-graph .sg-halo{position:absolute;left:50%;top:50%;width:calc(var(--sg-d) + 10px);height:calc(var(--sg-d) + 10px);transform:translate(-50%,-50%);border:1px solid currentColor;border-radius:50%;opacity:0;pointer-events:none}
.semctl-graph .sg-active .sg-halo{color:var(--sg-positive);opacity:.65}
.semctl-graph .sg-feedback{position:absolute;left:50%;top:50%;width:calc(var(--sg-d) + 11px);height:calc(var(--sg-d) + 11px);transform:translate(-50%,-50%);border:2px solid currentColor;border-radius:50%;opacity:.75;pointer-events:none}
.semctl-graph .sg-feedback.sg-returned{color:var(--sg-positive)}
.semctl-graph .sg-feedback.sg-failed{color:var(--sg-negative)}
.semctl-graph .sg-indexing-state{position:absolute;left:calc(50% + (var(--sg-d) / 2 + 2px) * .62);top:calc(50% + (var(--sg-d) / 2 + 2px) * .62);box-sizing:border-box;width:5px;height:5px;transform:translate(-50%,-50%);border:0;border-radius:50%;background:transparent;box-shadow:0 0 0 1.5px var(--sg-bg);pointer-events:none;z-index:1}
.semctl-graph .sg-indexing-pending .sg-indexing-state{border:1px solid var(--sg-edge)}
.semctl-graph .sg-indexing-processing .sg-indexing-state{background:var(--sg-accent)}
.semctl-graph .sg-indexing-failed .sg-indexing-state{background:var(--sg-negative)}
.semctl-graph .sg-pip-faint .sg-indexing-state{--sg-pip-opacity:.4;opacity:.4;width:4px;height:4px}
.semctl-graph .sg-pip-mid .sg-indexing-state{--sg-pip-opacity:.75;opacity:.75}
.semctl-graph .sg-pip-strong .sg-indexing-state{--sg-pip-opacity:1;opacity:1;width:6px;height:6px}
.semctl-graph .sg-selected .sg-dot{outline:2px solid var(--sg-accent);outline-offset:2px}
.semctl-graph .sg-target:hover .sg-dot,.semctl-graph .sg-target:focus-visible .sg-dot{--sg-dot-scale:1.18}
.semctl-graph .sg-muted .sg-dot{--sg-dot-opacity:.28;opacity:.28}
.semctl-graph .sg-label{position:absolute;left:calc(50% + var(--sg-d) / 2 + 5px);bottom:calc(50% + var(--sg-d) / 2 - 1px);max-width:180px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis;font:500 11px/14px var(--sans,system-ui,sans-serif);color:var(--t2);filter:drop-shadow(1px 0 1px var(--sg-bg)) drop-shadow(-1px 0 1px var(--sg-bg));pointer-events:none;text-align:left}
.semctl-graph.sg-compact .sg-label{font-size:10.5px}
.semctl-graph .sg-label.sg-label-left{left:auto;right:calc(50% + var(--sg-d) / 2 + 5px);text-align:right}
.semctl-graph .sg-label.sg-label-below{bottom:auto;top:calc(50% + var(--sg-d) / 2 + 3px)}
.semctl-graph .sg-selected .sg-label,.semctl-graph .sg-target:hover .sg-label,.semctl-graph .sg-target:focus-visible .sg-label{color:var(--t1);font-weight:600}
.semctl-graph .sg-label.sg-label-transient{visibility:hidden}
.semctl-graph .sg-selected .sg-label-transient,.semctl-graph .sg-target:hover .sg-label-transient,.semctl-graph .sg-target:focus-visible .sg-label-transient{visibility:visible}
.semctl-graph .sg-target:hover,.semctl-graph .sg-target:focus-visible,.semctl-graph .sg-target.sg-selected{z-index:3}
.semctl-graph .sg-hover-links{opacity:0;z-index:1}
.semctl-graph .sg-hover-links .sg-hit{pointer-events:none}
.semctl-graph .sg-node:hover .sg-hover-links,.semctl-graph .sg-node:focus-within .sg-hover-links{opacity:1}
.semctl-graph[data-motion=full] .sg-target{transition:left 560ms spring(560ms,.08),top 560ms spring(560ms,.08)}
.semctl-graph[data-motion=full] .sg-dot{transition:transform 120ms ease-out,opacity 160ms ease-out,outline-offset 160ms ease-out;animation:semctl-graph-dot-arrive 380ms cubic-bezier(.2,.8,.2,1) 1 backwards}
.semctl-graph[data-motion=full] .sg-label{animation:semctl-graph-fade 180ms ease-out 380ms 1 backwards}
.semctl-graph[data-motion=full] .sg-routes{animation:semctl-graph-fade 240ms ease-out 360ms 1 backwards}
.semctl-graph[data-motion=full] .sg-territories{transition:opacity 160ms ease-out;animation:semctl-graph-fade 300ms ease-out 200ms 1 backwards}
.semctl-graph[data-motion=full] .sg-halo{transition:opacity 180ms ease-out}
.semctl-graph[data-motion=full] .sg-active .sg-halo{animation:semctl-graph-active 3200ms ease-in-out 400ms infinite}
.semctl-graph[data-motion=full] .sg-hover-links{transition:opacity 100ms ease-out}
.semctl-graph[data-motion=full] .sg-route{transition:opacity 140ms ease-out}
.semctl-graph[data-motion=full] .sg-feedback{animation:semctl-graph-receipt 1000ms ease-out 1 both}
.semctl-graph[data-motion=full] .sg-indexing-failed .sg-indexing-state{animation:semctl-graph-failure 180ms ease-out 1}
@keyframes semctl-graph-dot-arrive{from{opacity:0;transform:translate(-50%,-50%) scale(.35)}to{opacity:var(--sg-dot-opacity,1);transform:translate(-50%,-50%) scale(var(--sg-dot-scale,1))}}
@keyframes semctl-graph-fade{from{opacity:0}to{opacity:var(--sg-layer-opacity,1)}}
@keyframes semctl-graph-failure{from{opacity:.2}to{opacity:var(--sg-pip-opacity,1)}}
@keyframes semctl-graph-receipt{from{opacity:.75}to{opacity:0}}
@keyframes semctl-graph-active{0%,100%{opacity:.65}50%{opacity:.5}}
.semctl-graph[data-motion=reduced] .sg-target,.semctl-graph[data-motion=reduced] .sg-dot,.semctl-graph[data-motion=reduced] .sg-label,.semctl-graph[data-motion=reduced] .sg-routes,.semctl-graph[data-motion=reduced] .sg-territories,.semctl-graph[data-motion=reduced] .sg-route,.semctl-graph[data-motion=reduced] .sg-halo,.semctl-graph[data-motion=reduced] .sg-hover-links,.semctl-graph[data-motion=reduced] .sg-feedback,.semctl-graph[data-motion=reduced] .sg-indexing-state{transition:none;animation:none}
@media (prefers-reduced-motion:reduce){.semctl-graph[data-motion=full] .sg-target,.semctl-graph[data-motion=full] .sg-dot,.semctl-graph[data-motion=full] .sg-label,.semctl-graph[data-motion=full] .sg-routes,.semctl-graph[data-motion=full] .sg-territories,.semctl-graph[data-motion=full] .sg-route,.semctl-graph[data-motion=full] .sg-halo,.semctl-graph[data-motion=full] .sg-active .sg-halo,.semctl-graph[data-motion=full] .sg-hover-links,.semctl-graph[data-motion=full] .sg-feedback,.semctl-graph[data-motion=full] .sg-indexing-failed .sg-indexing-state{transition:none;animation:none}}
${geometry.join("")}`,
})]);
