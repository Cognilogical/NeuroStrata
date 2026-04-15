package main

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/mark3labs/mcp-go/mcp"
)

const maxLogFileSize int64 = 500 * 1024 // 500 KB

func appendLogHandler(ctx context.Context, request mcp.CallToolRequest) (*mcp.CallToolResult, error) {
	if err := ctx.Err(); err != nil {
		return nil, err
	}

	content, err := request.RequireString("content")
	if err != nil {
		return mcp.NewToolResultError(err.Error()), nil
	}

	args := request.GetArguments()
	var tags []string
	if rawTags, ok := args["tags"].(string); ok && strings.TrimSpace(rawTags) != "" {
		parts := strings.Split(rawTags, ",")
		for _, p := range parts {
			t := strings.TrimSpace(p)
			if t != "" {
				tags = append(tags, t)
			}
		}
	}

	logDir := filepath.Join(".strata", "sessions")
	if err := os.MkdirAll(logDir, 0755); err != nil {
		return mcp.NewToolResultError(fmt.Sprintf("failed to create log directory: %v", err)), nil
	}

	currentFile := filepath.Join(logDir, "current.md")

	if stat, err := os.Stat(currentFile); err == nil {
		if stat.Size() > maxLogFileSize {
			timestamp := time.Now().Format("2006-01-02-150405")
			rolledFile := filepath.Join(logDir, fmt.Sprintf("%s.md", timestamp))
			if err := os.Rename(currentFile, rolledFile); err != nil {
				return mcp.NewToolResultError(fmt.Sprintf("failed to roll log file: %v", err)), nil
			}
		}
	}

	f, err := os.OpenFile(currentFile, os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0644)
	if err != nil {
		return mcp.NewToolResultError(fmt.Sprintf("failed to open log file: %v", err)), nil
	}
	defer f.Close()

	now := time.Now().Format("2006-01-02 15:04:05")
	var entry string

	if len(tags) > 0 {
		tagStr := ""
		for _, t := range tags {
			if !strings.HasPrefix(t, "#") {
				tagStr += "#" + t + " "
			} else {
				tagStr += t + " "
			}
		}
		tagStr = strings.TrimSpace(tagStr)

		entry = fmt.Sprintf("\n---\n### 🔄 Topic Switch: %s\n*Time: %s*\n---\n%s\n", tagStr, now, content)
	} else {
		entry = fmt.Sprintf("- *[%s]*: %s\n", now, content)
	}

	if _, err := f.WriteString(entry); err != nil {
		return mcp.NewToolResultError(fmt.Sprintf("failed to write to log file: %v", err)), nil
	}

	return mcp.NewToolResultText("Log appended successfully."), nil
}
