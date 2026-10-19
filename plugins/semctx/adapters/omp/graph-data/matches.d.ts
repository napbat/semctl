export function strengths(rows: readonly { score: number | null }[]): (number | null)[];
export const SCORE_WIDTH_PX: number;
export const SCORE_WIDTH_CH: number;
export const SCORE_HEIGHT_PX: number;
export function scoreTitle(score: number, strength: number | null, index: number): string;
export function verificationTitle(binding: { state: string; reason: string | null }): string;
