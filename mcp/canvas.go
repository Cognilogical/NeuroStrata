package main

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
)

type CanvasNode struct {
	ID     string `json:"id"`
	Type   string `json:"type"`
	Text   string `json:"text"`
	X      int    `json:"x"`
	Y      int    `json:"y"`
	Width  int    `json:"width"`
	Height int    `json:"height"`
	Color  string `json:"color,omitempty"`
}

type CanvasEdge struct{}

type Canvas struct {
	Nodes []CanvasNode `json:"nodes"`
	Edges []CanvasEdge `json:"edges"`
}

func GenerateCanvas(ctx context.Context, vaultPath string) error {
	points, err := ScrollPoints(ctx, "system_architecture")
	if err != nil {
		return fmt.Errorf("failed to fetch points: %w", err)
	}

	canvas := Canvas{
		Nodes: make([]CanvasNode, 0),
		Edges: make([]CanvasEdge, 0),
	}

	x, y := 0, 0
	for _, p := range points {
		id, _ := p["id"].(string)
		payload, _ := p["payload"].(map[string]interface{})
		data, _ := payload["data"].(string)

		text := fmt.Sprintf("**NeuroStrata Memory**\nID: `%s`\n\n%s", id, data)

		node := CanvasNode{
			ID:     id,
			Type:   "text",
			Text:   text,
			X:      x,
			Y:      y,
			Width:  400,
			Height: 300,
			Color:  "2", // Obsidian purpleish
		}
		canvas.Nodes = append(canvas.Nodes, node)

		x += 450
		if x > 4000 {
			x = 0
			y += 350
		}
	}

	outPath := filepath.Join(vaultPath, "NeuroStrata MemorySpace.canvas")
	b, err := json.MarshalIndent(canvas, "", "  ")
	if err != nil {
		return err
	}

	return os.WriteFile(outPath, b, 0644)
}
