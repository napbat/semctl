/** Tern shows `title` as a plain hover tooltip. Hover text stays short; the owning view keeps the full detail. */
export const TOOLTIP_LINES = 3;
export const TOOLTIP_LINE_CHARS = 88;

export type TooltipLine = string | false | null | undefined;
const CONTROL = /[\u0000-\u0008\u000b-\u001f\u007f-\u009f]/g;

/** At most `maxLines` unique non-empty lines; each long line ends with an ellipsis. Order the lines by importance. */
export function boundedTooltip(lines: readonly TooltipLine[], maxLines: number): string {
	const kept: string[] = [];
	for (const entry of lines) {
		if (!entry) continue;
		for (const raw of entry.split(/\r?\n/)) {
			const line = raw.replace(CONTROL, "").replace(/\s+/g, " ").trim();
			const short = line.length > TOOLTIP_LINE_CHARS ? `${line.slice(0, TOOLTIP_LINE_CHARS - 1).trimEnd()}…` : line;
			if (!short || kept.includes(short)) continue;
			kept.push(short);
			if (kept.length === maxLines) return kept.join("\n");
		}
	}
	return kept.join("\n");
}

export function compactTooltip(...lines: TooltipLine[]): string {
	return boundedTooltip(lines, TOOLTIP_LINES);
}
