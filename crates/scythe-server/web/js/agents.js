// The AI agent light in the editor's top bar and its panel: which agents are connected over MCP,
// what they say they are doing, their recent tool calls, and undo / pause / disconnect. Fed by
// the admin-only `agent_activity` event and `agents.activity`.

import { html, useEffect, useState } from "../vendor/preact-htm.js";
import { agentLight, agoText } from "./domains.js";
import { useStoreValue } from "./store.js";
import { Modal } from "./ui.js";

// Re-renders while the light can change by itself (a busy or error spell running out).
function useTicking(active) {
  const [, setNow] = useState(0);
  useEffect(() => {
    if (!active) return;
    const timer = setInterval(() => setNow(Date.now()), 500);
    return () => clearInterval(timer);
  }, [active]);
}

export function AgentDot({ store, onOpen }) {
  const activity = useStoreValue(store, "agents");
  const light = agentLight(activity, store.agentMarks, Date.now());
  useTicking(light === "busy" || light === "error");
  if (light === "off") return null;
  const names = activity.clients.map((c) => c.name).join(", ");
  const lines = [`Agent: ${names}`, activity.status, activity.paused && "Paused: changes are refused", "Click for details"];
  return html`<button class=${`agent-dot ${light} ${activity.paused ? "paused" : ""}`} title=${lines.filter(Boolean).join("\n")}
    aria-label=${`Agent activity (${light === "busy" ? "working" : light})`} onClick=${onOpen}><span></span></button>`;
}

function CallRow({ call, now, several }) {
  const state = call.ok == null ? "running" : call.ok ? "done" : "failed";
  const took = call.duration_ms != null ? ` in ${call.duration_ms} ms` : "";
  return html`<li class=${`agent-call ${state}`} title=${`${call.tool} · ${state}${took}`}>
    <span class="when mono">${agoText(call.started_ms, now)}</span>
    <span class="mark"></span>
    <div class="what">
      <div>${several ? `${call.agent}: ` : ""}${call.summary}</div>
      ${call.error && html`<div class="error">${call.error}</div>`}
      ${call.undo?.length > 0 && html`<div class="dim">Undo: ${call.undo.join(", ")}</div>`}
    </div>
  </li>`;
}

export function AgentsDialog({ store, onClose }) {
  const activity = useStoreValue(store, "agents");
  const [calls, setCalls] = useState(null);
  useTicking(true);
  useEffect(() => {
    store.run({ cmd: "agents.activity" }).then((a) => setCalls(a.calls), () => {});
  }, [store, activity?.calls?.at(-1)?.id]);
  const now = Date.now();
  const run = (command, message) => store.run(command).then(() => message && store.notify(message), () => {});
  const clients = activity?.clients ?? [];
  const list = [...(calls ?? activity?.calls ?? [])].reverse();
  const paused = !!activity?.paused;
  return html`<${Modal} title="Agents" onClose=${onClose} wide cls="agents-modal">
    ${clients.length === 0 && html`<p class="dim">No agent is connected.</p>`}
    ${clients.map(
      (c) => html`<div class="agent-client" key=${c.id}>
        <div class="agent-head">
          <strong>${c.name}</strong>
          <span class="dim">${c.version ? ` ${c.version}` : ""} · ${c.transport} · connected ${agoText(c.connected_ms, now) === "now" ? "just now" : `${agoText(c.connected_ms, now)} ago`}${c.running > 0 ? " · working" : ""}</span>
          <button class="btn small" onClick=${() => run({ cmd: "agents.disconnect", client: c.id }, "Agent disconnected")}>Disconnect</button>
        </div>
        ${c.status && html`<div class="agent-status">${c.status}</div>`}
      </div>`,
    )}
    ${paused && html`<p class="agent-paused">Paused: agents can read the project, their changes are refused.</p>`}
    <div class="agent-actions">
      <button class="btn" disabled=${!activity?.undo} title=${activity?.undo ? `Undo ${activity.undo.label} (${activity.undo.agent})` : "The last undo step is not an agent's"}
        onClick=${() => run({ cmd: "agents.undo" })}>Undo last agent change</button>
      <button class="btn" onClick=${() => run({ cmd: "agents.pause", paused: !paused }, paused ? "Agents resumed" : "Agents paused")}>
        ${paused ? "Resume agents" : "Pause agents"}</button>
      <button class="btn" disabled=${clients.length === 0} onClick=${() => run({ cmd: "agents.disconnect" }, "Agents disconnected")}>Disconnect all</button>
    </div>
    <h3 class="agent-title">Activity</h3>
    ${list.length === 0
      ? html`<p class="dim">No tool calls yet.</p>`
      : html`<ul class="agent-calls">${list.map((call) => html`<${CallRow} key=${call.id} call=${call} now=${now} several=${new Set(clients.map((c) => c.name)).size > 1} />`)}</ul>`}
  <//>`;
}
