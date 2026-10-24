/**
 * NeuroStrata OpenClaw Plugin — Client Shim
 *
 * Implements:
 * 1. Pre-edit guard validation (calls neurostrata_guard_validate before file ops)
 * 2. Turn-level task-intent detection (runs detect_task_intents, injects nudge)
 */

// ──────────────────────────────────────────────────────────────────────
// Types (mirror MCP tool signatures & OpenClaw plugin API)
// ──────────────────────────────────────────────────────────────────────

interface GuardValidateParams {
  action_type: string;
  payload: string;
  cwd: string;
  use_sandbox?: boolean;
}

interface GuardValidateResult {
  verdict: "pass" | "reject";
  reasons: string[];
  constraints: string[];
  rule_ids: string[];
}

interface TaskIntentSignal {
  marker: string;
  excerpt: string;
}

interface DetectIntentsParams {
  text: string;
}

interface DetectIntentsResult {
  signals: TaskIntentSignal[];
}

interface TaskCreateParams {
  namespace: string;
  title: string;
  description?: string;
  priority?: number;
  labels?: string[];
  blocked_by?: string[];
  parent_id?: string;
}

// OpenClaw plugin context (hypothetical)
interface OpenClawPluginContext {
  config: Record<string, unknown>;
  logger: { info: (msg: string, meta?: Record<string, unknown>) => void; warn: (msg: string, meta?: Record<string, unknown>) => void; error: (msg: string, meta?: Record<string, unknown>) => void };
  hooks: {
    beforeFileOp: (handler: FileOpHandler) => void;
    onTurnEnd: (handler: TurnEndHandler) => void;
  };
  conversation: {
    injectSystemMessage: (message: string) => void;
  };
}

type FileOpHandler = (op: FileOperation) => Promise<FileOpResult>;
type TurnEndHandler = (turn: ConversationTurn) => Promise<void>;

interface FileOperation {
  type: "write" | "edit" | "delete" | "mkdir";
  path: string;
  content?: string;
  oldContent?: string;
  newContent?: string;
}

interface FileOpResult {
  allowed: boolean;
  reason?: string;
  guardVerdict?: GuardValidateResult;
}

interface ConversationTurn {
  role: "user" | "assistant" | "system";
  content: string;
}

// ──────────────────────────────────────────────────────────────────────
// Configuration
// ──────────────────────────────────────────────────────────────────────

interface PluginConfig {
  namespace: string;
  guardEndpoint: string;
  strictMode: boolean;
  intentDetection: {
    enabled: boolean;
    injectReminder: boolean;
  };
}

const DEFAULT_CONFIG: PluginConfig = {
  namespace: "NeuroStrata",
  guardEndpoint: "http://localhost:8080/mcp",
  strictMode: true,
  intentDetection: { enabled: true, injectReminder: true },
};

// ──────────────────────────────────────────────────────────────────────
// MCP Client Helper
// ──────────────────────────────────────────────────────────────────────

class NeuroStrataMCPClient {
  private endpoint: string;
  private requestId = 0;

  constructor(endpoint: string) {
    this.endpoint = endpoint;
  }

  private async call<T>(method: string, params: unknown): Promise<T> {
    const id = ++this.requestId;
    const response = await fetch(this.endpoint, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ jsonrpc: "2.0", id, method, params }),
    });
    const data = await response.json();
    if (data.error) throw new Error(data.error.message);
    return data.result as T;
  }

  async guardValidate(params: GuardValidateParams): Promise<GuardValidateResult> {
    const result = await this.call<{
      verdict: ValidateVerdict;
      rule_ids: string[];
    }>("neurostrata_guard_validate", params);
    return this.mapVerdict(result.verdict, result.rule_ids);
  }

  async detectIntents(params: DetectIntentsParams): Promise<DetectIntentsResult> {
    return this.call<DetectIntentsResult>("neurostrata_detect_task_intents", params);
  }

  async taskCreate(params: TaskCreateParams): Promise<{ id: string }> {
    return this.call<{ id: string }>("neurostrata_task_create", params);
  }

  private mapVerdict(verdict: ValidateVerdict, rule_ids: string[]): GuardValidateResult {
    if (verdict.type === "DeterministicReject") {
      return { verdict: "reject", reasons: verdict.reasons, constraints: verdict.constraints, rule_ids };
    }
    if (verdict.type === "SandboxPassHighFidelity") {
      return { verdict: "pass", reasons: verdict.notes, constraints: [], rule_ids };
    }
    return { verdict: "reject", reasons: ["Unknown verdict"], constraints: [], rule_ids };
  }
}

type ValidateVerdict =
  | { type: "DeterministicReject"; reasons: string[]; constraints: string[] }
  | { type: "SandboxPassHighFidelity"; notes: string[] }
  | { type: "SandboxReject"; reasons: string[]; constraints: string[] };

// ──────────────────────────────────────────────────────────────────────
// Intent Detection (local fallback — mirrors src/task/intent.rs)
// ──────────────────────────────────────────────────────────────────────

