FROM node:20-bookworm

# Install core tools
RUN apt-get update && apt-get install -y \
    git \
    curl \
    jq \
    && rm -rf /var/lib/apt/lists/*

# Add a mock 'bd' command so the container doesn't fail when the agent calls it
# In a real environment, we'd install the real 133MB binary
RUN echo '#!/bin/bash\nmkdir -p .beads && echo "{\"status\": \"working\"}" > .beads/task.json\necho "Mock BD command ran!"' > /usr/local/bin/bd && \
    chmod +x /usr/local/bin/bd

# Add a mock 'neurostrata_add_memory' tool so the agent can succeed
RUN echo '#!/bin/bash\nmkdir -p .NeuroStrata/sessions && echo "{\"memory\": \"learned something\"}" > .NeuroStrata/sessions/memory.jsonl\necho "Mock NeuroStrata Memory Added!"' > /usr/local/bin/neurostrata_add_memory && \
    chmod +x /usr/local/bin/neurostrata_add_memory

# Set up git
RUN git config --global user.email "bot@neurostrata.dev" && \
    git config --global user.name "Agent Testbed" && \
    git config --global init.defaultBranch main

WORKDIR /workspace
