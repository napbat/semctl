import type { DescribeContext, NativeNode } from "@oh-my-pi/pi-tui";
import type { Hit } from "./inspector";
import { SCORE_WIDTH_CH, scoreTitle, verificationTitle } from "./graph-data/matches.js";

const scoreCache = new WeakMap<Hit, { rank: number; strength: number | null; mode: number; action: string; node: NativeNode }>();
const verificationCache = new WeakMap<Hit, { mode: number; action: string; node: NativeNode }>();
const states = {
	bound: { name: "check", glyph: "✓", tone: "success" as const },
	"changed-local": { name: "warn", glyph: "!", tone: "warning" as const },
	"range-mismatch": { name: "x-circle", glyph: "!", tone: "error" as const },
	unverified: { name: "info", glyph: "i", tone: "muted" as const },
	"not-probed": { name: "info", glyph: "i", tone: "muted" as const },
};

/** One footprint for every scored hit. A missing score has no indicator; an unknown scale keeps its rail. */
export function matchScoreIndicator(hit: Hit, rank: number, strength: number | null, cx: DescribeContext, action: string): NativeNode | undefined {
	if (typeof hit.score !== "number" || !Number.isFinite(hit.score)) return undefined;
	const mode = Number(cx.supports("meter"));
	const cached = scoreCache.get(hit);
	if (cached && cached.rank === rank && cached.strength === strength && cached.mode === mode && cached.action === action) return cached.node;
	const title = scoreTitle(hit.score, strength, rank);
	let node: NativeNode;
	if (mode) node = { k: "meter", key: "relevance", p: { value: strength, style: "bar", size: "sm",
		tone: strength === null ? "muted" : "accent", basis: "content", grow: 0, shrink: 0, title, aria: title, actions: { click: action } } };
	else node = { k: "text", key: "relevance", p: { text: strength === null ? "░".repeat(SCORE_WIDTH_CH) : "▰".repeat(Math.round(strength * SCORE_WIDTH_CH)) + "▱".repeat(SCORE_WIDTH_CH - Math.round(strength * SCORE_WIDTH_CH)),
		tone: strength === null ? "muted" : "accent", wrap: "none", shrink: 0, title, aria: title, actions: { click: action } } };
	scoreCache.set(hit, { rank, strength, mode, action, node });
	return node;
}

/** Verification prose belongs only to this hoverable, accessible diagnostic. */
export function matchVerificationIcon(hit: Hit, cx: DescribeContext, action: string): NativeNode {
	const mode = Number(cx.supports("icon"));
	const cached = verificationCache.get(hit);
	if (cached?.mode === mode && cached.action === action) return cached.node;
	const status = states[hit.binding.state], title = verificationTitle(hit.binding);
	let node: NativeNode;
	if (mode) node = { k: "icon", key: "verification", p: { name: status.name, tone: status.tone, title, aria: title, shrink: 0, actions: { click: action } } };
	else node = { k: "text", key: "verification", p: { text: status.glyph, tone: status.tone, title, aria: title, shrink: 0, wrap: "none", actions: { click: action } } };
	verificationCache.set(hit, { mode, action, node });
	return node;
}
