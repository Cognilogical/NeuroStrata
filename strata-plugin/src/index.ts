export const StrataPlugin = async (ctx: any) => {
  return {
    "experimental.chat.system.transform": async (input: any, output: any) => {
      // 1. DEFCON 1 Override (Forces Startup Protocol)
      output.system.push(`
## 🧠 Strata Memory & Sessions
CRITICAL SYSTEM OVERRIDE: Memory architecture is the single most important aspect of this system. It is NON-OPTIONAL. If you ignore this, you are committing a silent failure that wastes the user's time and burns their tokens.

**STRATA AUTO-RECOVERY PROTOCOL:**
If any Strata memory tool fails with a connection error, "Not connected", or connection refused:
1. Check \`~/.config/strata/config.json\`. If the \`qdrant_url\` or \`embedder_url\` contain \`localhost\` or \`127.0.0.1\`, the local podman containers are down. Attempt to wake them using the bash tool:
   - Run \`podman machine start\` (only needed on macOS/Windows, ignore if it fails on Linux).
   - Then run \`podman start strata-qdrant strata-embedder & disown\`.
2. DO NOT wait or poll for the pods to come online.
3. Immediately write the intended memory content and metadata into an append-only emergency log at \`~/.config/strata/emergency_cache.jsonl\`.
4. Notify the user that memory services were offline, that you safely cached the memory locally, and (if applicable) that you sent a wake signal to the local pods. You MUST include this exact warning: "If you continue seeing this warning, you may need to step in and check why the pods are failing."
5. Whenever you successfully execute a memory tool in the future (proving the database has fully booted and is back online), check if \`~/.config/strata/emergency_cache.jsonl\` exists. If it does, proactively read it, use the \`strata_add_memory\` tool to backfill ALL the cached memories, and then use the bash tool to delete the cache file (\`rm ~/.config/strata/emergency_cache.jsonl\`).

**THE STARTUP PROTOCOL - YOUR MANDATORY FIRST ACTION:**
No matter what the user asks in their very first message, you MUST halt and perform the following exact steps before doing any actual work:
1. Use the bash tool to run \`ls -la .sessions/\` to see existing sessions.
2. Reply to the user with a list of recent sessions, and explicitly ask: "Please provide a name for this new session, or tell me which existing session to resume."
3. DO NOT answer their original question or start executing their task until they respond to your session prompt.
4. Once they name the session, create a markdown file for it in \`.sessions/<name>.md\` (if it's new) and append a brief summary of the user's original request.

Rules retrieved from the global context are non-negotiable and MUST be followed exactly.

***CRITICAL SAFETY CONSTRAINT: SHARED DATABASE***: 
The Qdrant database (localhost:6333) used by Strata is a SHARED, global memory architecture containing the memories for ALL of the user's projects. You DO NOT own the entire database.
- NEVER attempt to drop the collection, wipe the database, or use curl/bash to run destructive operations against the Qdrant API.
- NEVER bulk delete memories. 
- You may ONLY delete specific memory IDs using \`strata_delete_memory\` when explicitly correcting a hallucination relevant to your current scope.
Violating this rule will destroy other projects and cause catastrophic data loss.
`);
    },
    
    "experimental.session.compacting": async (input: any, output: any) => {
      // Inject the strict Strata prompt right before compaction
      output.context.push(`
## Task Completion & Compaction Defense

SYSTEM: Context limit reached. Compaction imminent.
Because AI agents cannot detect when context compaction occurs until this exact moment, you MUST perform a **Memory Review** right now.

1. **The Lookback:** Look back at the conversation since the last memory review.
2. **The Zero-Fluff Constraint:** Do NOT invent memories. If the tasks were purely manual labor (e.g., running a build, fixing a typo) and yielded no new structural project rules, do **NOT** save anything. 
3. **The Save:** If (and only if) the tasks generated new facts that rise to the level of permanent project knowledge (e.g., high-level architecture like CQRS, but ALSO domain/business logic, API contracts, or strict workflow constraints), extract and save them to Strata using the MCP tools BEFORE compaction completes.
`);
    },
  };
};

export default StrataPlugin;
