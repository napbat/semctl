export const conceptSwatches: readonly { readonly light: string; readonly dark: string }[];
export function conceptPalette(key: string): { light: string; dark: string };
export function normalizeConceptPath(path: string | null | undefined): string | undefined;
