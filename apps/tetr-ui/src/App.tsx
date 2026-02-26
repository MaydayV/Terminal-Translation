import { invoke } from "@tauri-apps/api/core";
import { useEffect, useMemo, useRef, useState } from "react";
import { buildAlignedRows } from "./alignment";

type TranslationEntry = {
  id: number;
  reason: string;
  source: string;
  text: string;
  done: boolean;
  startedAt: number;
  updatedAt: number;
  inputChars?: number;
  truncated?: boolean;
};

type DonePayload = {
  translation?: string;
};

type StartedPayload = {
  reason?: string;
  selectedChars?: number;
  source?: string;
  truncated?: boolean;
};

type SessionStartedPayload = {
  provider?: string;
};

type UiEnvelope = {
  event: string;
  payload?: any;
};

const HISTORY_STORAGE_KEY = "tetr.ui.history.enabled";
const SOURCE_STORAGE_KEY = "tetr.ui.source.enabled";
const HISTORY_DEFAULT_ENABLED = false;
const SOURCE_DEFAULT_ENABLED = true;
const MAX_ENTRIES = 20;

function readBooleanStorage(key: string, fallback: boolean): boolean {
  try {
    const stored = window.localStorage.getItem(key);
    if (stored === null) {
      return fallback;
    }
    return stored === "1";
  } catch {
    return fallback;
  }
}

function writeBooleanStorage(key: string, value: boolean) {
  try {
    window.localStorage.setItem(key, value ? "1" : "0");
  } catch {
    // Ignore storage failures in restricted environments.
  }
}

function normalizeDisplayText(input: string): string {
  return input
    .replace(/\r\n/g, "\n")
    .replace(/[ \t]+\n/g, "\n")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
}

function compactText(input: string, maxChars = 120): string {
  const oneLine = normalizeDisplayText(input).replace(/\s+/g, " ").trim();
  if (!oneLine) {
    return "...";
  }
  if (oneLine.length <= maxChars) {
    return oneLine;
  }
  return `${oneLine.slice(0, maxChars)}...`;
}

function statusTone(status: string): "idle" | "running" | "success" | "error" {
  if (status.startsWith("错误")) {
    return "error";
  }
  if (status.includes("翻译中")) {
    return "running";
  }
  if (status.includes("已完成") || status.includes("已停止")) {
    return "success";
  }
  return "idle";
}

function reasonLabel(reason: string): string {
  if (reason === "prompt") {
    return "提示符触发";
  }
  if (reason === "idle") {
    return "空闲触发";
  }
  return reason;
}

