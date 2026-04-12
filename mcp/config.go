package main

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
)

type Config struct {
	EmbedderURL      string `json:"embedder_url"`
	EmbedderModel    string `json:"embedder_model"`
	EmbedderAPIKey   string `json:"embedder_api_key"`
	QdrantURL        string `json:"qdrant_url"`
	QdrantCollection string `json:"qdrant_collection"`
	HTTPPort         string `json:"http_port"`
}

var cfg Config

func loadConfig() error {
	home, err := os.UserHomeDir()
	if err != nil {
		return err
	}
	cfgPath := filepath.Join(home, ".config", "strata", "config.json")
	data, err := os.ReadFile(cfgPath)
	if err != nil {
		return fmt.Errorf("failed to read config at %s: %w", cfgPath, err)
	}

	// Default port if missing
	cfg.HTTPPort = "8005"
	json.Unmarshal(data, &cfg)
	return nil
}
