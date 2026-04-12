package main

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"
)

func IngestDirectory(ctx context.Context, dirPath, userID string) (int, error) {
	count := 0
	err := filepath.Walk(dirPath, func(path string, info os.FileInfo, err error) error {
		if err != nil {
			return err
		}
		if info.IsDir() || filepath.Ext(path) != ".md" {
			return nil
		}

		contentBytes, err := os.ReadFile(path)
		if err != nil {
			return err
		}

		content := string(contentBytes)
		chunks := strings.Split(content, "\n\n")

		for _, chunk := range chunks {
			chunk = strings.TrimSpace(chunk)
			if len(chunk) < 30 {
				continue
			}

			// Small delay to prevent hammering local embedder
			time.Sleep(100 * time.Millisecond)

			metadata := map[string]interface{}{"file": path, "source": "auto-ingest"}

			_, err := UpsertPoint(ctx, "", chunk, userID, metadata)
			if err != nil {
				fmt.Printf("Warning: failed to ingest chunk from %s: %v\n", path, err)
				continue
			}
			count++
		}
		return nil
	})
	return count, err
}
