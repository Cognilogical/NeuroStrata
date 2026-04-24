#!/bin/bash
# This script simulates an agent running.
# In a real setup, this would invoke `opencode run "..."` and pass your API keys.

if [ ! -d ".git" ]; then
    git init
    git config user.email "bot@test.com"
    git config user.name "Test Bot"
    git commit --allow-empty -m "initial commit"
fi

echo ">>> AGENT BOOTING UP..."
# For Epoch 1 (Failure state): The agent forgets to use bd, forgets to add memory, but creates the file.
# NeuroPlasticity will intercept this failure, and inject a fix into `.neuroplasticity/rules.json`.

# Check if the LLM Meta-Optimizer wrote a rule to fix us!
if grep -iq "bd" .neuroplasticity/rules.json 2>/dev/null; then
    echo "Agent read the new rule! Using bd tracker..."
    bd create "Track task"
fi

echo "Creating hello.txt..."
echo "hello world" > hello.txt
git add hello.txt
git commit -m "add hello.txt"

if grep -iq "neurostrata" .neuroplasticity/rules.json 2>/dev/null; then
    echo "Agent read the new rule! Adding memory..."
    neurostrata_add_memory "Added hello.txt"
fi

echo ">>> AGENT DONE."
