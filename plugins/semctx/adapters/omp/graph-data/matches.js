// Scale kind of each row's latest `strengths` result. Renderers receive only (hit, strength), so the
// tooltip wording is looked up by row identity; the most recent `strengths` call for a row wins.
const rowScales = new WeakMap();

/** Query-relative scale kind: "ratio" for nonnegative sets, "range" for signed sets, null when unscalable. */
export function scoreScale(rows) {
	if (rows.some(h => typeof h.score !== "number" || !Number.isFinite(h.score))) return null;
	const scores = rows.map(h => h.score), max = Math.max(...scores), min = Math.min(...scores);
	if (min >= 0) return max > 0 ? "ratio" : null;
	return max - min > 1e-12 ? "range" : null;
}

/** Query-relative visual strength only; never a validity/confidence score. */
export function strengths(rows) {
	const scale = scoreScale(rows);
	const scores = rows.map(h => h.score), max = Math.max(...scores), min = Math.min(...scores);
	return rows.map(h => {
		if (scale === null) { rowScales.delete(h); return null; }
		rowScales.set(h, scale);
		return scale === "ratio" ? h.score / max : 0.1 + 0.9 * (h.score - min) / (max - min);
	});
}

/** Scale kind recorded for `row` by its latest `strengths` call, or null. */
export function strengthScale(row) {
	return rowScales.get(row) ?? null;
}

// Native small meters have a 40 px minimum rail. Keep native and image bars aligned.
export const SCORE_WIDTH_PX = 40;
export const SCORE_WIDTH_CH = 6;
export const SCORE_HEIGHT_PX = 4;
export function scoreTitle(score, strength, index, scale = "ratio") {
	const basis = strength === null ? "Relative strength unavailable"
		: scale === "range" ? "Bar: position between lowest and highest result scores" : "Bar: relative to highest result score";
	return `Rank ${index + 1} · Score ${score}\n${basis} · not confidence.`;
}
const bindingLabels = { bound: "Source bytes verified", "changed-local": "Local changes", "range-mismatch": "Range mismatch", unverified: "Unverified source", "not-probed": "Source not checked" };
export function verificationTitle(binding) {
	return `${bindingLabels[binding.state] ?? binding.state}${binding.reason ? `\n${binding.reason}` : ""}${binding.state === "bound" ? "\nSymbol and focus identity not checked." : ""}`;
}
