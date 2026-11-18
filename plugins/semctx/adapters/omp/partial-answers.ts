const COMPLETE: readonly string[] = [];

/** CLI and MCP carry at most eight distinct partial answers. Fall back to raw output for unexpected metadata. */
export function readWarnings(value: unknown): readonly string[] | undefined {
	if (value === undefined) return COMPLETE;
	if (!Array.isArray(value) || value.length > 8) return undefined;
	for (const warning of value) {
		if (typeof warning !== "string" || !warning.length || warning.length > 4096 || Buffer.byteLength(warning, "utf8") > 4096) return undefined;
	}
	return value;
}
