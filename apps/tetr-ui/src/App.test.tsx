import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import App from "./App";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(async () => []),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => () => {}),
}));

describe("App", () => {
  it("renders a minimal translation-only shell with no status or history text", () => {
    const html = renderToStaticMarkup(<App />);

    expect(html).not.toContain("等待命令输出...");
    expect(html).not.toContain("暂无记录");
  });
});
