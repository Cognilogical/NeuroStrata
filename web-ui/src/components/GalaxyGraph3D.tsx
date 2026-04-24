import { useRef, useMemo, useEffect, useCallback } from 'react';
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
      if (!data || !data.nodes) return;
      
      const graphNode = data.nodes.find((n: any) => n.id === selectedNode.id);
      
      if (graphNode && typeof graphNode.x === 'number' && !Number.isNaN(graphNode.x)) {
        const nx = graphNode.x || 0;
        const ny = graphNode.y || 0;
        const nz = graphNode.z || 0;

        const distance = Math.hypot(nx, ny, nz);
        const distRatio = 1 + 60 / (distance || 1);
        
        const newPos = distance > 0
          ? { x: nx * distRatio, y: ny * distRatio, z: nz * distRatio }
          : { x: 0, y: 0, z: 100 };
        
        const lookAtPos = { x: nx, y: ny, z: nz };
        
        try {
          fgRef.current.cameraPosition(newPos, lookAtPos, 1500);
        } catch (err) {
          console.error('Failed to set camera position:', err);
        }
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

  const createNodeObject = useCallback((node: any) => {
    if (!node) return new THREE.Object3D();
    const mNode = node as MemoryNode;
    const material = nodeMaterials[mNode.memory_type] || defaultNodeMaterial;
    const size = Math.max(16, (mNode.degree || 1) * 3);
    
    const sprite = new THREE.Sprite(material);
    sprite.scale.set(size, size, 1);
    
    // Store original size and material for selection toggling
    sprite.userData = {
      originalMaterial: material,
      originalSize: size
    };
    
    return sprite;
  }, [nodeMaterials, defaultNodeMaterial]);

  useEffect(() => {
    if (!data || !data.nodes) return;
    
    data.nodes.forEach((node: any) => {
      const obj = node.__threeObj;
      if (!obj) return;
      
      const isSelected = selectedNode && selectedNode.id === node.id;
      
      if (isSelected) {
        obj.material = highlightMaterial;
        obj.scale.set(obj.userData.originalSize * 1.5, obj.userData.originalSize * 1.5, 1);
      } else {
        obj.material = obj.userData.originalMaterial;
        obj.scale.set(obj.userData.originalSize, obj.userData.originalSize, 1);
      }
    });
  }, [selectedNode, data, highlightMaterial]);

  const getLinkColor = useCallback((link: any) => {
    const isSourceSelected = selectedNode && (typeof link.source === 'object' ? link.source.id === selectedNode.id : link.source === selectedNode.id);
    const isTargetSelected = selectedNode && (typeof link.target === 'object' ? link.target.id === selectedNode.id : link.target === selectedNode.id);
    const highlight = isSourceSelected || isTargetSelected;
    
    if (highlight) return 'rgba(255, 255, 255, 0.9)';
    if (link.type === 'contains') return 'rgba(100, 150, 255, 0.4)';
    if (link.type === 'links_to') return 'rgba(255, 100, 255, 0.6)';
    return 'rgba(255, 255, 255, 0.2)';
  }, [selectedNode]);

  const getLinkWidth = useCallback((link: any) => {
    const isSourceSelected = selectedNode && (typeof link.source === 'object' ? link.source.id === selectedNode.id : link.source === selectedNode.id);
    const isTargetSelected = selectedNode && (typeof link.target === 'object' ? link.target.id === selectedNode.id : link.target === selectedNode.id);
    if (isSourceSelected || isTargetSelected) return 6;
    
    return link.type === 'links_to' ? 3 : 1.5;
  }, [selectedNode]);

  return (
    <div className="absolute inset-0 bg-black z-0">
      <ForceGraph3D
        ref={fgRef}
        graphData={data}
        backgroundColor="#000000"
        nodeThreeObject={createNodeObject}
        linkColor={getLinkColor}
        linkWidth={getLinkWidth}
        onNodeClick={(n) => onNodeClick(n as MemoryNode)}
        onLinkClick={(l) => onLinkClick(l as MemoryLink)}
      />
    </div>
  );
};
