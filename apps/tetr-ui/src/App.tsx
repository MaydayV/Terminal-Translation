import { invoke } from "@tauri-apps/api/core";
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
};

type UiEnvelope = {
  event: string;
  payload?: any;
};

const HISTORY_STORAGE_KEY = "tetr.ui.history.enabled";
const HISTORY_DEFAULT_ENABLED = true; // Debug default: keep history open for now.

export default function App() {
  const [entries, setEntries] = useState<TranslationEntry[]>([]);
  const [activeId, setActiveId] = useState<number | null>(null);
  const [status, setStatus] = useState("等待会话...");
  const [historyEnabled, setHistoryEnabled] = useState<boolean>(() => {
    try {
      const stored = window.localStorage.getItem(HISTORY_STORAGE_KEY);
      if (stored === null) {
        return HISTORY_DEFAULT_ENABLED;
      }
      return stored === "1";
    } catch {
      return HISTORY_DEFAULT_ENABLED;
    }
  });
  const activeIdRef = useRef<number | null>(null);
  const readySentRef = useRef(false);

  useEffect(() => {
    let pollTimer: number | undefined;
    let inFlight = false;
    let stopped = false;

    const applyEnvelope = (envelope: UiEnvelope) => {
      const payload = envelope.payload ?? {};

      switch (envelope.event) {
        case "session.started": {
          setStatus("会话已启动");
          break;
        }
        case "translation.started": {
          const data = payload as { reason?: string; selectedChars?: number };
          const id = Date.now();
          activeIdRef.current = id;
          setActiveId(id);
          setEntries((prev) => [
            {
              id,
              reason: data.reason ?? "unknown",
              text: "",
              done: false,
              startedAt: Date.now(),
              inputChars: data.selectedChars,
            },
            ...prev,
          ]);
          setStatus("翻译中...");
          break;
        }
        case "translation.delta": {
          const delta = (payload as { delta?: string }).delta ?? "";
          const currentId = activeIdRef.current;
          if (!delta || currentId === null) {
            break;
          }

          setEntries((prev) =>
            prev.map((entry) =>
              entry.id === currentId
                ? { ...entry, text: `${entry.text}${delta}` }
                : entry
            )
          );
          break;
        }
        case "translation.done": {
          const currentId = activeIdRef.current;
          if (currentId === null) {
            break;
          }

          const data = payload as DonePayload;
          const translation = data.translation;
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
          break;
        }
        case "session.error": {
          const message = (payload as { message?: string }).message ?? "unknown";
          setStatus(`错误: ${message}`);
          break;
        }
        case "session.stopped": {
          setStatus("会话已停止");
          break;
        }
        default:
          break;
      }
    };

    const poll = async () => {
      if (stopped || inFlight) {
        return;
      }

      inFlight = true;
      try {
        const events = await invoke<UiEnvelope[]>("drain_events");
        for (const envelope of events) {
          applyEnvelope(envelope);
        }
      } catch (error) {
        console.warn("[tetr-ui] drain_events failed", error);
      } finally {
        inFlight = false;
      }
    };

    if (!readySentRef.current) {
      readySentRef.current = true;
      void invoke("frontend_ready").catch(() => {
        setStatus("错误: 前端握手失败");
      });
    }

    void poll();
    pollTimer = window.setInterval(() => {
      void poll();
    }, 120);

    return () => {
      stopped = true;
      if (pollTimer !== undefined) {
        window.clearInterval(pollTimer);
      }
    };
  }, []);

  const activeEntry = useMemo(
    () => entries.find((entry) => entry.id === activeId) ?? entries[0],
    [entries, activeId]
  );

  useEffect(() => {
    try {
      window.localStorage.setItem(
        HISTORY_STORAGE_KEY,
        historyEnabled ? "1" : "0"
      );
    } catch {
      // Ignore storage failures in restricted environments.
    }
  }, [historyEnabled]);

  return (
    <div className={`panel ${historyEnabled ? "with-history" : "without-history"}`}>
      <header className="panel-header">
        <div className="header-left">
          <p className="status-text">{status}</p>
        </div>
        <button
          className={`history-toggle ${historyEnabled ? "on" : "off"}`}
          onClick={() => setHistoryEnabled((prev) => !prev)}
        >
          记录
        </button>
      </header>

      <section className="active-translation">
        <div className="section-title">实时翻译</div>
        <div className="translation-text">
          {activeEntry?.text || "等待命令输出..."}
        </div>
      </section>

      {historyEnabled ? (
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
      ) : null}
    </div>
  );
}
