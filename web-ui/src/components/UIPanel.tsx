import type { MemoryNode, MemoryLink } from '../types';

interface Props {
  viewMode: '2d' | '3d';
  setViewMode: (v: '2d' | '3d') => void;
  selectedNode: MemoryNode | null;
  selectedLink: MemoryLink | null;
  typeFilters: Record<string, boolean>;
  setTypeFilters: (f: Record<string, boolean>) => void;
  namespaceFilters: Record<string, boolean>;
  setNamespaceFilters: (f: Record<string, boolean>) => void;
}

export const UIPanel: React.FC<Props> = ({
  viewMode,
  setViewMode,
  selectedNode,
  selectedLink,
  typeFilters,
  setTypeFilters,
  namespaceFilters,
  setNamespaceFilters,
}) => {
  // Candy glassmorphism bevel effect class
  const panelGlassClass = "p-5 rounded-2xl bg-white/10 backdrop-blur-xl border border-white/20 shadow-[0_8px_32px_rgba(0,0,0,0.5),inset_0_1px_1px_rgba(255,255,255,0.4),inset_0_-1px_1px_rgba(255,255,255,0.1)]";

  return (
    <div className="absolute inset-0 pointer-events-none flex flex-col p-6 z-10 text-white font-sans">
      <div className="flex-1 flex gap-4 pointer-events-none">
        
        <div className="w-64 flex flex-col gap-4 pointer-events-auto max-h-[85vh] overflow-y-auto">
          <div className={panelGlassClass}>
            <h3 className="font-semibold mb-2 text-blue-300 drop-shadow-md text-lg">Namespaces</h3>
            <div className="flex flex-col gap-2 mb-6 max-h-48 overflow-y-auto pr-2 custom-scrollbar">
              {Object.entries(namespaceFilters).map(([ns, checked]) => (
                <label key={ns} className="flex items-center gap-3 cursor-pointer text-sm hover:text-blue-100 transition-colors">
                  <input 
                    type="checkbox" 
                    checked={checked} 
                    onChange={(e) => setNamespaceFilters({ ...namespaceFilters, [ns]: e.target.checked })} 
                    className="accent-blue-500 w-4 h-4 rounded-sm border-white/20 bg-white/10" 
                  />
                  <span className="truncate drop-shadow-sm font-medium" title={ns}>{ns}</span>
                </label>
              ))}
            </div>

            <h3 className="font-semibold mb-2 text-purple-300 drop-shadow-md text-lg">Memory Types</h3>
            <div className="flex flex-col gap-2">
              {Object.entries(typeFilters).map(([type, checked]) => (
                <label key={type} className="flex items-center gap-3 cursor-pointer text-sm hover:text-purple-100 transition-colors">
                  <input 
                    type="checkbox" 
                    checked={checked} 
                    onChange={(e) => setTypeFilters({ ...typeFilters, [type]: e.target.checked })} 
                    className="accent-purple-500 w-4 h-4 rounded-sm border-white/20 bg-white/10" 
                  />
                  <span className="drop-shadow-sm font-medium capitalize">{type}</span>
                </label>
              ))}
            </div>
          </div>
        </div>

        <div className="flex-1"></div>

        {(selectedNode || selectedLink) && (
          <div className="w-80 flex flex-col gap-4 pointer-events-auto max-h-[85vh]">
            <div className={panelGlassClass}>
              <h2 className="text-xl font-bold mb-4 drop-shadow-md">Details</h2>
              {selectedNode && (
                <div className="flex flex-col gap-2">
                  <div className="text-sm text-gray-300">Type: <span className="font-mono text-purple-300 drop-shadow-sm">{selectedNode.memory_type}</span></div>
                  {selectedNode.namespace && <div className="text-sm text-gray-300">Namespace: <span className="font-mono text-blue-300 drop-shadow-sm">{selectedNode.namespace}</span></div>}
                  <div className="mt-2 text-sm leading-relaxed whitespace-pre-wrap max-h-96 overflow-y-auto pr-2 custom-scrollbar bg-black/30 p-4 rounded-lg shadow-[inset_0_2px_4px_rgba(0,0,0,0.6)] border border-white/5">
                    {selectedNode.name}
                  </div>
                </div>
              )}
              {selectedLink && (
                <div className="flex flex-col gap-2">
                  <div className="text-sm text-gray-300">Type: <span className="font-mono text-white drop-shadow-sm">{selectedLink.type || 'Connection'}</span></div>
                  <div className="text-sm text-gray-300">From: <span className="font-mono text-white drop-shadow-sm">{selectedLink.source}</span></div>
                  <div className="text-sm text-gray-300">To: <span className="font-mono text-white drop-shadow-sm">{selectedLink.target}</span></div>
                </div>
              )}
            </div>
          </div>
        )}
      </div>

      <div className="absolute bottom-6 right-6 pointer-events-auto">
        <label className={`${panelGlassClass} !p-3 !px-5 flex items-center gap-3 cursor-pointer hover:bg-white/20 transition-all active:scale-95 group`}>
          <input 
            type="checkbox" 
            checked={viewMode === '3d'} 
            onChange={(e) => setViewMode(e.target.checked ? '3d' : '2d')} 
            className="accent-white w-5 h-5 rounded border-white/30 bg-white/10 cursor-pointer" 
          />
          <span className="font-bold text-lg tracking-wider drop-shadow-md group-hover:text-white text-gray-200">3D</span>
        </label>
      </div>
    </div>
  );
};
