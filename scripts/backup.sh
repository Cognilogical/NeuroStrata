#!/bin/bash
set -e

# Default settings
QDRANT_URL=${1:-"http://localhost:6333"}
COLLECTION_NAME=${2:-"strata"}
BACKUP_DIR="$HOME/.config/strata/db/backups"

mkdir -p "$BACKUP_DIR"

echo "Creating snapshot for Qdrant collection '$COLLECTION_NAME' at $QDRANT_URL..."

# Trigger snapshot generation inside Qdrant
RESPONSE=$(curl -s -X POST "$QDRANT_URL/collections/$COLLECTION_NAME/snapshots")

# Extract filename
if command -v jq &> /dev/null; then
    SNAPSHOT_NAME=$(echo "$RESPONSE" | jq -r '.result.name')
else
    # Fallback to grep if jq is not installed
    SNAPSHOT_NAME=$(echo "$RESPONSE" | grep -o '"name":"[^"]*' | cut -d'"' -f4)
fi

if [ -z "$SNAPSHOT_NAME" ] || [ "$SNAPSHOT_NAME" == "null" ]; then
    echo "Error: Failed to create snapshot."
    echo "Response: $RESPONSE"
    exit 1
fi

BACKUP_PATH="$BACKUP_DIR/$SNAPSHOT_NAME"
echo "Downloading snapshot to $BACKUP_PATH..."

# Download snapshot
curl -s -o "$BACKUP_PATH" "$QDRANT_URL/collections/$COLLECTION_NAME/snapshots/$SNAPSHOT_NAME"

echo "Cleaning up internal Qdrant storage..."
curl -s -X DELETE "$QDRANT_URL/collections/$COLLECTION_NAME/snapshots/$SNAPSHOT_NAME" > /dev/null

echo "====================================="
echo "✅ Backup successfully completed!"
echo "📍 Location: $BACKUP_PATH"
echo "====================================="
