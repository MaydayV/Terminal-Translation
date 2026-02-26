import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { useEffect, useMemo, useRef, useState, type CSSProperties } from "react";
import { prependEntryWithLimit, type TranslationEntry } from "./entries";

type DonePayload = {
  translation?: string;
};

type UiEnvelope = {
  event: string;
  payload?: any;
};

const MAX_ENTRIES = 200;
const EVENTS_AVAILABLE_EVENT = "tetr://events-available";
const FALLBACK_POLL_INTERVAL_MS = 1500;

function parseUiFontSizePx(input: unknown): number | null {
  const numeric =
    typeof input === "number"
      ? input
      : typeof input === "string"
      ? Number.parseInt(input, 10)
      : Number.NaN;

  if (!Number.isFinite(numeric)) {
    return null;
  }

  const rounded = Math.round(numeric);
  if (rounded < 9 || rounded > 22) {
    return null;
  }

  return rounded;
}

function parseUiPanelHeightPx(input: unknown): number | null {
  const numeric =
    typeof input === "number"
      ? input
      : typeof input === "string"
      ? Number.parseInt(input, 10)
      : Number.NaN;

  if (!Number.isFinite(numeric)) {
    return null;
  }

  const rounded = Math.round(numeric);
  if (rounded < 80 || rounded > 900) {
    return null;
  }

  return rounded;
}

function parseUiBgOpacityPercent(input: unknown): number | null {
  const numeric =
    typeof input === "number"
      ? input
      : typeof input === "string"
      ? Number.parseInt(input, 10)
      : Number.NaN;

  if (!Number.isFinite(numeric)) {
    return null;
  }

  const rounded = Math.round(numeric);
  if (rounded < 0 || rounded > 100) {
    return null;
  }

  return rounded;
}

function parseUiBgColor(input: unknown): string | null {
  if (typeof input !== "string") {
    return null;
  }
  const text = input.trim();
  if (!text) {
    return null;
  }
  const withHash = text.startsWith("#") ? text : `#${text}`;
  const raw = withHash.slice(1);
  if (!/^[0-9a-fA-F]+$/.test(raw)) {
    return null;
  }
  if (raw.length === 3) {
    const expanded = raw
      .split("")
      .map((ch) => `${ch}${ch}`)
      .join("")
      .toLowerCase();
    return `#${expanded}`;
  }
  if (raw.length === 6) {
    return `#${raw.toLowerCase()}`;
  }
  return null;
}

function parseUiBoolean(input: unknown): boolean | null {
  if (typeof input === "boolean") {
    return input;
  }

  if (typeof input === "number") {
    if (input === 1) {
      return true;
    }
    if (input === 0) {
      return false;
    }
    return null;
  }

  if (typeof input === "string") {
    const normalized = input.trim().toLowerCase();
    if (normalized === "1" || normalized === "true" || normalized === "on") {
      return true;
    }
    if (normalized === "0" || normalized === "false" || normalized === "off") {
      return false;
    }
  }

  return null;
}

