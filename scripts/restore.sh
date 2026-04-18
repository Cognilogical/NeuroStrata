#!/bin/bash
set -e

# Configuration
BACKUP_FILE=$1
QDRANT_URL=${2:-"http://localhost:6333"}
COLLECTION_NAME=${3:-"neurostrata"}

if [ -z "$BACKUP_FILE" ]; then
    echo "Usage: ./restore.sh <path_to_snapshot.snapshot> [QDRANT_URL] [COLLECTION_NAME]"
    echo "Example: ./restore.sh ~/.config/neurostrata/db/backups/neurostrata-1234.snapshot"
    exit 1
fi

if [ ! -f "$BACKUP_FILE" ]; then
    echo "❌ Error: Backup file not found: $BACKUP_FILE"
    exit 1
fi

echo "⚠️ WARNING: This will OVERWRITE the current '$COLLECTION_NAME' collection in Qdrant."
echo "Any memories created since the backup will be lost."
read -p "Are you sure you want to proceed? (y/n) " -n 1 -r
echo
if [[ ! $REPLY =~ ^[Yy]$ ]]; then
    echo "Restore cancelled."
    exit 0
fi

echo "Uploading snapshot to Qdrant..."
RESPONSE=$(curl -s -X POST "$QDRANT_URL/collections/$COLLECTION_NAME/snapshots/upload?priority=snapshot" \
    -H "Content-Type: multipart/form-data" \
    -F "snapshot=@$BACKUP_FILE")

if echo "$RESPONSE" | grep -q '"status":"ok"'; then
    echo "====================================="
    echo "✅ Restore successfully completed!"
    echo "====================================="
else
    echo "❌ Error during restore:"
    echo "$RESPONSE"
    exit 1
fi
