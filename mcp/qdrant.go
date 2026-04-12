package main

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"os"
	"time"

	"github.com/google/uuid"
)

type EmbedRequest struct {
	Input string `json:"input"`
	Model string `json:"model"`
}

type EmbedResponse struct {
	Data []struct {
		Embedding []float32 `json:"embedding"`
	} `json:"data"`
}

func getEmbedding(ctx context.Context, text string) ([]float32, error) {
	reqBody := EmbedRequest{Input: text, Model: cfg.EmbedderModel}
	body, err := json.Marshal(reqBody)
	if err != nil {
		return nil, err
	}

	req, err := http.NewRequestWithContext(ctx, "POST", cfg.EmbedderURL, bytes.NewBuffer(body))
	if err != nil {
		return nil, err
	}
	req.Header.Set("Content-Type", "application/json")
	if cfg.EmbedderAPIKey != "" {
		req.Header.Set("Authorization", "Bearer "+cfg.EmbedderAPIKey)
	}

	client := &http.Client{Timeout: 30 * time.Second}
	resp, err := client.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()

	if resp.StatusCode >= 400 {
		return nil, fmt.Errorf("embedder error: status code %d", resp.StatusCode)
	}

	var res EmbedResponse
	if err := json.NewDecoder(resp.Body).Decode(&res); err != nil {
		return nil, err
	}
	if len(res.Data) == 0 {
		return nil, errors.New("no embedding returned")
	}
	return res.Data[0].Embedding, nil
}

func UpsertPoint(ctx context.Context, id, content, userID string, metadataRaw interface{}) (string, error) {
	if id == "" {
		id = uuid.New().String()
	}

	embedding, err := getEmbedding(ctx, content)
	if err != nil {
		return "", fmt.Errorf("failed to generate embedding: %w", err)
	}

	payload := map[string]interface{}{"data": content, "user_id": userID}

	if metadataRaw != nil {
		switch v := metadataRaw.(type) {
		case map[string]interface{}:
			for k, val := range v {
				payload[k] = val
			}
		case string:
			var parsed map[string]interface{}
			if err := json.Unmarshal([]byte(v), &parsed); err == nil {
				for k, val := range parsed {
					payload[k] = val
				}
			}
		}
	}

	point := map[string]interface{}{"id": id, "vector": embedding, "payload": payload}
	qdrantReq := map[string]interface{}{"points": []interface{}{point}}
	qBody, _ := json.Marshal(qdrantReq)
	url := fmt.Sprintf("%s/collections/%s/points?wait=true", cfg.QdrantURL, cfg.QdrantCollection)

	req, err := http.NewRequestWithContext(ctx, "POST", url, bytes.NewBuffer(qBody))
	if err != nil {
		return "", err
	}
	req.Header.Set("Content-Type", "application/json")

	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return "", err
	}
	defer resp.Body.Close()

	if resp.StatusCode >= 400 {
		return "", fmt.Errorf("qdrant error: status code %d", resp.StatusCode)
	}
	return id, nil
}

func SearchPoints(ctx context.Context, query, userID string, limit int) ([]map[string]interface{}, error) {
	embedding, err := getEmbedding(ctx, query)
	if err != nil {
		return nil, err
	}

	qdrantReq := map[string]interface{}{
		"vector": embedding, "limit": limit, "with_payload": true,
		"filter": map[string]interface{}{
			"must": []interface{}{
				map[string]interface{}{"key": "user_id", "match": map[string]interface{}{"value": userID}},
			},
		},
	}
	qBody, _ := json.Marshal(qdrantReq)
	url := fmt.Sprintf("%s/collections/%s/points/search", cfg.QdrantURL, cfg.QdrantCollection)

	req, err := http.NewRequestWithContext(ctx, "POST", url, bytes.NewBuffer(qBody))
	if err != nil {
		return nil, err
	}
	req.Header.Set("Content-Type", "application/json")

	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()

	if resp.StatusCode >= 400 {
		return nil, fmt.Errorf("qdrant search error: %d", resp.StatusCode)
	}

	var result struct {
		Result []map[string]interface{} `json:"result"`
	}
	if err := json.NewDecoder(resp.Body).Decode(&result); err != nil {
		return nil, err
	}
	return result.Result, nil
}

