import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { useEffect, useMemo, useRef, useState } from "react";

type TranslationEntry = {
  id: number;
  reason: string;
  text: string;
  done: boolean;
  startedAt: number;
  inputChars?: number;
};

type DonePayload = {
  translation?: string;
  meta?: {
    provider?: string;
    model?: string;
    latency_ms?: number;
    latencyMs?: number;
  };
};

export default function App() {
  const [entries, setEntries] = useState<TranslationEntry[]>([]);
  const [activeId, setActiveId] = useState<number | null>(null);
  const [status, setStatus] = useState("等待会话...");
  const activeIdRef = useRef<number | null>(null);

  useEffect(() => {
    const unsubs: Promise<UnlistenFn>[] = [];

    unsubs.push(
      listen("session.started", () => {
        setStatus("会话已启动");
      })
    );

    unsubs.push(
      listen<{ reason?: string; selectedChars?: number }>("translation.started", (event) => {
        const id = Date.now();
        activeIdRef.current = id;
        setActiveId(id);
        setEntries((prev) => [
          {
            id,
            reason: event.payload?.reason ?? "unknown",
            text: "",
            done: false,
            startedAt: Date.now(),
            inputChars: event.payload?.selectedChars,
          },
          ...prev,
        ]);
        setStatus("翻译中...");
      })
    );

    unsubs.push(
      listen<{ delta?: string }>("translation.delta", (event) => {
        const delta = event.payload?.delta ?? "";
        if (!delta) {
          return;
        }

        const currentId = activeIdRef.current;
        if (currentId === null) {
          return;
        }

        setEntries((prev) => prev.map((entry) => (entry.id === currentId ? { ...entry, text: `${entry.text}${delta}` } : entry)));
      })
    );

    unsubs.push(
      listen<DonePayload>("translation.done", (event) => {
        const currentId = activeIdRef.current;
        if (currentId === null) {
          return;
        }

        const translation = event.payload?.translation;
        setEntries((prev) =>
          prev.map((entry) => {
            if (entry.id !== currentId) {
              return entry;
            }

            const nextText = translation ?? entry.text;
            return {
              ...entry,
              text: nextText,
              done: true,
            };
          })
        );
        activeIdRef.current = null;
        setStatus("已完成");
      })
    );

    unsubs.push(
      listen<{ message?: string }>("session.error", (event) => {
        setStatus(`错误: ${event.payload?.message ?? "unknown"}`);
      })
    );

    unsubs.push(
      listen("session.stopped", () => {
        setStatus("会话已停止");
      })
    );

    return () => {
      void Promise.all(unsubs).then((handlers) => {
        handlers.forEach((off) => off());
      });
    };
  }, []);

  const activeEntry = useMemo(
    () => entries.find((entry) => entry.id === activeId) ?? entries[0],
    [entries, activeId]
  );

  async function stopSession() {
    await invoke("request_stop");
  }

  return (
    <div className="panel">
      <header className="panel-header">
        <div>
          <h1>tetr</h1>
          <p>{status}</p>
        </div>
        <button className="stop-btn" onClick={stopSession}>
          结束会话
        </button>
      </header>

      <section className="active-translation">
        <h2>实时翻译</h2>
        <div className="translation-text">{activeEntry?.text || "等待命令输出..."}</div>
      </section>

      <section className="history">
        <h3>最近记录</h3>
        <ul>
          {entries.slice(0, 5).map((entry) => (
            <li key={entry.id}>
              <div className="meta-row">
                <span>{entry.reason}</span>
                <span>{entry.done ? "done" : "streaming"}</span>
              </div>
              <div className="history-text">{entry.text || "..."}</div>
            </li>
          ))}
        </ul>
      </section>
    </div>
  );
}
