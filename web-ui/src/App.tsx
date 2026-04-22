import { useState, useMemo, useEffect } from 'react';
import { GalaxyGraph3D } from './components/GalaxyGraph3D';
import { BlueprintGraph2D } from './components/BlueprintGraph2D';
import { UIPanel } from './components/UIPanel';
import { FileExplorer } from './components/FileExplorer';
import type { GraphData, MemoryNode, MemoryLink } from './types';

function App() {
  const [graphData, setGraphData] = useState<GraphData>({ nodes: [], links: [] });
  const [viewMode, setViewMode] = useState<'2d' | '3d'>('3d');
  const [selectedNode, setSelectedNode] = useState<MemoryNode | null>(null);
  const [selectedLink, setSelectedLink] = useState<MemoryLink | null>(null);
  
  const [namespaceFilters, setNamespaceFilters] = useState<Record<string, boolean>>({});
  const [typeFilters, setTypeFilters] = useState<Record<string, boolean>>({});

  useEffect(() => {
    fetch('/graph.json')
      .then(res => res.json())
      .then((data: GraphData) => {
        setGraphData(data);
        
        // Dynamically initialize namespace filters based on data
        const uniqueNamespaces = Array.from(new Set(data.nodes.map(n => n.namespace).filter(Boolean))) as string[];
        setNamespaceFilters(prev => {
          const next = { ...prev };
          uniqueNamespaces.forEach(ns => {
            if (next[ns] === undefined) next[ns] = true;
          });
          return next;
        });

        // Dynamically initialize type filters
        const uniqueTypes = Array.from(new Set(data.nodes.map(n => n.memory_type).filter(Boolean))) as string[];
        setTypeFilters(prev => {
          const next = { ...prev };
          uniqueTypes.forEach(t => {
            // Default to true, except for physical files which we want to hide from the graph by default
            if (next[t] === undefined) {
              next[t] = !['directory', 'markdown', 'code_ast'].includes(t);
            }
          });
          return next;
        });
      })
      .catch(e => console.error("Failed to load graph data", e));
  }, []);

  const filteredData = useMemo(() => {
    const nodes = graphData.nodes.filter(n => {
      // If namespace filter exists and is false, hide the node
      if (n.namespace && namespaceFilters[n.namespace] === false) return false;
      // If type filter is false, hide the node
      if (typeFilters[n.memory_type] === false) return false;
      return true;
    });
    
    const nodeIds = new Set(nodes.map(n => n.id));
    const links = graphData.links.filter(l => 
      nodeIds.has(typeof l.source === 'object' ? (l.source as any).id : l.source) && 
      nodeIds.has(typeof l.target === 'object' ? (l.target as any).id : l.target)
    );
    
    return { nodes, links };
  }, [graphData, typeFilters, namespaceFilters]);

  const handleNodeClick = (node: MemoryNode) => {
    setSelectedNode(node);
    setSelectedLink(null);
  };

  const handleLinkClick = (link: MemoryLink) => {
    setSelectedLink(link);
    setSelectedNode(null);
  };

  return (
    <div className="relative w-full h-screen overflow-hidden flex">
      <div className="absolute inset-0 z-0">
        {viewMode === '3d' ? (
          <GalaxyGraph3D data={filteredData} selectedNode={selectedNode} onNodeClick={handleNodeClick} onLinkClick={handleLinkClick} />
        ) : (
          <BlueprintGraph2D data={filteredData} selectedNode={selectedNode} onNodeClick={handleNodeClick} onLinkClick={handleLinkClick} />
        )}
      </div>

      {/* Left Pane: File Explorer */}
      <div className="z-10 p-6 pointer-events-none h-full">
        <FileExplorer 
          nodes={graphData.nodes} 
          selectedNode={selectedNode} 
          onNodeSelect={handleNodeClick} 
        />
      </div>

      {/* Right Pane: Filters and Details */}
      <UIPanel 
        viewMode={viewMode}
        setViewMode={setViewMode}
        selectedNode={selectedNode}
        selectedLink={selectedLink}
        typeFilters={typeFilters}
        setTypeFilters={setTypeFilters}
        namespaceFilters={namespaceFilters}
        setNamespaceFilters={setNamespaceFilters}
      />
    </div>
  );
}

export default App;
