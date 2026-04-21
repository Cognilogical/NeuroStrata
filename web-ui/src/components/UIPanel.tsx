
import type { MemoryNode, MemoryLink } from '../types';

interface Props {
  viewMode: '2d' | '3d';
  setViewMode: (v: '2d' | '3d') => void;
  selectedNode: MemoryNode | null;
  selectedLink: MemoryLink | null;
  filters: Record<string, boolean>;
  setFilters: (f: Record<string, boolean>) => void;
  showGlobal: boolean;
  setShowGlobal: (s: boolean) => void;
}

export const UIPanel: React.FC<Props> = ({
  viewMode,
  setViewMode,
  selectedNode,
  selectedLink,
  filters,
  setFilters,
  showGlobal,
  setShowGlobal,
}) => {
  return (
    <div className="absolute inset-0 pointer-events-none flex p-6 z-10 text-white font-sans">
      <div className="w-64 flex flex-col gap-4 pointer-events-auto">
        <div className="p-4 rounded-xl bg-white/10 backdrop-blur-md border border-white/20 shadow-lg">
          <h2 className="text-xl font-bold mb-4">View Controls</h2>
          <div className="flex gap-2 mb-4 bg-black/20 p-1 rounded-lg">
            <button 
              className={`flex-1 py-1 rounded-md transition-all ${viewMode === '3d' ? 'bg-white/20 shadow' : 'hover:bg-white/5'}`}
              onClick={() => setViewMode('3d')}
            >3D Galaxy</button>
            <button 
              className={`flex-1 py-1 rounded-md transition-all ${viewMode === '2d' ? 'bg-white/20 shadow' : 'hover:bg-white/5'}`}
              onClick={() => setViewMode('2d')}
            >2D Blueprint</button>
          </div>
          
          <h3 className="font-semibold mb-2">Filters</h3>
          <div className="flex flex-col gap-2">
            <label className="flex items-center gap-2 cursor-pointer">
              <input type="checkbox" checked={showGlobal} onChange={(e) => setShowGlobal(e.target.checked)} className="accent-blue-500" />
              Show Global Memories
            </label>
            {Object.entries(filters).map(([type, checked]) => (
              <label key={type} className="flex items-center gap-2 cursor-pointer">
                <input type="checkbox" checked={checked} onChange={(e) => setFilters({ ...filters, [type]: e.target.checked })} className="accent-blue-500" />
                {type}
              </label>
            ))}
          </div>
        </div>
      </div>

      <div className="flex-1"></div>

      {(selectedNode || selectedLink) && (
        <div className="w-80 flex flex-col gap-4 pointer-events-auto">
          <div className="p-4 rounded-xl bg-white/10 backdrop-blur-md border border-white/20 shadow-lg">
            <h2 className="text-xl font-bold mb-4">Details</h2>
            {selectedNode && (
              <div className="flex flex-col gap-2">
                <div className="text-sm text-gray-300">Type: <span className="font-mono text-white">{selectedNode.memory_type}</span></div>
                {selectedNode.namespace && <div className="text-sm text-gray-300">Namespace: <span className="font-mono text-white">{selectedNode.namespace}</span></div>}
                <p className="mt-2 text-sm leading-relaxed whitespace-pre-wrap">
                  {selectedNode.name}
                </p>
              </div>
            )}
            {selectedLink && (
              <div className="flex flex-col gap-2">
                <div className="text-sm text-gray-300">Type: <span className="font-mono text-white">{selectedLink.type || 'Connection'}</span></div>
                <div className="text-sm text-gray-300">From: <span className="font-mono text-white">{selectedLink.source}</span></div>
                <div className="text-sm text-gray-300">To: <span className="font-mono text-white">{selectedLink.target}</span></div>
              </div>
            )}
          </div>
        </div>
      )}
    </div>
  );
};
