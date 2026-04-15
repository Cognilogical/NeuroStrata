package main

import (
	"context"
	"fmt"
	"github.com/mark3labs/mcp-go/mcp"
	"time"
)

func addMemoryHandler(ctx context.Context, request mcp.CallToolRequest) (*mcp.CallToolResult, error) {
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	content, err := request.RequireString("content")
	if err != nil {
		return mcp.NewToolResultError(err.Error()), nil
	}
	userID := request.GetString("user_id", "system_architecture")
	args := request.GetArguments()

	opCtx, cancel := context.WithTimeout(ctx, 30*time.Second)
	defer cancel()

	if err := validateMemoryPaths(userID, args["metadata"]); err != nil {
		return mcp.NewToolResultError(err.Error()), nil
	}

	id, err := UpsertPoint(opCtx, "", content, userID, args["metadata"])
	if err != nil {
		return nil, err
	}
	return mcp.NewToolResultText(fmt.Sprintf("Memory added successfully. ID: %s", id)), nil
}

func searchMemoryHandler(ctx context.Context, request mcp.CallToolRequest) (*mcp.CallToolResult, error) {
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	query, err := request.RequireString("query")
	if err != nil {
		return mcp.NewToolResultError(err.Error()), nil
	}
	userID := request.GetString("user_id", "system_architecture")

	opCtx, cancel := context.WithTimeout(ctx, 30*time.Second)
	defer cancel()

	out := ""

	// 1. Search Global Tier First
	globalPoints, err := SearchPoints(opCtx, query, "global", 3)
	if err == nil && len(globalPoints) > 0 {
		for _, p := range globalPoints {
			id, _ := p["id"].(string)
			payload, _ := p["payload"].(map[string]interface{})
			data, _ := payload["data"].(string)
			out += fmt.Sprintf("- [🌍 GLOBAL DIRECTIVE] [ID: %s] %s\n", id, data)
		}
	}

	// 2. Search Local Project Tier
	if userID != "global" {
		localPoints, err := SearchPoints(opCtx, query, userID, 5)
		if err == nil && len(localPoints) > 0 {
			for _, p := range localPoints {
				id, _ := p["id"].(string)
				payload, _ := p["payload"].(map[string]interface{})
				data, _ := payload["data"].(string)
				out += fmt.Sprintf("- [🛑 CRITICAL PROJECT RULE] [ID: %s] %s\n", id, data)
			}
		}
	}

	if out == "" {
		return mcp.NewToolResultText("No relevant memories found."), nil
	}

	return mcp.NewToolResultText(out), nil
}

func updateMemoryHandler(ctx context.Context, request mcp.CallToolRequest) (*mcp.CallToolResult, error) {
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	id, err := request.RequireString("id")
	if err != nil {
		return mcp.NewToolResultError(err.Error()), nil
	}
	content, err := request.RequireString("content")
	if err != nil {
		return mcp.NewToolResultError(err.Error()), nil
	}

	userID := request.GetString("user_id", "system_architecture")
	args := request.GetArguments()

	opCtx, cancel := context.WithTimeout(ctx, 30*time.Second)
	defer cancel()

	if err := validateMemoryPaths(userID, args["metadata"]); err != nil {
		return mcp.NewToolResultError(err.Error()), nil
	}

	_, err = UpsertPoint(opCtx, id, content, userID, args["metadata"])
	if err != nil {
		return nil, err
	}
	return mcp.NewToolResultText("Memory updated successfully."), nil
}

func deleteMemoryHandler(ctx context.Context, request mcp.CallToolRequest) (*mcp.CallToolResult, error) {
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	id, err := request.RequireString("id")
	if err != nil {
		return mcp.NewToolResultError(err.Error()), nil
	}

	opCtx, cancel := context.WithTimeout(ctx, 30*time.Second)
	defer cancel()

	if err := DeletePoint(opCtx, id); err != nil {
		return nil, err
	}
	return mcp.NewToolResultText("Memory deleted successfully."), nil
}

func generateCanvasHandler(ctx context.Context, request mcp.CallToolRequest) (*mcp.CallToolResult, error) {
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	vaultPath, err := request.RequireString("vault_path")
	if err != nil {
		return mcp.NewToolResultError(err.Error()), nil
	}

	opCtx, cancel := context.WithTimeout(ctx, 30*time.Second)
	defer cancel()

	if err := GenerateCanvas(opCtx, vaultPath); err != nil {
		return nil, err
	}
	return mcp.NewToolResultText("Canvas generated successfully at " + vaultPath), nil
}

func ingestDirectoryHandler(ctx context.Context, request mcp.CallToolRequest) (*mcp.CallToolResult, error) {
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	path, err := request.RequireString("path")
	if err != nil {
		return mcp.NewToolResultError(err.Error()), nil
	}
	userID := request.GetString("user_id", "system_architecture")

	// Ingesting might take longer, give it 5 minutes
	opCtx, cancel := context.WithTimeout(ctx, 5*time.Minute)
	defer cancel()

	count, err := IngestDirectory(opCtx, path, userID)
	if err != nil {
		return nil, err
	}
	return mcp.NewToolResultText(fmt.Sprintf("Successfully ingested %d markdown chunks.", count)), nil
}

func dumpDbHandler(ctx context.Context, request mcp.CallToolRequest) (*mcp.CallToolResult, error) {
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	outputPath, err := request.RequireString("output_path")
	if err != nil {
		return mcp.NewToolResultError(err.Error()), nil
	}

	opCtx, cancel := context.WithTimeout(ctx, 2*time.Minute)
	defer cancel()

	count, err := DumpDatabase(opCtx, outputPath)
	if err != nil {
		return nil, err
	}
	return mcp.NewToolResultText(fmt.Sprintf("Successfully exported %d points to %s", count, outputPath)), nil
}
