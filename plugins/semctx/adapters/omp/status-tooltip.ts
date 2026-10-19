/** Keep hover content brief. The owning status or diagnostics view retains the full error. */
export function progressTooltip(text: string, progress: number | undefined, diagnostic: string | undefined, detailsHint: string): string {
	const clean = (value: string) => value.replace(/[\u0000-\u0008\u000b-\u001f\u007f-\u009f]/g, "").trim();
	const lines = diagnostic?.split(/\r?\n/).map(clean).filter(Boolean) ?? [];
	const preview = lines.slice(0, 2).map(line => line.length > 180 ? `${line.slice(0, 179)}…` : line);
	const shortened = lines.length > preview.length || preview.some((line, index) => line !== lines[index]);
	return [clean(text), progress === undefined ? undefined : `Reported progress: ${Math.floor(progress * 100)}%`,
		...preview, shortened ? detailsHint : undefined].filter(Boolean).join("\n");
}
