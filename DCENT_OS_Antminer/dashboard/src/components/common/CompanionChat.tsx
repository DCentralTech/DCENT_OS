// CompanionChat — the in-dashboard chat panel for the DCENT_OS companion.
// The device (this dashboard) calls the operator's configured LLM (local by
// default) and can take miner actions via tools. Hardware actions pause for an
// explicit confirmation. This is NOT the daemon MCP server — it's the firmware's
// own companion interface.

import React, { useCallback, useEffect, useRef, useState } from 'react';
import { chat, loadLlmConfig, PROVIDER_PRESETS, isCloud, type ChatTurn, type ToolCall } from '../../companion/llm';
import { TOOL_SPECS, findTool, COMPANION_SYSTEM_PROMPT, type ToolDef } from '../../companion/tools';

interface Msg { id: number; role: 'user' | 'assistant' | 'note'; text: string; }

function toolResult(id: string, content: string): ChatTurn {
  return { role: 'tool', toolCallId: id, content };
}

let MID = 0;

export function CompanionChat() {
  const cfg = loadLlmConfig();
  const [msgs, setMsgs] = useState<Msg[]>([]);
  const [turns, setTurns] = useState<ChatTurn[]>([{ role: 'system', content: COMPANION_SYSTEM_PROMPT }]);
  const [input, setInput] = useState('');
  const [busy, setBusy] = useState(false);
  const [confirm, setConfirm] = useState<{ call: ToolCall; preview: string } | null>(null);
  const pending = useRef<{ working: ChatTurn[]; calls: ToolCall[]; idx: number } | null>(null);
  const scrollRef = useRef<HTMLDivElement>(null);

  const addMsg = useCallback((role: Msg['role'], text: string) => {
    setMsgs(m => [...m, { id: ++MID, role, text }]);
  }, []);

  useEffect(() => {
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [msgs, confirm, busy]);

  const runTool = useCallback(async (tool: ToolDef, call: ToolCall): Promise<ChatTurn> => {
    try {
      const out = await tool.run(call.arguments);
      return toolResult(call.id, out);
    } catch (e) {
      return toolResult(call.id, `Error: ${(e as Error).message}`);
    }
  }, []);

  // Forward declaration via ref so execFrom can re-enter drive.
  const driveRef = useRef<(w: ChatTurn[]) => Promise<void>>(async () => {});

  const execFrom = useCallback(async (working: ChatTurn[], calls: ToolCall[], startIdx: number) => {
    let w = working;
    for (let i = startIdx; i < calls.length; i++) {
      const call = calls[i];
      const tool = findTool(call.name);
      if (!tool) { w = [...w, toolResult(call.id, `Unknown tool: ${call.name}`)]; continue; }
      if (tool.hardware) {
        pending.current = { working: w, calls, idx: i };
        setConfirm({ call, preview: tool.preview(call.arguments) });
        setTurns(w);
        return; // wait for the operator to confirm/decline
      }
      w = [...w, await runTool(tool, call)];
    }
    void driveRef.current(w); // all tool results in — continue the conversation
  }, [runTool]);

  const drive = useCallback(async (working: ChatTurn[]) => {
    let res;
    try {
      res = await chat(cfg, working, TOOL_SPECS);
    } catch (e) {
      addMsg('note', (e as Error).message);
      setBusy(false);
      return;
    }
    const asst: ChatTurn = { role: 'assistant', content: res.content, toolCalls: res.toolCalls };
    const w = [...working, asst];
    setTurns(w);
    if (res.content) addMsg('assistant', res.content);
    if (!res.toolCalls.length) {
      if (!res.content) {
        addMsg('note', 'The model replied with nothing — make sure a chat/instruct model is loaded (some base models return empty completions).');
      }
      setBusy(false);
      return;
    }
    void execFrom(w, res.toolCalls, 0);
  }, [cfg, addMsg, execFrom]);
  driveRef.current = drive;

  const onConfirmRun = useCallback(() => {
    const p = pending.current;
    if (!p) return;
    const call = p.calls[p.idx];
    const tool = findTool(call.name);
    setConfirm(null);
    pending.current = null;
    if (!tool) return;
    addMsg('note', `▶ ${tool.preview(call.arguments)}`);
    void (async () => {
      const w = [...p.working, await runTool(tool, call)];
      void execFrom(w, p.calls, p.idx + 1);
    })();
  }, [addMsg, runTool, execFrom]);

  const onConfirmCancel = useCallback(() => {
    const p = pending.current;
    if (!p) return;
    const call = p.calls[p.idx];
    setConfirm(null);
    pending.current = null;
    addMsg('note', '✕ Declined.');
    const w = [...p.working, toolResult(call.id, 'The operator declined this action; do not retry it.')];
    void execFrom(w, p.calls, p.idx + 1);
  }, [addMsg, execFrom]);

  const send = useCallback(() => {
    const text = input.trim();
    if (!text || busy || confirm) return;
    setInput('');
    addMsg('user', text);
    setBusy(true);
    void drive([...turns, { role: 'user', content: text }]);
  }, [input, busy, confirm, turns, addMsg, drive]);

  if (!cfg.enabled) {
    return (
      <div className="companion-chat companion-chat-off">
        <p className="companion-chat-off-title">Chat with your companion</p>
        <p className="companion-chat-off-body">
          Connect a local LLM (Ollama / LM Studio) — or a cloud provider — in
          <strong> Settings → Companion</strong>, then ask things like
          <em> "lower the noise of my miner"</em> and it gets to work. Local-first and
          off by default; nothing leaves your network until you turn it on.
        </p>
      </div>
    );
  }

  const providerLabel = PROVIDER_PRESETS[cfg.provider].label;

  return (
    <div className="companion-chat">
      <div className="companion-chat-head">
        <span>Companion chat</span>
        <span className={`companion-chat-provider${isCloud(cfg.provider) ? ' is-cloud' : ''}`}>{providerLabel} · {cfg.model}</span>
      </div>
      <div className="companion-chat-log" ref={scrollRef}>
        {msgs.length === 0 && (
          <div className="companion-chat-hint">Ask me about your miner, or tell me what to do — e.g. <em>"make it quieter"</em>.</div>
        )}
        {msgs.map(m => (
          <div key={m.id} className={`companion-chat-msg companion-chat-${m.role}`}>{m.text}</div>
        ))}
        {busy && !confirm && <div className="companion-chat-msg companion-chat-assistant companion-chat-typing">…</div>}
        {confirm && (
          <div className="companion-chat-confirm" role="alertdialog" aria-label="Confirm companion action">
            <div className="companion-chat-confirm-text">The companion wants to: <strong>{confirm.preview}</strong></div>
            <div className="companion-chat-confirm-actions">
              <button type="button" className="companion-chat-btn-run" onClick={onConfirmRun}>Run it</button>
              <button type="button" className="companion-chat-btn-cancel" onClick={onConfirmCancel}>Not now</button>
            </div>
          </div>
        )}
      </div>
      <div className="companion-chat-input">
        <input
          type="text"
          value={input}
          placeholder={confirm ? 'Confirm the action above first…' : 'Message your companion…'}
          disabled={busy || !!confirm}
          onChange={e => setInput(e.target.value)}
          onKeyDown={e => { if (e.key === 'Enter') send(); }}
          aria-label="Message your companion"
        />
        <button type="button" onClick={send} disabled={busy || !!confirm || !input.trim()} aria-label="Send">↑</button>
      </div>
    </div>
  );
}

export default CompanionChat;
