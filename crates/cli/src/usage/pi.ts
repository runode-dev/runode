// runode: report pi's model and usage to the runode app, shown under the pane.
// Installed by `runode setup usage`; installing again overwrites this file.
// Does nothing outside a runode terminal (no RUNODE_BIN / RUNODE_SESSION).
import { spawn } from "node:child_process";

export default function (pi: any) {
  // `message` is the reply that just ended; it may not be in the session entries yet.
  const report = (ctx: any, message?: any) => {
    const usage = message?.usage;
    const bin = process.env.RUNODE_BIN;
    if (!bin || !process.env.RUNODE_SESSION) return;
    let context: any;
    let cost = usage?.cost?.total ?? 0;
    try {
      context = ctx.getContextUsage?.();
      for (const entry of ctx.sessionManager?.getEntries?.() ?? []) {
        if (entry?.type === "message" && entry.message?.role === "assistant" && entry.message !== message) {
          cost += entry.message.usage?.cost?.total ?? 0;
        }
      }
    } catch {}
    const payload = {
      model: ctx.model?.name ?? ctx.model?.id,
      context_tokens: context?.tokens,
      context_window: context?.contextWindow ?? ctx.model?.contextWindow,
      usage,
      cost,
    };
    try {
      const child = spawn(bin, ["usage-hook", "pi"], { stdio: ["pipe", "ignore", "ignore"] });
      child.on("error", () => {});
      child.stdin.on("error", () => {});
      child.stdin.end(JSON.stringify(payload));
    } catch {}
  };
  pi.on("session_start", (_event: any, ctx: any) => report(ctx));
  pi.on("model_select", (_event: any, ctx: any) => report(ctx));
  pi.on("message_end", (event: any, ctx: any) => {
    if (event.message?.role === "assistant") report(ctx, event.message);
  });
}
