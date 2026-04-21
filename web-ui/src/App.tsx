import { useState, useMemo, useEffect } from 'react';
import { GalaxyGraph3D } from './components/GalaxyGraph3D';
import { BlueprintGraph2D } from './components/BlueprintGraph2D';
import { UIPanel } from './components/UIPanel';
import type { GraphData, MemoryNode, MemoryLink } from './types';

function App() {
  const [graphData, setGraphData] = useState<GraphData>({ nodes: [], links: [] });
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
    code_ast: true
  });

  useEffect(() => {
    // In production/desktop app this would load from .NeuroStrata/graph/graph.json
    // For local dev with Vite, we might need a symlink or custom server setup, but we use an absolute or relative path that points there.
    fetch('/graph.json')
      .then(res => res.json())
      .then(data => setGraphData(data))
      .catch(e => console.error("Failed to load graph data", e));
  }, []);

  const filteredData = useMemo(() => {
    const nodes = graphData.nodes.filter(n => {
      if (!showGlobal && n.namespace === 'global') return false;
      return filters[n.memory_type] !== false;
    });
    
    const nodeIds = new Set(nodes.map(n => n.id));
    const links = graphData.links.filter(l => 
      nodeIds.has(typeof l.source === 'object' ? (l.source as any).id : l.source) && 
      nodeIds.has(typeof l.target === 'object' ? (l.target as any).id : l.target)
    );
    
    return { nodes, links };
  }, [graphData, filters, showGlobal]);

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

