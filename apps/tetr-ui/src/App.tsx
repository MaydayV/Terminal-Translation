import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { useEffect, useMemo, useRef, useState, type CSSProperties } from "react";

type UiEnvelope = {
  event: string;
  payload?: any;
};

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
  const [displayText, setDisplayText] = useState("");
  const [fontSizePx, setFontSizePx] = useState(11);
  const [bgColor, setBgColor] = useState("#121b2d");
  const [bgOpacityPercent, setBgOpacityPercent] = useState(100);

  const textRef = useRef("");
  const readySentRef = useRef(false);
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
            uiBgColor?: unknown;
            uiBgOpacityPercent?: unknown;
          };
          const nextSize = parseUiFontSizePx(data.uiFontSizePx);
          if (nextSize !== null) {
            setFontSizePx(nextSize);
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
          textRef.current = "";
          setDisplayText("");
          break;
        }
        case "translation.delta": {
          const delta = (payload as { delta?: string }).delta ?? "";
          if (!delta) {
            break;
          }

          const next = `${textRef.current}${delta}`;
          textRef.current = next;
          setDisplayText(next);
          break;
        }
        case "translation.done": {
          const translation = (payload as { translation?: string }).translation;
          const next =
            typeof translation === "string"
              ? normalizeDisplayText(translation)
              : normalizeDisplayText(textRef.current);
          textRef.current = next;
          setDisplayText(next);
          break;
        }
        case "session.error": {
          const message = (payload as { message?: string }).message ?? "unknown";
          console.warn("[tetr-ui] session error", message);
          break;
        }
        case "session.stopped":
          break;
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
        console.warn("[tetr-ui] frontend_ready failed");
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

  useEffect(() => {
    const node = translationScrollRef.current;
    if (!node) {
      return;
    }
    node.scrollTop = node.scrollHeight;
  }, [displayText]);

  const [bgR, bgG, bgB] = useMemo(() => hexToRgb(bgColor), [bgColor]);
  const bgAlpha = useMemo(() => bgOpacityPercent / 100, [bgOpacityPercent]);
  const panelStyle = useMemo(
    () =>
      ({
        "--window-font-size": `${fontSizePx}px`,
        "--window-bg": `rgba(${bgR}, ${bgG}, ${bgB}, ${bgAlpha})`,
      }) as CSSProperties,
    [fontSizePx, bgR, bgG, bgB, bgAlpha]
  );

  return (
    <div className="panel" style={panelStyle}>
      <div className="translation-layer" ref={translationScrollRef}>
        <div className="translation-text">{displayText}</div>
      </div>
    </div>
  );
}
