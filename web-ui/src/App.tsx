import { useState, useMemo } from 'react';
import { GalaxyGraph3D } from './components/GalaxyGraph3D';
import { BlueprintGraph2D } from './components/BlueprintGraph2D';
import { UIPanel } from './components/UIPanel';
import type { GraphData, MemoryNode, MemoryLink } from './types';

// Mock data for initial render
const mockData: GraphData = {
  nodes: [
    { id: '1', name: 'User Authentication', memory_type: 'rule', namespace: 'global', degree: 4 },
    { id: '2', name: 'Frontend Architecture', memory_type: 'preference', namespace: 'frontend', degree: 3 },
    { id: '3', name: 'API Design', memory_type: 'context', namespace: 'backend', degree: 2 },
    { id: '4', name: 'Database Setup', memory_type: 'bootstrap', namespace: 'database', degree: 5 },
    { id: '5', name: 'DevOps Configuration', memory_type: 'persona', namespace: 'global', degree: 1 },
  ],
  links: [
    { source: '1', target: '3', type: 'related_to' },
    { source: '2', target: '3', type: 'depends_on' },
    { source: '3', target: '4', type: 'related_to' },
    { source: '1', target: '4', type: 'depends_on' },
  ]
};

function App() {
  const [viewMode, setViewMode] = useState<'2d' | '3d'>('3d');
  const [selectedNode, setSelectedNode] = useState<MemoryNode | null>(null);
  const [selectedLink, setSelectedLink] = useState<MemoryLink | null>(null);
  const [showGlobal, setShowGlobal] = useState(true);
  
  const [filters, setFilters] = useState<Record<string, boolean>>({
    rule: true,
    preference: true,
    bootstrap: true,
    persona: true,
    context: true,
  });

  const filteredData = useMemo(() => {
    const nodes = mockData.nodes.filter(n => {
      if (!showGlobal && n.namespace === 'global') return false;
      return filters[n.memory_type] !== false;
    });
    
    const nodeIds = new Set(nodes.map(n => n.id));
    const links = mockData.links.filter(l => 
      nodeIds.has(typeof l.source === 'object' ? (l.source as any).id : l.source) && 
      nodeIds.has(typeof l.target === 'object' ? (l.target as any).id : l.target)
    );
    
    return { nodes, links };
  }, [filters, showGlobal]);

  const handleNodeClick = (node: MemoryNode) => {
    setSelectedNode(node);
    setSelectedLink(null);
  };

  const handleLinkClick = (link: MemoryLink) => {
    setSelectedLink(link);
    setSelectedNode(null);
  };

  return (
    <div className="relative w-full h-screen overflow-hidden">
      {viewMode === '3d' ? (
        <GalaxyGraph3D data={filteredData} onNodeClick={handleNodeClick} onLinkClick={handleLinkClick} />
      ) : (
        <BlueprintGraph2D data={filteredData} onNodeClick={handleNodeClick} onLinkClick={handleLinkClick} />
      )}
      
      <UIPanel 
        viewMode={viewMode}
        setViewMode={setViewMode}
        selectedNode={selectedNode}
        selectedLink={selectedLink}
        filters={filters}
        setFilters={setFilters}
        showGlobal={showGlobal}
        setShowGlobal={setShowGlobal}
      />
    </div>
  );
}

export default App;
