import type { GraphData } from "../graph";
/** Lexical identifier weights for drawing placement, not the server's embeddings. L2-normalized; at most 48 terms; order is weight desc, then term asc. */
export type TermVector = ReadonlyMap<string, number>;
export interface SemanticNeighbour { a: string; b: string; similarity: number }
export interface SemanticFamily { key: string; label: string; terms: string[]; members: string[]; cohesion: number }
export function identifierTerms(text: string): string[];
/** Aggregate nodes (overview groups) carry a precomputed centroid. `graphEmbedding` uses it as-is. */
export function bindTermVector(node: object, vector: TermVector): void;
/** `informative`: at least one term is shared by two nodes and not every non-empty vector is the same. */
export function graphEmbedding(data: Pick<GraphData, "nodes" | "edges">): { vectors: Map<string, TermVector>; informative: boolean };
export function cosine(a?: TermVector, b?: TermVector): number;
export function centroid(vectors: Iterable<TermVector | undefined>): TermVector;
/**
 * Classical MDS on a symmetric similarity, read once per unordered ID pair and once per ID with itself.
 * The squared map distance follows S(a, a) + S(b, b) − 2·S(a, b); the RMS radius is 1.
 * S need not be positive semidefinite: the axes follow the two largest eigenvalues, not the largest magnitudes.
 * Undefined for fewer than 2 or more than 512 IDs, no pair with positive similarity, or no spread. Otherwise one point per unique ID.
 */
export function similarityMap(ids: readonly string[], similarity: (a: string, b: string) => number): Map<string, [number, number]> | undefined;
/** `similarityMap` with cosine similarity as S, so the map distance is the chord distance between the vectors. */
export function semanticMap(ids: readonly string[], vectors: ReadonlyMap<string, TermVector>): Map<string, [number, number]> | undefined;
export function semanticNeighbours(ids: readonly string[], vectors: ReadonlyMap<string, TermVector>, options?: { k?: number; minimum?: number }): SemanticNeighbour[];
export function semanticFamilies(ids: readonly string[], vectors: ReadonlyMap<string, TermVector>, options?: { limit?: number; threshold?: number }): SemanticFamily[];
