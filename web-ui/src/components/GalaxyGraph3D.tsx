import { useRef, useMemo, useEffect, useCallback, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import ForceGraph3D from 'react-force-graph-3d';
import * as THREE from 'three';
import type { GraphData, MemoryNode, MemoryLink } from '../types';

/// Whether this webview can actually give three.js a canvas to draw on.
///
/// The 3D view failed silently before this check existed: no crash, no message,
/// just the black background of an empty scene, which is indistinguishable from
/// a graph that has not loaded. A webview without hardware acceleration is a
/// normal state, not an error -- say so and offer the 2D view.
const detectWebGL = (): { ok: boolean; detail: string } => {
  try {
    const canvas = document.createElement('canvas');
    const gl =
      (canvas.getContext('webgl2') as WebGL2RenderingContext | null) ??
      (canvas.getContext('webgl') as WebGLRenderingContext | null);
    if (!gl) {
      return { ok: false, detail: 'the webview refused a WebGL context' };
    }
    const info = gl.getExtension('WEBGL_debug_renderer_info');
    const renderer = info ? String(gl.getParameter(info.UNMASKED_RENDERER_WEBGL)) : 'unknown renderer';
    return { ok: true, detail: renderer };
  } catch (e) {
    return { ok: false, detail: String(e) };
  }
};

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
  file: '#ffcc00',
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
  const [webgl] = useState(detectWebGL);

  useEffect(() => {
    invoke('log_message', {
      msg: webgl.ok
        ? `3D view: WebGL available (${webgl.detail})`
        : `3D view unavailable: ${webgl.detail}`,
    });
  }, [webgl]);

  // Frame the graph once per load. The camera starts around 870 units out while
  // the simulation settles the whole graph inside roughly 100 units, so every
  // sprite lands in a dim smudge a few pixels wide in the middle of a black
  // screen -- which reads as "the 3D view is broken" (bead neurostrata-u30).
  // The 2D view never had the problem because its default zoom happens to suit
  // that scale.
  const framedFor = useRef<GraphData | null>(null);

  const frameGraph = useCallback(() => {
    if (!fgRef.current || !data?.nodes?.length || framedFor.current === data) return;
    framedFor.current = data;
    try {
      fgRef.current.zoomToFit(600, 80);
      invoke('log_message', { msg: `3D view framed ${data.nodes.length} nodes` });
    } catch (e) {
      invoke('log_message', { msg: `3D view could not frame the graph: ${e}` });
    }
  }, [data]);

  useEffect(() => {
    framedFor.current = null;
    // The engine usually settles first and onEngineStop does the framing; this
    // is the backstop for a graph small enough that it never fires.
    const timer = setTimeout(frameGraph, 2500);
    return () => clearTimeout(timer);
  }, [data, frameGraph]);

  useEffect(() => {
    if (fgRef.current) {
      // Basic repulsion to spread out nodes
      fgRef.current.d3Force('charge').strength(-200);
      fgRef.current.d3Force('link').distance(50);
      
      // We manually hook into the d3 force engine loop to herd global nodes.
      // D3 applies forces step by step. We push global nodes towards a distant anchor,
      // and project nodes towards the center.
      fgRef.current.d3Force('namespace-clustering', (alpha: number) => {
        const nodes = data.nodes;
        if (!nodes) return;
        
        for (let i = 0; i < nodes.length; i++) {
          const node: any = nodes[i];
          const isGlobal = node.namespace === 'global' || node.namespace === 'Global';
          
          if (isGlobal) {
            // Pull Global nodes to a distant satellite cluster (x: 800, y: 800, z: 800)
            const strength = 0.5 * alpha;
            node.vx = (node.vx || 0) + (800 - (node.x || 0)) * strength;
            node.vy = (node.vy || 0) + (800 - (node.y || 0)) * strength;
            node.vz = (node.vz || 0) + (800 - (node.z || 0)) * strength;
          } else {
            // Pull Project nodes closer to center (0,0,0)
            const strength = 0.1 * alpha;
            node.vx = (node.vx || 0) + (0 - (node.x || 0)) * strength;
            node.vy = (node.vy || 0) + (0 - (node.y || 0)) * strength;
            node.vz = (node.vz || 0) + (0 - (node.z || 0)) * strength;
          }
        }
      });
      
      // Optional: re-warm the simulation so the custom force takes effect immediately
      fgRef.current.d3ReheatSimulation();
    }
  }, [data]);

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
    if (link.type === 'CONTAINS' || link.type === 'contains') return 'rgba(100, 150, 255, 0.4)';
    if (link.type === 'RELATES_TO' || link.type === 'links_to') return 'rgba(255, 100, 255, 0.6)';
    if (link.type === 'GOVERNS') return 'rgba(100, 255, 218, 0.6)';
    return 'rgba(255, 255, 255, 0.2)';
  }, [selectedNode]);

  const getLinkWidth = useCallback((link: any) => {
    const isSourceSelected = selectedNode && (typeof link.source === 'object' ? link.source.id === selectedNode.id : link.source === selectedNode.id);
    const isTargetSelected = selectedNode && (typeof link.target === 'object' ? link.target.id === selectedNode.id : link.target === selectedNode.id);
    if (isSourceSelected || isTargetSelected) return 6;
    
    return (link.type === 'RELATES_TO' || link.type === 'links_to') ? 3 : 1.5;
  }, [selectedNode]);

  if (!webgl.ok) {
    return (
      <div className="absolute inset-0 bg-black z-0 flex items-center justify-center p-8">
        <div className="max-w-md text-center border border-white/15 rounded-2xl bg-white/5 px-8 py-6">
          <p className="text-lg font-semibold text-blue-200">The 3D view needs WebGL</p>
          <p className="mt-2 text-sm text-gray-300">
            This window could not get a WebGL context, so the galaxy has nothing to draw on
            ({webgl.detail}). Turn off the 3D switch to use the 2D graph, which draws on a plain
            canvas and shows the same memories and edges.
          </p>
          <p className="mt-3 text-xs text-gray-500">
            Hardware acceleration being off or unavailable is the usual cause.
          </p>
        </div>
      </div>
    );
  }

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
        onEngineStop={frameGraph}
      />
    </div>
  );
};
