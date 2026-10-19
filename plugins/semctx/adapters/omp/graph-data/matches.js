/** Query-relative visual strength only; never a validity/confidence score. */
export function strengths(rows) {
	if (rows.some(h => h.score === null || !Number.isFinite(h.score) || h.score < 0)) return rows.map(() => null);
	const max = Math.max(0, ...rows.map(h => h.score));
	return rows.map(h => max > 0 ? h.score / max : null);
}

// Native small meters have a 40 px minimum rail. Keep native and image bars aligned.
export const SCORE_WIDTH_PX = 40;
export const SCORE_WIDTH_CH = 6;
export const SCORE_HEIGHT_PX = 4;
export function scoreTitle(score, strength, index) {
	return `Rank ${index + 1} · Score ${score}\n${strength === null ? "Relative strength unavailable" : "Relative to this result's highest score"}; not confidence.`;
}
const bindingLabels = { bound: "Source bytes verified", "changed-local": "Local changes", "range-mismatch": "Range mismatch", unverified: "Unverified source", "not-probed": "Source not checked" };
export function verificationTitle(binding) {
	return `${bindingLabels[binding.state] ?? binding.state}${binding.reason ? `: ${binding.reason}` : ""}\nBinding checks bytes and coordinates, not semantic symbol or focus identity.`;
}
