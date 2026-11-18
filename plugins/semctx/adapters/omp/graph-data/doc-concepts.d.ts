import type { GraphData } from "../graph";
import type { TermVector } from "./embedding.js";
/** Repository-relative POSIX path of a catalog Markdown file and its text. */
export interface DocSource { path: string; text: string }
/** Key "family:<doc folder>"; label is the humanized last folder segment. */
export interface DocConceptFamily { key: string; label: string }
/** "doc": the source doc itself. "mention": explicit doc evidence. "reference": reference/import edges or a same-stem sibling. "folder": folder plurality. */
export type DocConceptVia = "doc" | "mention" | "reference" | "folder";
export interface DocConcept {
	/** "doc:<source path>" */
	key: string;
	/** Short display name, at most 28 characters, unique in the result. */
	label: string;
	/** Full document title. */
	title: string;
	/** Repository-relative path of the doc that names the concept. */
	source: string;
	/** Only when at least 2 concepts share the doc folder. */
	family?: DocConceptFamily;
	/** L2-normalized title and heading terms (embedding.js tokenizer). */
	terms: TermVector;
	/** Assigned node IDs (files and directories), sorted. */
	members: string[];
}
export interface DocConceptResult {
	/** Sorted by member count desc, then key. */
	concepts: DocConcept[];
	/** Node ID -> concept. Project nodes, the checkout root and unsupported files have no entry. */
	assignments: Map<string, { key: string; via: DocConceptVia }>;
	/** `files` counts catalog file nodes; `assigned` counts the assigned ones. `hubs` lists index docs that point at too much code to name a concept. */
	stats: { docs: number; hubs: string[]; files: number; assigned: number };
}
/** 256 files, 1 MiB per file, 16 MiB total. `docConcepts` applies the same bounds in path order. */
export const DOC_SOURCE_LIMITS: Readonly<{ files: number; fileBytes: number; totalBytes: number }>;
/** .md, .markdown or .mdx, case-insensitive. */
export function isDocPath(path: string): boolean;
/** Pure and deterministic: the result does not depend on node, edge or doc order. Docs whose path is not a catalog file node are ignored. */
export function docConcepts(data: Pick<GraphData, "nodes" | "edges">, docs: readonly DocSource[]): DocConceptResult;
/** WeakMap binding, like `bindTermVector`. `undefined` removes the binding. */
export function bindConceptFamily(node: object, family: DocConceptFamily | undefined): void;
export function conceptFamily(node: object): DocConceptFamily | undefined;
