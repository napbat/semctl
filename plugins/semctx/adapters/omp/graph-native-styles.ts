import type { NativeStylesheet } from "@oh-my-pi/pi-tui";

// A single surface-lifetime sheet, independent of graph identities, labels and history.
// Quarter-percent positions and half-degree angles keep rounding below the dot radius
// at the largest (760px) surface. Short curve segments bound angular rounding error.
export const POSITION_STEPS = 400;
export const ANGLE_STEPS = 720;
const position = (value: number) => Math.max(0, Math.min(POSITION_STEPS, Math.round(value * POSITION_STEPS)));
export function positionClasses(x: number, y: number, width: number, height: number): string {
	return `sg-x${position(x / width)} sg-y${position(y / height)}`;
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
	const length = Math.round(Math.hypot(to[0] - from[0], to[1] - from[1]) / width * POSITION_STEPS);
	return { x: position(from[0] / width) / 4, y: position(from[1] / height) / 4, length: Math.min(568, length) / 4, angle: angle / 2 };
}
export function segmentClasses(from: readonly number[], to: readonly number[], width: number, height: number): string {
	const segment = quantizedSegment(from, to, width, height);
	return `sg-x${segment.x * 4} sg-y${segment.y * 4} sg-l${segment.length * 4} sg-a${segment.angle * 2}`;
}
const geometry: string[] = [];
for (let step = 0; step <= POSITION_STEPS; step++) {
	geometry.push(`.semctl-graph .sg-x${step}{left:${step / 4}%}.semctl-graph .sg-y${step}{top:${step / 4}%}`);
}
for (let step = 0; step <= 568; step++) geometry.push(`.semctl-graph .sg-l${step}{width:${step / 4}%}`);
for (let step = 0; step < ANGLE_STEPS; step++) geometry.push(`.semctl-graph .sg-a${step}{transform:rotate(${step / 2}deg)}`);

export const GRAPH_STYLES: readonly NativeStylesheet[] = Object.freeze([Object.freeze({
	name: "semctl-native-graph",
	css: `
.semctl-graph{position:relative;width:100%;max-width:560px;color:var(--sg-edge);--sg-fg:var(--sf-p-text,var(--t1));--sg-edge:var(--sf-muted);--sg-accent:var(--sf-accent,var(--accent));--sg-bg:var(--panel);isolation:isolate}
.semctl-graph.sg-compact{max-width:280px}
.semctl-graph.sg-overview{max-width:760px}
.semctl-graph .sg-ratio{height:0;padding-bottom:47.142857%}
.semctl-graph.sg-small .sg-ratio{padding-bottom:27.857143%}
.semctl-graph.sg-medium .sg-ratio{padding-bottom:38.571429%}
.semctl-graph.sg-compact .sg-ratio{padding-bottom:62.857143%}
.semctl-graph.sg-overview .sg-ratio{padding-bottom:42.105263%}
.semctl-graph .sg-plane,.semctl-graph .sg-route,.semctl-graph .sg-node,.semctl-graph .sg-hover-links{position:absolute;inset:0;pointer-events:none}
.semctl-graph .sg-route{color:var(--sg-edge);opacity:.38}
.semctl-graph.sg-focused .sg-route{opacity:.12}
.semctl-graph .sg-route.sg-related{color:var(--sg-accent);opacity:.86}
.semctl-graph .sg-segment{position:absolute;height:6px;margin-top:-3px;transform-origin:0 3px;pointer-events:auto}
.semctl-graph .sg-stroke{position:absolute;top:3px;left:-1px;width:calc(100% + 2px);border-top:1px solid currentColor;pointer-events:none}
.semctl-graph .sg-import .sg-stroke{border-top-style:dashed}
.semctl-graph .sg-association .sg-stroke{border-top-style:dotted}
.semctl-graph .sg-arrow{position:absolute;right:0;top:1px;width:4px;height:4px;border-top:1px solid currentColor;border-right:1px solid currentColor;transform:rotate(45deg);pointer-events:none}
.semctl-graph .sg-target{position:absolute;width:24px;height:24px;transform:translate(-50%,-50%);padding:0;border:0;border-radius:50%;background:transparent;pointer-events:auto;z-index:2}
.semctl-graph .sg-target.sg-action{cursor:pointer}
.semctl-graph .sg-dot{position:absolute;left:50%;top:50%;width:7px;height:7px;transform:translate(-50%,-50%);border-radius:50%;background:currentColor;pointer-events:none}
.semctl-graph .sg-dot.sg-large{width:10px;height:10px}
.semctl-graph .sg-root .sg-dot{outline:1px solid var(--sg-accent);outline-offset:1px}
.semctl-graph .sg-halo{position:absolute;left:50%;top:50%;width:17px;height:17px;transform:translate(-50%,-50%);border:1px solid currentColor;border-radius:50%;opacity:0;pointer-events:none}
.semctl-graph .sg-active .sg-halo{opacity:.65}
.semctl-graph .sg-feedback{position:absolute;left:50%;top:50%;width:18px;height:18px;transform:translate(-50%,-50%);border:2px solid currentColor;border-radius:50%;opacity:.75;pointer-events:none}
.semctl-graph .sg-feedback.sg-returned{color:var(--sf-ok)}
.semctl-graph .sg-feedback.sg-failed{color:var(--sf-bad)}
.semctl-graph .sg-selected .sg-dot,.semctl-graph .sg-target:hover .sg-dot,.semctl-graph .sg-target:focus-visible .sg-dot{outline:2px solid var(--sg-accent);outline-offset:3px}
.semctl-graph .sg-muted .sg-dot{opacity:.4}
.semctl-graph .sg-label{position:absolute;left:21px;top:-4px;max-width:180px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis;font:450 11px/14px var(--sans,system-ui,sans-serif);color:var(--sg-fg);filter:drop-shadow(1px 0 1px var(--sg-bg)) drop-shadow(-1px 0 1px var(--sg-bg));pointer-events:none;text-align:left}
.semctl-graph .sg-label.sg-label-left{left:auto;right:21px;text-align:right}
.semctl-graph .sg-label.sg-label-below{top:19px}
.semctl-graph .sg-selected .sg-label{font-weight:600}
.semctl-graph .sg-hover-links{opacity:0;z-index:1}
.semctl-graph .sg-hover-links .sg-route{opacity:.9;color:var(--sg-accent)}
.semctl-graph .sg-hover-links .sg-segment{pointer-events:none}
.semctl-graph .sg-node:hover .sg-hover-links,.semctl-graph .sg-node:focus-within .sg-hover-links{opacity:1}
.semctl-graph[data-motion=full] .sg-halo{transition:opacity 220ms ease-out}
.semctl-graph[data-motion=full] .sg-hover-links{transition:opacity 120ms ease-out}
.semctl-graph[data-motion=full] .sg-dot{animation:semctl-graph-arrive 180ms ease-out 1}
.semctl-graph[data-motion=full] .sg-feedback{animation:semctl-graph-receipt 1100ms ease-out 1 both}
@keyframes semctl-graph-arrive{from{opacity:.25}to{opacity:1}}
@keyframes semctl-graph-receipt{from{opacity:.85;transform:translate(-50%,-50%) scale(.7)}to{opacity:0;transform:translate(-50%,-50%) scale(1.6)}}
@media (prefers-reduced-motion:reduce){.semctl-graph[data-motion=full] .sg-halo,.semctl-graph[data-motion=full] .sg-hover-links,.semctl-graph[data-motion=full] .sg-dot,.semctl-graph[data-motion=full] .sg-feedback{transition:none;animation:none}}
${geometry.join("")}`,
})]);
