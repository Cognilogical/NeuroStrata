package main

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
)

// GraphEdge represents a connection parsed from graphify-out
type GraphEdge struct {
	Source string `json:"source"`
	Target string `json:"target"`
	Label  string `json:"label"`
}

// FuseGraphEdges reads the local Graphify knowledge graph (if it exists)
// and looks for edges connected to the found vector ID or file reference.
func FuseGraphEdges(ctx context.Context, points []map[string]interface{}) string {
	// Attempt to find graphify-out in the current directory (project root)
	cwd, err := os.Getwd()
	if err != nil {
		return ""
	}

	edgesFile := filepath.Join(cwd, "graphify-out", "edges.json")
	data, err := os.ReadFile(edgesFile)
	if err != nil {
		// No graphify graph exists
		return ""
	}

	var edges []GraphEdge
	if err := json.Unmarshal(data, &edges); err != nil {
		return ""
	}

	if len(edges) == 0 {
		return ""
	}

	// This is a minimal mockup logic: if there are edges, we might extract
	// any interesting ones related to the returned pointers.
	// For now, if the graph is present, we append a generalized notice
	// indicating Edge Fusion is active.
	fusedText := "\n> [🕸️ Graphify Edge Fusion]: Local Knowledge Graph detected. "
	fusedText += fmt.Sprintf("Loaded %d edges to support context traversal.\n", len(edges))

	return fusedText
}
