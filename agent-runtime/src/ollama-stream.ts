// Ollama's native /api/chat for the Pi lane. The OpenAI-compatible endpoint
// cannot carry `num_ctx`, and a silently default context window truncated the
// 2026-11-22 D1/D2 prompts; the native API sends the frozen window (ADR-0081)
// on every request, and the host proxy checks it.
import { createAssistantMessageEventStream, type AssistantMessage, type Context, type Model } from "@mariozechner/pi-ai";

interface OllamaMessage {
  role: "system" | "user" | "assistant" | "tool";
  content: string;
  tool_calls?: Array<{ function: { name: string; arguments: Record<string, unknown> } }>;
  tool_name?: string;
}

function text(content: unknown): string {
  if (typeof content === "string") return content;
  if (!Array.isArray(content)) return "";
  return content.map((part: any) => (part?.type === "text" ? part.text : "")).join("");
}

/** Pi context -> Ollama native chat messages. Images and thinking are dropped. */
export function ollamaMessages(context: Context): OllamaMessage[] {
  const out: OllamaMessage[] = [];
  if (context.systemPrompt) out.push({ role: "system", content: context.systemPrompt });
  for (const message of context.messages as any[]) {
    if (message.role === "user") out.push({ role: "user", content: text(message.content) });
    else if (message.role === "assistant") {
      const calls = (message.content as any[]).filter((part) => part.type === "toolCall");
      out.push({
        role: "assistant",
        content: text(message.content),
        ...(calls.length ? { tool_calls: calls.map((call) => ({ function: { name: call.name, arguments: call.arguments } })) } : {}),
      });
    } else if (message.role === "toolResult") {
      out.push({ role: "tool", tool_name: message.toolName, content: text(message.content) });
    }
  }
  return out;
}

export function ollamaRequest(model: Model<any>, context: Context, contextWindow: number | undefined) {
  return {
    model: model.id,
    messages: ollamaMessages(context),
    tools: (context.tools ?? []).map((tool) => ({
      type: "function",
      function: { name: tool.name, description: tool.description, parameters: tool.parameters },
    })),
    stream: true,
    think: false,
    options: {
      ...(contextWindow ? { num_ctx: contextWindow } : {}),
      num_predict: model.maxTokens,
    },
  };
}

/** A Pi StreamFn over Ollama's native chat API, reached through the host proxy. */
export function ollamaStreamFn(baseUrl: string, token: string, contextWindow: number | undefined) {
  return (model: Model<any>, context: Context, options?: { signal?: AbortSignal }) => {
    const stream = createAssistantMessageEventStream();
    const message: AssistantMessage = {
      role: "assistant",
      content: [],
      api: model.api,
      provider: model.provider,
      model: model.id,
      usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } },
      stopReason: "stop",
      timestamp: Date.now(),
    };
    (async () => {
      try {
        const response = await fetch(`${baseUrl.replace(/\/$/, "")}/api/chat`, {
          method: "POST",
          headers: { "content-type": "application/json", authorization: `Bearer ${token}` },
          body: JSON.stringify(ollamaRequest(model, context, contextWindow)),
          signal: options?.signal,
        });
        if (!response.ok || !response.body) throw new Error(`model proxy answered ${response.status}`);
        stream.push({ type: "start", partial: message });
        let body = "";
        let content = "";
        const calls: Array<{ name: string; arguments: Record<string, unknown> }> = [];
        const decoder = new TextDecoder();
        for await (const chunk of response.body as any as AsyncIterable<Uint8Array>) {
          body += decoder.decode(chunk, { stream: true });
          let newline;
          while ((newline = body.indexOf("\n")) >= 0) {
            const line = body.slice(0, newline).trim();
            body = body.slice(newline + 1);
            if (!line) continue;
            const event = JSON.parse(line);
            if (event.error) throw new Error(String(event.error));
            content += event.message?.content ?? "";
            for (const call of event.message?.tool_calls ?? []) {
              calls.push({ name: call.function?.name, arguments: call.function?.arguments ?? {} });
            }
            if (event.done) {
              message.usage.input = event.prompt_eval_count ?? 0;
              message.usage.output = event.eval_count ?? 0;
              message.usage.totalTokens = message.usage.input + message.usage.output;
              if (event.done_reason === "length") message.stopReason = "length";
            }
          }
        }
        if (content) message.content.push({ type: "text", text: content });
        calls.forEach((call, index) =>
          message.content.push({ type: "toolCall", id: `call_${index}_${Date.now()}`, name: call.name, arguments: call.arguments }),
        );
        if (calls.length) message.stopReason = "toolUse";
        stream.push({ type: "done", reason: message.stopReason as "stop" | "length" | "toolUse", message });
      } catch (error) {
        message.stopReason = options?.signal?.aborted ? "aborted" : "error";
        message.errorMessage = String(error);
        stream.push({ type: "error", reason: message.stopReason as "aborted" | "error", error: message });
      }
    })();
    return stream;
  };
}
