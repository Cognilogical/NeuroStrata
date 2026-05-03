#!/bin/bash
# install_hooks.sh
# Installs a pre-push git hook to enforce NeuroStrata memory extraction.

install_hook() {
    local hook_file="$1"
    
    mkdir -p "$(dirname "$hook_file")"
    
    cat << 'EOF' > "$hook_file"
#!/bin/bash
# NeuroStrata Pre-Push Hook (Behavioral Forcing)
# Ensures the agent (or user) has extracted knowledge to the memory DB before pushing.

DB_DIR="$HOME/.config/NeuroStrata/data/db"
DB_FILE="$HOME/.config/NeuroStrata/data/db/ladybug.db"
OLD_DB_FILE="$HOME/.config/NeuroStrata/data/db"

# Determine which DB to check
if [ -f "$DB_FILE" ]; then
    DB_TARGET="$DB_FILE"
elif [ -f "$OLD_DB_FILE" ]; then
    DB_TARGET="$OLD_DB_FILE"
elif [ -d "$DB_DIR" ]; then
    DB_TARGET="$DB_DIR"
else
    # If no DB exists yet, allow the push
    exit 0
fi

# Get the latest commit time
LAST_COMMIT_TIME=$(git log -1 --format="%ct" 2>/dev/null || echo 0)
# Get the db modification time (macOS/Linux compatible-ish)
DB_MOD_TIME=$(stat -c "%Y" "$DB_TARGET" 2>/dev/null || stat -f "%m" "$DB_TARGET" 2>/dev/null || echo 0)

# If the database hasn't been touched since the last commit, flag it.
if [ "$DB_MOD_TIME" -lt "$LAST_COMMIT_TIME" ]; then
    echo -e "\n\033[1;31m=================================================================\033[0m"
    echo -e "\033[1;31m[NeuroStrata] 🛑 WAIT! YOU FORGOT TO ADD A MEMORY!\033[0m"
    echo -e "\033[1;33mThe NeuroStrata database hasn't been updated since your last commit.\033[0m"
    echo -e "\033[1;33mYou MUST extract your architectural decisions, bug fixes, or context\033[0m"
    echo -e "\033[1;33musing the 'neurostrata_add_memory' tool before you push.\033[0m"
    echo -e "\033[1;31m=================================================================\033[0m\n"
    
    if [ -z "$NEUROSTRATA_SKIP_CHECK" ]; then
        echo -e "Push blocked. Run 'neurostrata_add_memory' or use NEUROSTRATA_SKIP_CHECK=1 git push\n"
        exit 1
    fi
    echo -e "NEUROSTRATA_SKIP_CHECK is set. Bypassing memory check...\n"
fi

# Auto-sync the AST Software Graph on push
if command -v neurostrata-mcp &> /dev/null; then
    echo -e "\n\033[1;36m[NeuroStrata] Auto-syncing AST Software Graph to LadybugDB...\033[0m"
    NAMESPACE=$(basename "$PWD")
    neurostrata-mcp ingest . "$NAMESPACE" >/dev/null 2>&1 || true
fi

exit 0
EOF

    chmod +x "$hook_file"
    echo "NeuroStrata hook installed successfully at $hook_file"
}

install_sync_hook() {
    local hook_file="$1"
    
    mkdir -p "$(dirname "$hook_file")"
    
    cat << 'EOF' > "$hook_file"
#!/bin/bash
# NeuroStrata Auto-Sync Hook
# Automatically syncs the AST graph when codebase state changes (commit/checkout).

if command -v neurostrata-mcp &> /dev/null; then
    # Run in background to not block the developer
    (
        NAMESPACE=$(basename "$PWD")
        neurostrata-mcp ingest . "$NAMESPACE" >/dev/null 2>&1 || true
    ) &
fi
exit 0
EOF

    chmod +x "$hook_file"
    echo "NeuroStrata AST sync hook installed at $hook_file"
}

if [ "$1" == "--global" ]; then
    echo "Installing global git hook template..."
    TEMPLATE_DIR="$HOME/.git-templates/hooks"
    install_hook "$TEMPLATE_DIR/pre-push"
    install_sync_hook "$TEMPLATE_DIR/post-commit"
    install_sync_hook "$TEMPLATE_DIR/post-checkout"
    install_sync_hook "$TEMPLATE_DIR/post-merge"
    git config --global init.templateDir "$HOME/.git-templates"
    echo "Global template configured! Future 'git init' or 'git clone' will include this hook."
    
    # Also install to the current NeuroStrata repo
    if [ -d ".git" ]; then
        install_hook ".git/hooks/pre-push"
        install_sync_hook ".git/hooks/post-commit"
        install_sync_hook ".git/hooks/post-checkout"
        install_sync_hook ".git/hooks/post-merge"
    fi
else
    if [ ! -d ".git" ]; then
        echo "Error: Must be run from the root of a git repository (or use --global)."
        exit 1
    fi
    install_hook ".git/hooks/pre-push"
    install_sync_hook ".git/hooks/post-commit"
    install_sync_hook ".git/hooks/post-checkout"
    install_sync_hook ".git/hooks/post-merge"
fi
