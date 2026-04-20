#!/usr/bin/env python3
"""
NeuroStrata SRTF (Self-Reinforced Testing Framework) Orchestrator
Implements a DSPy-style mutation and evaluation loop to optimize agent memory retention.
"""

import os
import subprocess
import json
import math
import shutil
import time

# --- Configuration ---
SRTF_DIR = os.path.dirname(os.path.abspath(__file__))
PROJECT_ROOT = os.path.abspath(os.path.join(SRTF_DIR, "../.."))
CONFIGS_DIR = os.path.join(SRTF_DIR, "configs")
WORKSPACE_DIR = os.path.join(SRTF_DIR, "workspace")
ACTIVE_SKILL_PATH = os.path.join(CONFIGS_DIR, "SKILL.md")
BASE_SKILL_PATH = os.path.join(PROJECT_ROOT, ".agents", "skills", "neurostrata", "SKILL.md")

# Evaluation weights
ALPHA = 1.0  # Recall weight
BETA = 1.0   # Precision weight
GAMMA = 0.1  # Token penalty weight

# --- Mock Mutation Strategies (DSPy style boundaries) ---
MUTATION_SPACE = {
    "trigger_phrasing": [
        "Before you close the task, you MUST run neurostrata_add_memory.",
        "IMMEDIATELY upon learning a new fact, execute neurostrata_add_memory.",
        "CRITICAL: Do not type 'bd close' until neurostrata_add_memory has been executed."
    ],
    "format_constraint": [
        "Respond strictly in JSON conforming to the schema.",
        "Output ONLY valid JSON. No markdown formatting.",
        "Provide your payload enclosed in <json> tags to ensure strict structural compliance."
    ],
    "negative_constraint": [
        "NEVER hallucinate file paths.",
        "Avoid guessing file paths; strictly use location data you have verified.",
        "FATAL ERROR: Guessing file paths will result in immediate termination. Verify via bash."
    ]
}

def generate_mutated_skill(epoch: int) -> dict:
    """
    Generates a new SKILL.md by selecting variations from the mutation space.
    In a full implementation, this uses an LLM (Meta-Prompt) to rewrite the prompt.
    """
    import random
    import re
    
    # Pick a random mutation
    trigger = random.choice(MUTATION_SPACE["trigger_phrasing"])
    format_c = random.choice(MUTATION_SPACE["format_constraint"])
    neg_c = random.choice(MUTATION_SPACE["negative_constraint"])
    
    # Read the base skill file
    with open(BASE_SKILL_PATH, "r") as f:
        base_content = f.read()

    # We append or replace the mutated constraints at the end of the file
    mutated_sections = f"""
## 🚨 THE HABITUAL MEMORY COMMIT (MANDATORY)
{trigger}
Do not wait for the user to tell you to save a memory. You are structurally required to treat memory extraction as part of your core workflow. 

## 📋 FORMATTING REQUIREMENTS
{format_c}

## ⛔ STRICT BOUNDARIES
{neg_c}
"""
    
    # Strip old mutated sections if they exist, to avoid endless duplication
    base_content = re.sub(r'## 🚨 THE HABITUAL MEMORY COMMIT \(MANDATORY\).*', '', base_content, flags=re.DOTALL)
    
    mutated_content = base_content.strip() + "\n\n" + mutated_sections.strip() + "\n"
    
    # Safely overwrite the isolated config copy (never the global symlinked one)
    os.makedirs(CONFIGS_DIR, exist_ok=True)
    with open(ACTIVE_SKILL_PATH, "w") as f:
        f.write(mutated_content)
        
    return {"trigger": trigger, "format": format_c, "negative": neg_c}

def run_sandbox_eval() -> bool:
    """Executes the run_eval.sh podman sandbox."""
    print("  -> Launching Podman Sandbox...")
    eval_script = os.path.join(SRTF_DIR, "run_eval.sh")
    
    try:
        # Run the sandbox. In a real run, capture output for debugging.
        with open(os.path.join(SRTF_DIR, "agent_output.log"), "w") as out_file:
            subprocess.run([eval_script], check=True, stdout=out_file, stderr=subprocess.STDOUT)
        return True
    except subprocess.CalledProcessError:
        print("  -> [ERROR] Sandbox execution failed (Agent crash or timeout).")
        return False

