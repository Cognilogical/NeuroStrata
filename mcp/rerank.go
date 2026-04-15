package main

import (
	"context"
)

// RerankPoints is a placeholder for a reranker pipeline.
// If a RerankerURL is configured, it would call out to an external re-scoring API
// (e.g. cross-encoder/bge-reranker) to adjust the vector search results.
// For now, it acts as a passthrough to satisfy the architectural requirement.
func RerankPoints(ctx context.Context, query string, points []map[string]interface{}) ([]map[string]interface{}, error) {
	if cfg.RerankerURL == "" {
		// Pass-through if no reranker configured
		return points, nil
	}

	// TODO: Implement HTTP POST to cfg.RerankerURL using cfg.RerankerModel
	// The response would contain adjusted scores, allowing us to re-sort the 'points' slice.
	return points, nil
}
