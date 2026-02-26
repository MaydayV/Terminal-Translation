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

type SessionStartedPayload = {
  provider?: string;
};

type DonePayload = {
  translation?: string;
};

export default function App() {
  const [entries, setEntries] = useState<TranslationEntry[]>([]);
  const [activeId, setActiveId] = useState<number | null>(null);
  const [status, setStatus] = useState("等待会话...");
  const [provider, setProvider] = useState("-");
  const activeIdRef = useRef<number | null>(null);

  useEffect(() => {
    const unsubs: Promise<UnlistenFn>[] = [];

    unsubs.push(
      listen<SessionStartedPayload>("session.started", (event) => {
        setProvider(event.payload?.provider ?? "-");
        setStatus("会话已启动");
      })
    );

    unsubs.push(
      listen<{ reason?: string; selectedChars?: number }>(
        "translation.started",
        (event) => {
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
        }
      )
    );

    unsubs.push(
      listen<{ delta?: string }>("translation.delta", (event) => {
        const delta = event.payload?.delta ?? "";
        const currentId = activeIdRef.current;
        if (!delta || currentId === null) {
          return;
        }

        setEntries((prev) =>
          prev.map((entry) =>
            entry.id === currentId
              ? { ...entry, text: `${entry.text}${delta}` }
              : entry
          )
        );
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

            return {
              ...entry,
              text: translation ?? entry.text,
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
        <div className="header-left">
          <h1>tetr 实时翻译</h1>
          <p className="status-text">{status}</p>
        </div>

        <div className="header-right">
          <span className="provider-pill">{provider}</span>
          <button className="stop-btn" onClick={stopSession}>
            结束
          </button>
        </div>
      </header>

      <section className="active-translation">
        <div className="section-title">实时翻译</div>
        <div className="translation-text">
          {activeEntry?.text || "等待命令输出..."}
        </div>
      </section>

      <section className="history">
        <div className="section-title">最近记录</div>
        <ul>
          {entries.slice(0, 4).map((entry) => (
            <li key={entry.id}>
              <div className="meta-row">
                <span>{entry.reason}</span>
                <span>{entry.done ? "已完成" : "流式中"}</span>
              </div>
              <div className="history-text">{entry.text || "..."}</div>
            </li>
          ))}
        </ul>
      </section>
    </div>
  );
}
