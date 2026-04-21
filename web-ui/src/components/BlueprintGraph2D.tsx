
import ForceGraph2D from 'react-force-graph-2d';
import type { GraphData, MemoryNode, MemoryLink } from '../types';

interface Props {
  data: GraphData;
  onNodeClick: (node: MemoryNode) => void;
  onLinkClick: (link: MemoryLink) => void;
}

export const BlueprintGraph2D: React.FC<Props> = ({ data, onNodeClick, onLinkClick }) => {
  return (
    <div className="absolute inset-0 bg-[#0a192f] z-0" style={{ backgroundImage: 'linear-gradient(rgba(255,255,255,0.05) 1px, transparent 1px), linear-gradient(90deg, rgba(255,255,255,0.05) 1px, transparent 1px)', backgroundSize: '20px 20px' }}>
      <ForceGraph2D
        graphData={data}
        backgroundColor="transparent"
        nodeCanvasObject={(node, ctx, globalScale) => {
          const mNode = node as MemoryNode;
          const label = mNode.name;
          const fontSize = 12 / globalScale;
          const size = Math.max(8, mNode.degree * 2);
          
          ctx.beginPath();
          ctx.arc(mNode.x || 0, mNode.y || 0, size, 0, 2 * Math.PI, false);
          ctx.fillStyle = mNode.namespace === 'global' ? '#64ffda' : '#e6f1ff';
          ctx.fill();
          ctx.lineWidth = 1;
          ctx.strokeStyle = '#0a192f';
          ctx.stroke();

          ctx.font = `${fontSize}px Sans-Serif`;
          ctx.textAlign = 'center';
          ctx.textBaseline = 'middle';
          ctx.fillStyle = '#a8b2d1';
          ctx.fillText(label, mNode.x || 0, (mNode.y || 0) + size + fontSize);
        }}
        linkColor={() => 'rgba(100, 255, 218, 0.4)'}
        linkWidth={1}
        onNodeClick={(n) => onNodeClick(n as MemoryNode)}
        onLinkClick={(l) => onLinkClick(l as MemoryLink)}
      />
    </div>
  );
};