const MARKERS: readonly [string, string][] = [
  ["todo:", "todo:"],
  ["fixme:", "fixme:"],
  ["follow-up:", "follow-up:"],
  ["followup:", "followup:"],
  ["next step:", "next step:"],
  ["action item:", "action item:"],
  ["remaining work:", "remaining work:"],
  ["we should also ", "we should also"],
  ["later we need to ", "later we need to"],
  ["need to follow up", "need to follow up"],
  ["still need to ", "still need to"],
  ["must also fix ", "must also fix"],
  ["filed follow-up", "filed follow-up"],
  ["defer ", "defer"],
  ["deferred ", "deferred"],
];

const NEGATION_PREFIXES = ["never ", "do not ", "don't ", "do not, "];

function detectTaskIntentsLocal(text: string): TaskIntentSignal[] {
  const lower = text.toLowerCase();
  const out: TaskIntentSignal[] = [];
  const seen = new Set<string>();

  for (const [matchStr, canonical] of MARKERS) {
    const idx = lower.indexOf(matchStr);
    if (idx === -1) continue;
    if (seen.has(canonical)) continue;

    // negation guard
    const clauseStart = text.slice(0, idx).lastIndexOf("\n") + 1;
    let start = clauseStart;
    const sentPos = text.slice(0, idx).lastIndexOf(". ");
    if (sentPos !== -1 && sentPos + 2 > start) start = sentPos + 2;
    const clausePrefix = text.slice(start, idx).trimStart().toLowerCase();
    if (NEGATION_PREFIXES.some((p) => clausePrefix.startsWith(p))) continue;

    // excerpt
    const from = idx;
    const rest = text.slice(from);
    const end = Math.min(rest.indexOf("\n") === -1 ? rest.length : rest.indexOf("\n"), 160);
    let excerpt = rest.slice(0, end).trimEnd();
    if (excerpt.length > 160) excerpt = excerpt.slice(0, 160);

    seen.add(canonical);
    out.push({ marker: canonical, excerpt });
  }
  return out;
}

// ──────────────────────────────────────────────────────────────────────
// Plugin Entry Point
// ──────────────────────────────────────────────────────────────────────

let config: PluginConfig = DEFAULT_CONFIG;
let mcpClient: NeuroStrataMCPClient;

export async function initialize(ctx: OpenClawPluginContext): Promise<void> {
  config = { ...DEFAULT_CONFIG, ...(ctx.config as Record<string, unknown>) } as PluginConfig;
  mcpClient = new NeuroStrataMCPClient(config.guardEndpoint);

  // Register file operation hook
  ctx.hooks.beforeFileOp(wrapFileOp);

  // Register turn hook for intent detection
  ctx.hooks.onTurnEnd(onTurnEnd);

  ctx.logger.info("NeuroStrata OpenClaw plugin initialized", { namespace: config.namespace });
}

// ──────────────────────────────────────────────────────────────────────
// File Op Hook: guard_validate before write/edit/delete
// ──────────────────────────────────────────────────────────────────────

async function wrapFileOp(op: FileOperation): Promise<FileOpResult> {
  const actionType = op.type; // "write" | "edit" | "delete" | "mkdir"
  const payload = JSON.stringify({ path: op.path, content: op.content, oldContent: op.oldContent, newContent: op.newContent });
  const cwd = process.cwd();

  if (config.strictMode) {
    try {
      const result = await mcpClient.guardValidate({
        action_type: actionType,
        payload,
        cwd,
        use_sandbox: false,
      });

      if (result.verdict === "reject") {
        const reason = result.reasons.join("; ");
        ctx.logger.warn("Guard rejected file operation", { actionType, reason, rule_ids: result.rule_ids });
        return {
          allowed: false,
          reason: `Zero-Action Start violation: ${reason}`,
          guardVerdict: result,
        };
      }
    } catch (err) {
      ctx.logger.error("Guard validation failed", { error: String(err) });
      if (config.strictMode) {
        return {
          allowed: false,
          reason: `Guard validation unavailable: ${err}`,
        };
      }
    }
  }

  return { allowed: true };
}

// ──────────────────────────────────────────────────────────────────────
// Turn Hook: intent detection + nudge
// ──────────────────────────────────────────────────────────────────────

async function onTurnEnd(turn: ConversationTurn): Promise<void> {
  if (!config.intentDetection.enabled) return;
  if (turn.role === "system") return; // don't re-process our own nudges

  const text = turn.content;
  const hasTaskCreate = text.includes("neurostrata_task_create");

  let signals: TaskIntentSignal[];
  try {
    const result = await mcpClient.detectIntents({ text });
    signals = result.signals;
  } catch {
    signals = detectTaskIntentsLocal(text);
  }

  if (signals.length > 0 && !hasTaskCreate && config.intentDetection.injectReminder) {
    const markerList = signals.map((s) => s.marker).join(", ");
    const nudge = `[NeuroStrata] Task intent detected (${markerList}) but no neurostrata_task_create in this turn. Consider creating a task to track this work.`;
    ctx.conversation.injectSystemMessage(nudge);
  }
}

// ──────────────────────────────────────────────────────────────────────
// Re-export for testing
// ──────────────────────────────────────────────────────────────────────

export { detectTaskIntentsLocal, MARKERS, NEGATION_PREFIXES };
export type { PluginConfig, GuardValidateResult, TaskIntentSignal, FileOperation, FileOpResult };