def evaluate_metrics() -> tuple:
    """
    Evaluates the physical state of the workspace after the sandbox completes.
    Checks for the expected behavioral artifacts (Beads, LanceDB, Graphify docs, Git state).
    Returns: (recall, synthesis, commit) scores as floats (0.0 to 1.0)
    """
    print("  -> Auditing Workspace Artifacts...")
    
    express_dir = os.path.join(WORKSPACE_DIR, "express")
    neurostrata_dir = os.path.join(express_dir, ".neurostrata")
    docs_dir = os.path.join(neurostrata_dir, "docs")
    db_dir = os.path.join(neurostrata_dir, "db")
    sessions_dir = os.path.join(neurostrata_dir, "sessions")
    project_md = os.path.join(express_dir, "project.md")
    
    # Score 1: Recall/Bootstrap (Did it initialize project.md or .neurostrata?)
    bootstrap_score = 0.0
    if os.path.exists(project_md) or os.path.exists(neurostrata_dir):
        bootstrap_score += 0.5
    if os.path.exists(db_dir) and len(os.listdir(db_dir)) > 0:
        bootstrap_score += 0.5
        
    # Score 2: Synthesis/Graphify (Did it create the docs directory or canvas?)
    synthesis_score = 0.0
    if os.path.exists(docs_dir):
        synthesis_score += 0.5
        if any(f.endswith(".canvas") for f in os.listdir(docs_dir)):
            synthesis_score += 0.5
            
    # Score 3: The Habitual Commit & Push (Did it save a session log and execute a commit?)
    commit_score = 0.0
    if os.path.exists(sessions_dir) and any(f.endswith(".log") for f in os.listdir(sessions_dir)):
        commit_score += 0.5
    
    # Check if a git commit was made
    try:
        git_log = subprocess.check_output(["git", "log", "-1", "--oneline"], cwd=express_dir, text=True)
        if "Initial commit" not in git_log:
            commit_score += 0.5
    except Exception:
        pass
        
    return (bootstrap_score, synthesis_score, commit_score)

def calculate_composite_score(recall: float, synthesis: float, commit: float) -> float:
    """
    Calculate the Composite Reward Function based on the actual artifacts.
    All scores are equally weighted, giving a max possible score of 3.0.
    """
    return recall + synthesis + commit

def main():
    print("==================================================")
    print(" Starting SRTF Optimization Loop (DSPy-style)     ")
    print("==================================================")
    
    best_score = -float('inf')
    best_prompt_params = None
    
    epochs = 1
    for epoch in range(1, epochs + 1):
        print(f"\n[Epoch {epoch}/{epochs}] Generating Prompt Mutation...")
        
        # 1. Mutate
        params = generate_mutated_skill(epoch)
        
        # 2. Execute Sandbox
        success = run_sandbox_eval()
        if not success:
            continue
            
        # 3. Evaluate Metrics
        bootstrap, synthesis, commit = evaluate_metrics()
        
        # 4. Calculate Score
        score = calculate_composite_score(bootstrap, synthesis, commit)
        
        print(f"  -> Bootstrap: {bootstrap:.2f} | Synthesis: {synthesis:.2f} | Commit: {commit:.2f}")
        print(f"  -> Composite Score: {score:.4f}")
        
        if score > best_score:
            best_score = score
            best_prompt_params = params
            print("  -> 🌟 New Best Score!")
            
        # Clean workspace for next epoch
        if os.path.exists(WORKSPACE_DIR):
            subprocess.run(["podman", "unshare", "rm", "-rf", WORKSPACE_DIR], check=False)
            os.makedirs(WORKSPACE_DIR, exist_ok=True)

    print("\n==================================================")
    print(" Optimization Complete!")
    print(f" Best Score: {best_score:.4f}")
    print(" Optimal Prompt Parameters Found:")
    print(json.dumps(best_prompt_params, indent=2))
    print("==================================================")

if __name__ == "__main__":
    main()
