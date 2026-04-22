import { useRef, useMemo, useEffect } from 'react';
import ForceGraph3D from 'react-force-graph-3d';
import * as THREE from 'three';
import type { GraphData, MemoryNode, MemoryLink } from '../types';

interface Props {
  data: GraphData;
  selectedNode: MemoryNode | null;
  onNodeClick: (node: MemoryNode) => void;
  onLinkClick: (link: MemoryLink) => void;
}

const colorMap: Record<string, string> = {
  rule: '#ff4b4b',
  preference: '#00ffcc',
  bootstrap: '#ffaa00',
  persona: '#cc00ff',
  context: '#4b9dff',
  directory: '#555555',
  markdown: '#ffffff',
  code_ast: '#ffcc00',
};

const getGlowTexture = () => {
  const canvas = document.createElement('canvas');
  canvas.width = 64;
  canvas.height = 64;
  const ctx = canvas.getContext('2d');
  if (ctx) {
    const gradient = ctx.createRadialGradient(32, 32, 0, 32, 32, 32);
    // Core bright glow for the document nodes
    gradient.addColorStop(0, 'rgba(255, 255, 255, 1)');
    gradient.addColorStop(0.1, 'rgba(255, 255, 255, 0.8)');
    gradient.addColorStop(0.4, 'rgba(255, 255, 255, 0.2)');
    gradient.addColorStop(1, 'rgba(0, 0, 0, 0)');
    ctx.fillStyle = gradient;
    ctx.fillRect(0, 0, 64, 64);
  }
  return new THREE.CanvasTexture(canvas);
};

export const GalaxyGraph3D = ({ data, selectedNode, onNodeClick, onLinkClick }: Props) => {
  const fgRef = useRef<any>(null);
  
  useEffect(() => {
    if (selectedNode && fgRef.current) {
      // Find the actual node object in the graph with coordinates
      const graphNode = fgRef.current.graphData().nodes.find((n: any) => n.id === selectedNode.id);
      
      if (graphNode && typeof graphNode.x === 'number' && !Number.isNaN(graphNode.x)) {
        // Safe check for distance calculation
        const distance = Math.hypot(graphNode.x, graphNode.y, graphNode.z);
        const distRatio = 1 + 60 / (distance || 1); // fallback to 1 if distance is 0
        
        // Define new camera position relative to node
        const newPos = distance > 0
          ? { x: graphNode.x * distRatio, y: graphNode.y * distRatio, z: graphNode.z * distRatio }
          : { x: 0, y: 0, z: 100 }; // fallback
        
        // Explicitly extract primitive numbers for lookAt to avoid proxy object mutations
        const lookAtPos = { x: graphNode.x, y: graphNode.y, z: graphNode.z };
        
        fgRef.current.cameraPosition(
          newPos, // new position
          lookAtPos, // lookAt
          1500  // ms transition duration
        );
      }
    }
  }, [selectedNode, data]);

  const { nodeMaterials, defaultNodeMaterial, highlightMaterial } = useMemo(() => {
    const nodeTex = getGlowTexture();
    
    const nMats: Record<string, THREE.SpriteMaterial> = {};
    for (const [key, color] of Object.entries(colorMap)) {
      nMats[key] = new THREE.SpriteMaterial({
        map: nodeTex,
        color: color,
        transparent: true,
        blending: THREE.AdditiveBlending,
        depthWrite: false
      });
    }
    
    const defNodeMat = new THREE.SpriteMaterial({
      map: nodeTex,
      color: '#888888',
      transparent: true,
      blending: THREE.AdditiveBlending,
      depthWrite: false
    });

    const hlMat = new THREE.SpriteMaterial({
      map: nodeTex,
      color: '#ffffff',
      transparent: true,
      blending: THREE.AdditiveBlending,
      depthWrite: false
    });

    return { nodeMaterials: nMats, defaultNodeMaterial: defNodeMat, highlightMaterial: hlMat };
  }, []);

  return (
    <div className="absolute inset-0 bg-black z-0">
      <ForceGraph3D
        ref={fgRef}
        graphData={data}
        backgroundColor="#000000"
        nodeThreeObject={(node: any) => {
          if (!node) return new THREE.Object3D();
          const mNode = node as MemoryNode;
          const isSelected = selectedNode && selectedNode.id === mNode.id;
          const size = Math.max(16, (mNode.degree || 1) * 3) * (isSelected ? 1.5 : 1);
          
          const material = isSelected ? highlightMaterial : (nodeMaterials[mNode.memory_type] || defaultNodeMaterial);
          const sprite = new THREE.Sprite(material);
          sprite.scale.set(size, size, 1);
          return sprite;
        }}
        linkColor={(link: any) => {
          const isSourceSelected = selectedNode && (typeof link.source === 'object' ? link.source.id === selectedNode.id : link.source === selectedNode.id);
          const isTargetSelected = selectedNode && (typeof link.target === 'object' ? link.target.id === selectedNode.id : link.target === selectedNode.id);
          const highlight = isSourceSelected || isTargetSelected;
          
          if (highlight) return 'rgba(255, 255, 255, 0.9)';
          if (link.type === 'contains') return 'rgba(100, 150, 255, 0.4)';
          if (link.type === 'links_to') return 'rgba(255, 100, 255, 0.6)';
          return 'rgba(255, 255, 255, 0.2)';
        }}
        linkWidth={(link: any) => {
          const isSourceSelected = selectedNode && (typeof link.source === 'object' ? link.source.id === selectedNode.id : link.source === selectedNode.id);
          const isTargetSelected = selectedNode && (typeof link.target === 'object' ? link.target.id === selectedNode.id : link.target === selectedNode.id);
          if (isSourceSelected || isTargetSelected) return 6;
          
          return link.type === 'links_to' ? 3 : 1.5;
        }}
        onNodeClick={(n) => onNodeClick(n as MemoryNode)}
        onLinkClick={(l) => onLinkClick(l as MemoryLink)}
      />
    </div>
  );
};