function hexToRgb(hex: string): [number, number, number] {
  const raw = hex.replace(/^#/, "");
  const normalized = raw.length === 3 ? raw.split("").map((ch) => `${ch}${ch}`).join("") : raw;
  const safe = normalized.padEnd(6, "0").slice(0, 6);
  const r = Number.parseInt(safe.slice(0, 2), 16);
  const g = Number.parseInt(safe.slice(2, 4), 16);
  const b = Number.parseInt(safe.slice(4, 6), 16);
  return [r, g, b];
}

function normalizeDisplayText(input: string): string {
  return input
    .replace(/\r\n/g, "\n")
    .replace(/\r/g, "\n")
    .replace(/[ \t]+\n/g, "\n");
}

export default function App() {
  const [entries, setEntries] = useState<TranslationEntry[]>([]);
  const [activeId, setActiveId] = useState<number | null>(null);
  const [status, setStatus] = useState("等待命令输出...");
  const [fontSizePx, setFontSizePx] = useState(11);
  const [realtimeHeightPx, setRealtimeHeightPx] = useState<number | null>(null);
  const [historyHeightPx, setHistoryHeightPx] = useState<number | null>(null);
  const [historyEnabled, setHistoryEnabled] = useState(true);
  const [bgColor, setBgColor] = useState("#121b2d");
  const [bgOpacityPercent, setBgOpacityPercent] = useState(100);

  const activeIdRef = useRef<number | null>(null);
  const readySentRef = useRef(false);
  const nextIdRef = useRef(1);
  const translationScrollRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    let pollTimer: number | undefined;
    let unlisten: UnlistenFn | null = null;
    let inFlight = false;
    let stopped = false;

    const applyEnvelope = (envelope: UiEnvelope) => {
      const payload = envelope.payload ?? {};

      switch (envelope.event) {
        case "session.started": {
          const data = payload as {
            uiFontSizePx?: unknown;
            uiRealtimeHeightPx?: unknown;
            uiHistoryHeightPx?: unknown;
            uiHistoryEnabled?: unknown;
            uiBgColor?: unknown;
            uiBgOpacityPercent?: unknown;
          };
          const nextSize = parseUiFontSizePx(data.uiFontSizePx);
          if (nextSize !== null) {
            setFontSizePx(nextSize);
          }
          setRealtimeHeightPx(parseUiPanelHeightPx(data.uiRealtimeHeightPx));
          setHistoryHeightPx(parseUiPanelHeightPx(data.uiHistoryHeightPx));
          const nextHistoryEnabled = parseUiBoolean(data.uiHistoryEnabled);
          if (nextHistoryEnabled !== null) {
            setHistoryEnabled(nextHistoryEnabled);
          }
          const nextBg = parseUiBgColor(data.uiBgColor);
          if (nextBg !== null) {
            setBgColor(nextBg);
          }
          const nextOpacity = parseUiBgOpacityPercent(data.uiBgOpacityPercent);
          if (nextOpacity !== null) {
            setBgOpacityPercent(nextOpacity);
          }
          break;
        }
        case "translation.started": {
          const id = nextIdRef.current++;
          activeIdRef.current = id;
          setActiveId(id);
          setEntries((prev) =>
            prependEntryWithLimit(
              prev,
              {
                id,
                text: "",
                done: false,
                updatedAt: Date.now(),
              },
              MAX_ENTRIES
            )
          );
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
    void listen(EVENTS_AVAILABLE_EVENT, () => {
      void poll();
    })
      .then((dispose) => {
        if (stopped) {
          dispose();
          return;
        }
        unlisten = dispose;
      })
      .catch((error) => {
        console.warn("[tetr-ui] listen events-available failed", error);
      });

    pollTimer = window.setInterval(() => {
      void poll();
    }, FALLBACK_POLL_INTERVAL_MS);

    return () => {
      stopped = true;
      if (pollTimer !== undefined) {
        window.clearInterval(pollTimer);
      }
      if (unlisten) {
        unlisten();
      }
    };
  }, []);

  const activeEntry = useMemo(
    () => entries.find((entry) => entry.id === activeId) ?? entries[0],
    [entries, activeId]
  );
  const historyEntries = useMemo(
    () =>
      entries
        .filter((entry) => entry.id !== activeEntry?.id && entry.text.trim().length > 0)
        .slice(0, 12),
    [entries, activeEntry?.id]
  );

  useEffect(() => {
    const node = translationScrollRef.current;
    if (!node) {
      return;
    }
    node.scrollTop = node.scrollHeight;
  }, [activeEntry?.text, activeId]);

  const displayText = activeEntry?.text || status;
  const [bgR, bgG, bgB] = useMemo(() => hexToRgb(bgColor), [bgColor]);
  const bgAlpha = useMemo(() => bgOpacityPercent / 100, [bgOpacityPercent]);
  const translationCardAlpha = useMemo(() => Math.min(1, bgAlpha + 0.12), [bgAlpha]);
  const historyCardAlpha = useMemo(() => Math.min(1, bgAlpha + 0.07), [bgAlpha]);
  const panelStyle = useMemo(
    () =>
      ({
        "--window-font-size": `${fontSizePx}px`,
        "--window-bg": `rgba(${bgR}, ${bgG}, ${bgB}, ${bgAlpha})`,
        "--window-card-primary": `rgba(${bgR}, ${bgG}, ${bgB}, ${translationCardAlpha})`,
        "--window-card-secondary": `rgba(${bgR}, ${bgG}, ${bgB}, ${historyCardAlpha})`,
      }) as CSSProperties,
    [fontSizePx, bgR, bgG, bgB, bgAlpha, translationCardAlpha, historyCardAlpha]
  );
  const contentStyle = useMemo(() => {
    if (!historyEnabled) {
      return { gridTemplateRows: "minmax(0, 1fr)" } as CSSProperties;
    }

    if (realtimeHeightPx !== null && historyHeightPx !== null) {
      return { gridTemplateRows: `${realtimeHeightPx}px ${historyHeightPx}px` } as CSSProperties;
    }
    if (realtimeHeightPx !== null) {
      return { gridTemplateRows: `${realtimeHeightPx}px minmax(0, 1fr)` } as CSSProperties;
    }
    if (historyHeightPx !== null) {
      return { gridTemplateRows: `minmax(0, 1fr) ${historyHeightPx}px` } as CSSProperties;
    }
    return { gridTemplateRows: "minmax(0, 2fr) minmax(0, 1fr)" } as CSSProperties;
  }, [historyEnabled, realtimeHeightPx, historyHeightPx]);

  return (
    <div className="panel minimal-panel" style={panelStyle}>
      <div className="content-grid" style={contentStyle}>
        <section className="active-translation minimal-content">
          <div className="translation-card" ref={translationScrollRef}>
            <div className="translation-text">{displayText}</div>
          </div>
        </section>

        {historyEnabled ? (
          <section className="history-section minimal-content">
            <div className="history-card">
              {historyEntries.length === 0 ? (
                <div className="history-empty">暂无记录</div>
              ) : (
                historyEntries.map((entry) => (
                  <div key={entry.id} className="history-item">
                    <div className="history-text">{normalizeDisplayText(entry.text)}</div>
                  </div>
                ))
              )}
            </div>
          </section>
        ) : null}
      </div>
    </div>
  );
}
