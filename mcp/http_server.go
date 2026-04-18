package main

import (
	"encoding/json"
	"fmt"
	"os"
	"net/http"
)

type MemoryRequest struct {
	ID       string      `json:"id"`
	Content  string      `json:"content"`
	UserID   string      `json:"user_id"`
	Metadata interface{} `json:"metadata"`
}

type SearchRequest struct {
	Query  string `json:"query"`
	UserID string `json:"user_id"`
	Limit  int    `json:"limit"`
}

func startHTTPServer() {
	mux := http.NewServeMux()

	mux.HandleFunc("/api/memory", func(w http.ResponseWriter, r *http.Request) {
		ctx := r.Context()
		if r.Method == http.MethodPost || r.Method == http.MethodPut {
			var req MemoryRequest
			if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
				http.Error(w, "Bad request", http.StatusBadRequest)
				return
			}
			if req.UserID == "" {
				req.UserID = "system_architecture"
			}

			id, err := UpsertPoint(ctx, req.ID, req.Content, req.UserID, req.Metadata)
			if err != nil {
				http.Error(w, err.Error(), http.StatusInternalServerError)
				return
			}
			json.NewEncoder(w).Encode(map[string]string{"status": "success", "id": id})

		} else if r.Method == http.MethodDelete {
			id := r.URL.Query().Get("id")
			if id == "" {
				http.Error(w, "Missing id", http.StatusBadRequest)
				return
			}
			err := DeletePoint(ctx, id)
			if err != nil {
				http.Error(w, err.Error(), http.StatusInternalServerError)
				return
			}
			json.NewEncoder(w).Encode(map[string]string{"status": "deleted"})
		} else {
			http.Error(w, "Method not allowed", http.StatusMethodNotAllowed)
		}
	})

	mux.HandleFunc("/api/memory/search", func(w http.ResponseWriter, r *http.Request) {
		ctx := r.Context()
		if r.Method != http.MethodPost {
			http.Error(w, "Use POST for search", http.StatusMethodNotAllowed)
			return
		}
		var req SearchRequest
		if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
			http.Error(w, "Bad request", http.StatusBadRequest)
			return
		}
		if req.UserID == "" {
			req.UserID = "system_architecture"
		}
		if req.Limit == 0 {
			req.Limit = 5
		}

		points, err := SearchPoints(ctx, req.Query, req.UserID, req.Limit)
		if err != nil {
			http.Error(w, err.Error(), http.StatusInternalServerError)
			return
		}

		results := []map[string]interface{}{}
		for _, p := range points {
			res := map[string]interface{}{
				"id": p["id"],
			}
			if payload, ok := p["payload"].(map[string]interface{}); ok {
				res["content"] = payload["data"]
				res["user_id"] = payload["user_id"]
			}
			results = append(results, res)
		}

		json.NewEncoder(w).Encode(map[string]interface{}{"results": results})
	})

	fmt.Fprintf(os.Stderr, "NeuroStrata REST API listening on :%s\n", cfg.HTTPPort)
	if err := http.ListenAndServe(":"+cfg.HTTPPort, mux); err != nil {
		fmt.Fprintf(os.Stderr, "HTTP Server error: %v\n", err)
	}
}
