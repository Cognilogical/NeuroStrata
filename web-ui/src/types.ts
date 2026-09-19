export interface MemoryNode {
  id: string;
  name: string;
  memory_type: string;
  namespace?: string;
  agent_name?: string;
  degree: number;
  content?: string;
  location?: string;
  absolute_path?: string;
  /** d3-force adds positional state at runtime */
  x?: number;
  y?: number;
  z?: number;
  vx?: number;
  vy?: number;
  vz?: number;
  [key: string]: unknown;
}

export interface MemoryLink {
  type?: string;
  [key: string]: unknown;
}

export interface GraphData {
  nodes: MemoryNode[];
  links: MemoryLink[];
}