func DeletePoint(ctx context.Context, id string) error {
	qdrantReq := map[string]interface{}{"points": []string{id}}
	qBody, _ := json.Marshal(qdrantReq)
	url := fmt.Sprintf("%s/collections/%s/points/delete?wait=true", cfg.QdrantURL, cfg.QdrantCollection)

	req, err := http.NewRequestWithContext(ctx, "POST", url, bytes.NewBuffer(qBody))
	if err != nil {
		return err
	}
	req.Header.Set("Content-Type", "application/json")

	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()

	if resp.StatusCode >= 400 {
		return fmt.Errorf("qdrant delete error: %d", resp.StatusCode)
	}
	return nil
}

func ScrollPoints(ctx context.Context, userID string) ([]map[string]interface{}, error) {
	qdrantReq := map[string]interface{}{
		"limit": 10000, "with_payload": true,
		"filter": map[string]interface{}{
			"must": []interface{}{
				map[string]interface{}{"key": "user_id", "match": map[string]interface{}{"value": userID}},
			},
		},
	}
	qBody, _ := json.Marshal(qdrantReq)
	url := fmt.Sprintf("%s/collections/%s/points/scroll", cfg.QdrantURL, cfg.QdrantCollection)

	req, err := http.NewRequestWithContext(ctx, "POST", url, bytes.NewBuffer(qBody))
	if err != nil {
		return nil, err
	}
	req.Header.Set("Content-Type", "application/json")

	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()

	if resp.StatusCode >= 400 {
		return nil, fmt.Errorf("qdrant scroll error: %d", resp.StatusCode)
	}

	var result struct {
		Result struct {
			Points []map[string]interface{} `json:"points"`
		} `json:"result"`
	}
	if err := json.NewDecoder(resp.Body).Decode(&result); err != nil {
		return nil, err
	}
	return result.Result.Points, nil
}

func DumpDatabase(ctx context.Context, outputPath string) (int, error) {
	var allPoints []map[string]interface{}
	var offset interface{}

	for {
		qdrantReq := map[string]interface{}{
			"limit":        100,
			"with_payload": true,
			"with_vector":  true,
		}
		if offset != nil {
			qdrantReq["offset"] = offset
		}

		qBody, _ := json.Marshal(qdrantReq)
		url := fmt.Sprintf("%s/collections/%s/points/scroll", cfg.QdrantURL, cfg.QdrantCollection)

		req, err := http.NewRequestWithContext(ctx, "POST", url, bytes.NewBuffer(qBody))
		if err != nil {
			return 0, err
		}
		req.Header.Set("Content-Type", "application/json")

		resp, err := http.DefaultClient.Do(req)
		if err != nil {
			return 0, err
		}

		if resp.StatusCode >= 400 {
			resp.Body.Close()
			return 0, fmt.Errorf("qdrant scroll error: %d", resp.StatusCode)
		}

		var result struct {
			Result struct {
				Points         []map[string]interface{} `json:"points"`
				NextPageOffset interface{}              `json:"next_page_offset"`
			} `json:"result"`
		}

		if err := json.NewDecoder(resp.Body).Decode(&result); err != nil {
			resp.Body.Close()
			return 0, err
		}
		resp.Body.Close()

		points := result.Result.Points
		if len(points) == 0 {
			break
		}

		allPoints = append(allPoints, points...)
		offset = result.Result.NextPageOffset
		if offset == nil {
			break
		}
	}

	outBytes, err := json.MarshalIndent(allPoints, "", "  ")
	if err != nil {
		return 0, err
	}

	err = os.WriteFile(outputPath, outBytes, 0644)
	if err != nil {
		return 0, err
	}

	return len(allPoints), nil
}
