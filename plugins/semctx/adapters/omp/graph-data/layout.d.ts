import type { GraphData } from "../graph";

export interface GraphCategory { key: string; label: string; type: string }
export function category(node: GraphData["nodes"][number]): GraphCategory;
export function exactActivity(node: GraphData["nodes"][number], targets: ReadonlySet<string>, codebaseId: string | null): boolean;

export type GraphRelationKind = "containment" | "association" | "import" | "reference" | "generic";
export interface GraphRelationStyle { readonly label: string; readonly dash: readonly number[]; }
export const relations: Readonly<Record<GraphRelationKind, GraphRelationStyle>>;
export function relationKind(edge: GraphData["edges"][number]): GraphRelationKind;
export function relationStyle(edge: GraphData["edges"][number]): GraphRelationStyle;
export function representativeLinks(data: GraphData): Array<GraphData["edges"][number] & { kind: GraphRelationKind; count: number; indices: number[]; index: number }>;

export interface GraphPosition {
	node: GraphData["nodes"][number];
	x: number;
	y: number;
	radius: number;
	category: string;
	degree: number;
}
export interface GraphLayout {
	positions: Map<string, GraphPosition>;
	width: number;
	height: number;
	bounds: { x: number; y: number; width: number; height: number };
}
export function layoutGraph(data: GraphData, availableWidth?: number): GraphLayout;
export function edgeGeometry(edges: GraphData["edges"], index: number, positions: Map<string, GraphPosition>): {
	from: [number, number];
	to: [number, number];
	control: number[];
	self: boolean;
	x: number;
	y: number;
} | null;
