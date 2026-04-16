package main

import (
	"fmt"
	"os"

	"github.com/mark3labs/mcp-go/mcp"
	"github.com/mark3labs/mcp-go/server"
)

func main() {
	if err := loadConfig(); err != nil {
		fmt.Fprintf(os.Stderr, "Config error: %v\n", err)
		os.Exit(1)
	}

	// 1. Start Dual-Mode HTTP Server
	go startHTTPServer()

	// 2. Start MCP Server
	s := server.NewMCPServer("Strata Native Agent Memory", "2.0.0")

	// Base Tools
	s.AddTool(mcp.NewTool("strata_add_memory",
		mcp.WithDescription("Store an architectural rule, project pattern, or task insight."),
		mcp.WithString("content", mcp.Required(), mcp.Description("The text of the memory to save.")),
		mcp.WithString("user_id", mcp.DefaultString("system_architecture"), mcp.Description("The namespace.")),
		mcp.WithObject("metadata", mcp.Description("Optional dictionary with Bi-Directional Anchors: {\"doc_refs\":[{\"file\":\"...\"}], \"code_refs\":[{\"file\":\"...\", \"symbol\":\"...\"}]}")),
	), addMemoryHandler)

	s.AddTool(mcp.NewTool("strata_search_memory",
		mcp.WithDescription("Search the project's long-term memory for architectural rules."),
		mcp.WithString("query", mcp.Required(), mcp.Description("What to search for.")),
		mcp.WithString("user_id", mcp.DefaultString("system_architecture"), mcp.Description("The namespace.")),
	), searchMemoryHandler)

	// CRUD Enhancement Tools
	s.AddTool(mcp.NewTool("strata_update_memory",
		mcp.WithDescription("Update an existing memory by ID. Use search first to get the ID."),
		mcp.WithString("id", mcp.Required(), mcp.Description("The ID of the memory to update.")),
		mcp.WithString("content", mcp.Required(), mcp.Description("The new text of the memory.")),
		mcp.WithString("user_id", mcp.DefaultString("system_architecture"), mcp.Description("The namespace.")),
		mcp.WithObject("metadata", mcp.Description("Optional dictionary with Bi-Directional Anchors: {\"doc_refs\":[{\"file\":\"...\"}], \"code_refs\":[{\"file\":\"...\", \"symbol\":\"...\"}]}")),
	), updateMemoryHandler)

	s.AddTool(mcp.NewTool("strata_delete_memory",
		mcp.WithDescription("Delete an obsolete or incorrect memory by ID."),
		mcp.WithString("id", mcp.Required(), mcp.Description("The ID of the memory to delete.")),
	), deleteMemoryHandler)

	// Advanced Enhancement Tools
	s.AddTool(mcp.NewTool("strata_generate_canvas",
		mcp.WithDescription("Generate an Obsidian Canvas file visualizing the current memory space."),
		mcp.WithString("vault_path", mcp.Required(), mcp.Description("Absolute path to the Obsidian vault.")),
	), generateCanvasHandler)

	s.AddTool(mcp.NewTool("strata_ingest_directory",
		mcp.WithDescription("Read all markdown files in a directory and embed them into Strata."),
		mcp.WithString("path", mcp.Required(), mcp.Description("Absolute path to the architecture directory.")),
		mcp.WithString("user_id", mcp.DefaultString("system_architecture"), mcp.Description("The namespace.")),
	), ingestDirectoryHandler)

	s.AddTool(mcp.NewTool("strata_dump_db",
		mcp.WithDescription("Dump the entire Strata vector database to a JSON file for backup purposes."),
		mcp.WithString("output_path", mcp.Required(), mcp.Description("Absolute path to save the JSON dump.")),
	), dumpDbHandler)

	s.AddTool(mcp.NewTool("strata_append_log",
		mcp.WithDescription("Silently append a summary of tasks or architectural decisions to the continuous backup log."),
		mcp.WithString("content", mcp.Required(), mcp.Description("The summary to append.")),
		mcp.WithString("project_root", mcp.Required(), mcp.Description("Absolute path to the current local project workspace.")),
		mcp.WithString("tags", mcp.Description("Optional comma-separated list of tags ONLY if you are switching subjects (e.g. 'auth, database'). Do NOT provide tags for minor updates.")),
	), appendLogHandler)

	// Serve over stdio
	if err := server.ServeStdio(s); err != nil {
		fmt.Fprintf(os.Stderr, "Server error: %v\n", err)
	}
}