function formatTime(timestamp: number): string {
  return new Date(timestamp).toLocaleTimeString([], {
    hour12: false,
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
}

export default function App() {
  const [entries, setEntries] = useState<TranslationEntry[]>([]);
  const [activeId, setActiveId] = useState<number | null>(null);
  const [status, setStatus] = useState("等待会话...");
  const [provider, setProvider] = useState("-");
  const [historyEnabled, setHistoryEnabled] = useState<boolean>(() =>
    readBooleanStorage(HISTORY_STORAGE_KEY, HISTORY_DEFAULT_ENABLED)
  );
  const [sourceEnabled, setSourceEnabled] = useState<boolean>(() =>
    readBooleanStorage(SOURCE_STORAGE_KEY, SOURCE_DEFAULT_ENABLED)
  );
  const [hoveredRowId, setHoveredRowId] = useState<number | null>(null);

  const activeIdRef = useRef<number | null>(null);
  const readySentRef = useRef(false);
  const nextIdRef = useRef(1);
  const translationScrollRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    let pollTimer: number | undefined;
    let inFlight = false;
    let stopped = false;

    const applyEnvelope = (envelope: UiEnvelope) => {
      const payload = envelope.payload ?? {};

      switch (envelope.event) {
        case "session.started": {
          const data = payload as SessionStartedPayload;
          setProvider(data.provider ?? "-");
          setStatus("会话已启动");
          break;
        }
        case "translation.started": {
          const data = payload as StartedPayload;
          const id = nextIdRef.current++;
          const sourceText = normalizeDisplayText(data.source ?? "");

          activeIdRef.current = id;
          setActiveId(id);
          setEntries((prev) => [
            {
              id,
              reason: data.reason ?? "unknown",
              source: sourceText,
              text: "",
              done: false,
              startedAt: Date.now(),
              updatedAt: Date.now(),
              inputChars: data.selectedChars,
              truncated: data.truncated,
            },
            ...prev,
          ].slice(0, MAX_ENTRIES));
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
                ? { ...entry, text: `${entry.text}${delta}`, updatedAt: Date.now() }
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
                text: normalizeDisplayText(translation ?? entry.text),
                done: true,
                updatedAt: Date.now(),
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
  const alignedRows = useMemo(
    () => buildAlignedRows(activeEntry?.source ?? "", activeEntry?.text ?? ""),
    [activeEntry?.source, activeEntry?.text]
  );
  const showAlignedView = sourceEnabled && Boolean(activeEntry?.source);

  useEffect(() => {
    writeBooleanStorage(HISTORY_STORAGE_KEY, historyEnabled);
  }, [historyEnabled]);

  useEffect(() => {
    writeBooleanStorage(SOURCE_STORAGE_KEY, sourceEnabled);
  }, [sourceEnabled]);

  useEffect(() => {
    const node = translationScrollRef.current;
    if (!node) {
      return;
    }

    node.scrollTop = node.scrollHeight;
  }, [activeEntry?.text, activeId, showAlignedView]);

  useEffect(() => {
    setHoveredRowId(null);
  }, [activeId]);

  async function stopSession() {
    try {
      await invoke("request_stop");
    } catch {
      setStatus("错误: 停止会话失败");
    }
  }

  const statusKind = statusTone(status);
  const activeMeta = activeEntry
    ? `${reasonLabel(activeEntry.reason)} · ${
        activeEntry.inputChars ?? activeEntry.source.length
      } 字${activeEntry.truncated ? " · 已截断" : ""}`
    : "等待命令输出...";

  return (
    <div
      className={`panel ${historyEnabled ? "with-history" : "without-history"}`}
    >
      <header className="panel-header">
        <div className="header-left">
          <span className={`status-pill ${statusKind}`}>{status}</span>
          <span className="provider-pill">{provider}</span>
        </div>
        <div className="header-actions">
          <button
            className={`toggle-btn ${sourceEnabled ? "on" : "off"}`}
            onClick={() => setSourceEnabled((prev) => !prev)}
          >
            原文
          </button>
          <button
            className={`toggle-btn ${historyEnabled ? "on" : "off"}`}
            onClick={() => setHistoryEnabled((prev) => !prev)}
          >
            记录
          </button>
          <button className="stop-btn" onClick={stopSession}>
            结束
          </button>
        </div>
      </header>

      <section className="active-translation">
        <div className="section-header">
          <div className="section-title">实时翻译</div>
          <div className="section-meta">{activeMeta}</div>
        </div>

        {showAlignedView ? (
          <div className="aligned-card" ref={translationScrollRef}>
            <div className="aligned-head">
              <span>原文行</span>
              <span>译文段</span>
            </div>

            <div className="aligned-list">
              {alignedRows.length === 0 ? (
                <div className="aligned-empty">等待翻译输出...</div>
              ) : (
                alignedRows.map((row) => {
                  const active = hoveredRowId === row.id;
                  return (
                    <div
                      key={row.id}
                      className={`aligned-row ${active ? "active" : ""}`}
                      onMouseEnter={() => setHoveredRowId(row.id)}
                      onMouseLeave={() => setHoveredRowId(null)}
                    >
                      <div
                        className={`aligned-cell source ${
                          row.source ? "" : "is-empty"
                        }`}
                      >
                        {row.source || "—"}
                      </div>
                      <div
                        className={`aligned-cell translation ${
                          row.translation ? "" : "is-empty"
                        }`}
                      >
                        {row.translation || (activeEntry?.done ? "—" : "...")}
                      </div>
                    </div>
                  );
                })
              )}
            </div>
          </div>
        ) : null}

        {!showAlignedView ? (
          <div className="translation-card" ref={translationScrollRef}>
            <div className="translation-text">
              {activeEntry?.text || "等待命令输出..."}
            </div>
          </div>
        ) : null}
      </section>

      {historyEnabled ? (
        <section className="history">
          <div className="section-title">最近记录</div>
          <ul>
            {entries.map((entry) => (
              <li key={entry.id}>
                <div className="meta-row">
                  <span>{reasonLabel(entry.reason)}</span>
                  <span>{entry.done ? "已完成" : "流式中"}</span>
                </div>
                <div className="history-text">{compactText(entry.text)}</div>
                <div className="history-time">{formatTime(entry.updatedAt)}</div>
              </li>
            ))}
          </ul>
        </section>
      ) : null}
    </div>
  );
}
