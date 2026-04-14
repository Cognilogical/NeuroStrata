package main

import (
	"errors"
	"os"
	"path/filepath"
)

func validateMemoryPaths(userID string, metadataRaw interface{}) error {
	if metadataRaw == nil {
		return nil
	}

	metadata, ok := metadataRaw.(map[string]interface{})
	if !ok {
		return errors.New("invalid metadata format")
	}

	globalDir := filepath.Join(os.Getenv("HOME"), ".config/strata/global")

	validatePaths := func(refsKey string) error {
		refs, ok := metadata[refsKey].([]interface{})
		if !ok {
			return nil
		}

		for _, item := range refs {
			doc, ok := item.(map[string]interface{})
			if !ok {
				continue
			}
			if filePath, ok := doc["file"].(string); ok {
				absPath, _ := filepath.Abs(os.ExpandEnv(filePath))
				if userID == "global" {
					if !filepath.HasPrefix(absPath, globalDir) {
						return errors.New("global metadata file path must be inside ~/.config/strata/global directory")
					}
				} else {
					if filepath.HasPrefix(absPath, globalDir) {
						return errors.New("domain/task metadata file path cannot point to ~/.config/strata/global directory")
					}
				}
			}
		}
		return nil
	}

	if err := validatePaths("doc_refs"); err != nil {
		return err
	}
	if err := validatePaths("code_refs"); err != nil {
		return err
	}
	return nil
}
