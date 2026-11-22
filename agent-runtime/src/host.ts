// The sidecar's only door to the world: every tool call and declared hook is a
// round trip to the host, which runs admission, gates and the ledger.
import type { HookPoint, HookResultMessage, HostMessage, SidecarMessage, ToolResultMessage } from "./protocol.ts";

export interface HostLink {
  send(message: SidecarMessage): void;
}

export interface HookOutcome {
  allow: boolean;
  reason?: string;
}

export class HostChannel {
  private nextId = 0;
  private readonly tools = new Map<string, (result: ToolResultMessage) => void>();
  private readonly hooks = new Map<string, (result: HookResultMessage) => void>();
  private cancelled = false;
  private readonly onCancel: Array<() => void> = [];
  private terminal = false;

  private readonly link: HostLink;
  private readonly declared: ReadonlySet<HookPoint>;

  constructor(link: HostLink, declared: ReadonlySet<HookPoint>) {
    this.link = link;
    this.declared = declared;
  }

  /** Whether the host admitted a finish. */
  get finished(): boolean {
    return this.terminal;
  }

  get isCancelled(): boolean {
    return this.cancelled;
  }

  whenCancelled(callback: () => void): void {
    if (this.cancelled) callback();
    else this.onCancel.push(callback);
  }

  /** Routes one host message to the waiter it answers. */
  receive(message: HostMessage): void {
    if (message.type === "tool_result") {
      const waiter = this.tools.get(message.call_id);
      if (!waiter) throw new Error("tool_result for an unknown call");
      this.tools.delete(message.call_id);
      if (message.terminal) this.terminal = true;
      waiter(message);
    } else if (message.type === "hook_result") {
      const waiter = this.hooks.get(message.hook_id);
      if (!waiter) throw new Error("hook_result for an unknown hook");
      this.hooks.delete(message.hook_id);
      waiter(message);
    } else if (message.type === "cancel") {
      this.cancelled = true;
      for (const callback of this.onCancel.splice(0)) callback();
      const cancelled: ToolResultMessage = {
        type: "tool_result",
        call_id: "",
        content: "Run cancelled by the host.",
        is_error: true,
        terminal: true,
      };
      for (const [id, waiter] of this.tools) waiter({ ...cancelled, call_id: id });
      this.tools.clear();
      for (const [id, waiter] of this.hooks) waiter({ type: "hook_result", hook_id: id, decision: "deny", reason: "cancelled" });
      this.hooks.clear();
    } else {
      throw new Error("unexpected host message during a run");
    }
  }

  /** Executes one tool call on the host. The host's answer is final. */
  callTool(name: string, args: unknown): Promise<ToolResultMessage> {
    if (this.cancelled) {
      return Promise.resolve({ type: "tool_result", call_id: "", content: "Run cancelled by the host.", is_error: true, terminal: true });
    }
    const callId = `call-${++this.nextId}`;
    return new Promise((resolve) => {
      this.tools.set(callId, resolve);
      this.link.send({ type: "tool_call", call_id: callId, name, arguments: args });
    });
  }

  /** Runs a declared hook on the host; an undeclared point is a no-op allow. */
  async hook(
    point: HookPoint,
    detail: { tool?: string; arguments?: unknown; result?: { content: string; is_error: boolean } } = {},
  ): Promise<HookOutcome> {
    if (!this.declared.has(point)) return { allow: true };
    if (this.cancelled) return { allow: false, reason: "cancelled" };
    const hookId = `hook-${++this.nextId}`;
    const result = await new Promise<HookResultMessage>((resolve) => {
      this.hooks.set(hookId, resolve);
      this.link.send({ type: "hook", hook_id: hookId, point, ...detail });
    });
    return { allow: result.decision === "allow", reason: result.reason };
  }
}
