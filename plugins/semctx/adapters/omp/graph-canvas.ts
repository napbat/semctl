/** Layout and native CSS must use the same aspect ratio or route endpoints miss their dots. */
export const GRAPH_CANVAS = {
	inline: { width: 560, height: 264 },
	small: { width: 560, height: 156 },
	medium: { width: 560, height: 216 },
	compact: { width: 280, height: 176 },
	overview: { width: 760, height: 320 },
} as const;
