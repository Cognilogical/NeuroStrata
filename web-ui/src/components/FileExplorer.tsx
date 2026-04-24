import React, { useState } from 'react';
import type { MemoryNode } from '../types';

interface Props {
  nodes: MemoryNode[];
  selectedNode: MemoryNode | null;
  onNodeSelect: (node: MemoryNode) => void;
}

export const FileExplorer: React.FC<Props> = ({ nodes, selectedNode, onNodeSelect }) => {
  const [expandedDirs, setExpandedDirs] = useState<Record<string, boolean>>({});

  const panelGlassClass = "p-5 rounded-2xl bg-black/60 backdrop-blur-xl border border-white/20 shadow-[0_8px_32px_rgba(0,0,0,0.5),inset_0_1px_1px_rgba(255,255,255,0.4),inset_0_-1px_1px_rgba(255,255,255,0.1)] text-white";

  // Filter only physical files/directories
  const fileNodes = nodes.filter(n => ['directory', 'markdown', 'code_ast'].includes(n.memory_type));
  
  // Sort by path (id) to keep directories and their files grouped naturally
  fileNodes.sort((a, b) => a.id.localeCompare(b.id));

  const getAncestorIds = (id: string) => {
    if (id === '.') return [];
    const ancestors = [];
    const parts = id.split('/');
    
    if (parts[0] === '.') {
      let current = '.';
      for (let i = 1; i < parts.length - 1; i++) {
        current += '/' + parts[i];
        ancestors.push(current);
      }
    } else {
      let current = '';
      for (let i = 0; i < parts.length - 1; i++) {
        current += (i === 0 ? '' : '/') + parts[i];
        ancestors.push(current);
      }
    }
    return ancestors;
  };

  const handleNodeClick = (node: MemoryNode) => {
    if (node.memory_type === 'directory') {
      setExpandedDirs(prev => ({
        ...prev,
        [node.id]: !prev[node.id]
      }));
    } else {
      onNodeSelect(node);
    }
  };

  return (
    <div className={`w-80 h-full flex flex-col ${panelGlassClass} pointer-events-auto overflow-hidden`}>
      <h2 className="text-xl font-bold mb-4 drop-shadow-md text-blue-300">File Explorer</h2>
      <div className="flex-1 overflow-y-auto custom-scrollbar pr-2 flex flex-col gap-1 text-sm">
        {fileNodes.length === 0 ? (
          <div className="text-gray-400 italic">No physical files found in memory graph.</div>
        ) : (
          fileNodes.map(node => {
            const ancestors = getAncestorIds(node.id);
            const isVisible = ancestors.every(anc => expandedDirs[anc]);
            
            if (!isVisible) return null;

            const isSelected = selectedNode?.id === node.id;
            
            // Calculate indentation based on path depth
            const parts = node.id.replace(/^\.\//, '').split('/');
            const depth = parts.length - 1;
            const displayName = parts[parts.length - 1];
            const isDirectory = node.memory_type === 'directory';
            const isExpanded = expandedDirs[node.id];

            return (
              <button
                key={node.id}
                onClick={() => handleNodeClick(node)}
                style={{ paddingLeft: `${depth * 12 + 8}px` }}
                className={`text-left py-1.5 min-h-[32px] w-full rounded transition-all flex items-center ${isSelected ? 'bg-blue-500/30 text-blue-100 font-bold border-l-2 border-blue-500' : 'hover:bg-white/10 text-gray-300 border-l-2 border-transparent'}`}
                title={node.id}
              >
                <span className="mr-2 opacity-70 flex-shrink-0 text-base leading-none w-4 text-center inline-block">
                  {isDirectory ? (isExpanded ? '📂' : '📁') : node.memory_type === 'markdown' ? '📝' : '📄'}
                </span>
                <span className="truncate leading-tight mt-0.5">{displayName}</span>
              </button>
            );
          })
        )}
      </div>
    </div>
  );
};
