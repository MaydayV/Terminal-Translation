import { invoke } from "@tauri-apps/api/core";
import { useEffect, useMemo, useRef, useState, type CSSProperties } from "react";

type TranslationEntry = {
  id: number;
  text: string;
  done: boolean;
  updatedAt: number;
};

type DonePayload = {
  translation?: string;
};

type UiEnvelope = {
  event: string;
  payload?: any;
};

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

function normalizeDisplayText(input: string): string {
  return input
    .replace(/\r\n/g, "\n")
    .replace(/[ \t]+\n/g, "\n")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
}

export default function App() {
  const [entries, setEntries] = useState<TranslationEntry[]>([]);
  const [activeId, setActiveId] = useState<number | null>(null);
  const [status, setStatus] = useState("等待命令输出...");
  const [fontSizePx, setFontSizePx] = useState(11);

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
          const nextSize = parseUiFontSizePx(
            (payload as { uiFontSizePx?: unknown }).uiFontSizePx
          );
          if (nextSize !== null) {
            setFontSizePx(nextSize);
          }
          break;
        }
        case "translation.started": {
          const id = nextIdRef.current++;
          activeIdRef.current = id;
          setActiveId(id);
          setEntries((prev) => [
            {
              id,
              text: "",
              done: false,
              updatedAt: Date.now(),
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

  useEffect(() => {
    const node = translationScrollRef.current;
    if (!node) {
      return;
    }
    node.scrollTop = node.scrollHeight;
  }, [activeEntry?.text, activeId]);

  const displayText = activeEntry?.text || status;
  const panelStyle = useMemo(
    () =>
      ({
        "--window-font-size": `${fontSizePx}px`,
      }) as CSSProperties,
    [fontSizePx]
  );

  return (
    <div className="panel minimal-panel" style={panelStyle}>
      <section className="active-translation minimal-content">
        <div className="section-title">实时翻译</div>
        <div className="translation-card" ref={translationScrollRef}>
          <div className="translation-text">{displayText}</div>
        </div>
      </section>
    </div>
  );
}
