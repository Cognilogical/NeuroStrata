export interface MemoryNode {
  id: string;
  name: string;
  memory_type: string;
  namespace?: string;
  agent_name?: string;
  degree: number;
  [key: string]: any;
}

export interface MemoryLink {
  source: string;
  target: string;
  type?: string;
  [key: string]: any;
}

export interface GraphData {
  nodes: MemoryNode[];
  links: MemoryLink[];
}
