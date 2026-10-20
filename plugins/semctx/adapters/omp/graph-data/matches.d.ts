export type ScoreScale = "ratio" | "range";
export function scoreScale(rows: readonly { score: number | null }[]): ScoreScale | null;
export function strengths(rows: readonly { score: number | null }[]): (number | null)[];
export function strengthScale(row: object): ScoreScale | null;
export const SCORE_WIDTH_PX: number;
export const SCORE_WIDTH_CH: number;
export const SCORE_HEIGHT_PX: number;
export function scoreTitle(score: number, strength: number | null, index: number, scale?: ScoreScale | null): string;
export function verificationTitle(binding: { state: string; reason: string | null }): string;
