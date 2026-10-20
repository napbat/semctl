import { boundedTooltip, TOOLTIP_LINE_CHARS } from "./tooltip-text";

const PROGRESS_LINES = 4;

/** Keep hover content brief. The owning status or diagnostics view retains the full error. */
export function progressTooltip(text: string, progress: number | undefined, diagnostic: string | undefined, detailsHint: string): string {
	const clean = (value: string) => value.replace(/[\u0000-\u0008\u000b-\u001f\u007f-\u009f]/g, "").replace(/\s+/g, " ").trim();
	const head = [clean(text), progress === undefined ? "" : `Reported progress: ${Math.floor(progress * 100)}%`].filter(Boolean);
	const lines = diagnostic?.split(/\r?\n/).map(clean).filter(Boolean) ?? [];
	const room = PROGRESS_LINES - head.length;
	const complete = lines.length <= room && lines.every(line => line.length <= TOOLTIP_LINE_CHARS);
	return boundedTooltip([...head, ...(complete ? lines : lines.slice(0, room - 1)), complete ? undefined : detailsHint], PROGRESS_LINES);
}
