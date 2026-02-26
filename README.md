# tetr

终端实时翻译助手（Terminal Real-time Translator）。

`tetr` 以“子 Shell 包装器”方式工作：
- 你的原始命令输出仍在当前终端原样显示；
- 同时在底部浮窗实时显示中文译文；
- 不改变你现有命令行习惯，按需开启即可。

## 功能特性

- PTY 透传：保留原生终端交互体验。
- 流式翻译：按增量实时输出译文。
- 智能触发：提示符触发 + 空闲超时触发。
- 智能截断：超长输出优先保留错误与尾部关键内容。
- 翻译约束：仅翻译英文自然语言；中文、命令、路径、代码、错误码保持原样。
- 配置方式：支持向导配置（`tetr config`）与脚本配置（`tetr set` / `tetr config set`）。
- 模型连通性测试：支持 `tetr config test` 快速验证当前模型可用性。

## 适用平台

- 当前 Homebrew 发布包：`macOS arm64 (Apple Silicon)`。
- 源码构建：项目为跨平台 Rust/Tauri 架构，但正式发布与主线验证以 macOS 为主。

## 安装指南（含 Homebrew 新手引导）

### 1. 先安装 Homebrew（如果你还没有）

- Homebrew 官网：[https://brew.sh](https://brew.sh)
- Homebrew 安装文档：[https://docs.brew.sh/Installation](https://docs.brew.sh/Installation)

在 macOS 终端执行官方安装命令：

```bash
/bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"
```

安装完成后，按安装脚本提示将 `brew` 加入 shell 环境（常见示例）：

Apple Silicon（通常是 `zsh`）：
```bash
echo 'eval "$(/opt/homebrew/bin/brew shellenv)"' >> ~/.zprofile
eval "$(/opt/homebrew/bin/brew shellenv)"
```

Intel Mac（如果你的脚本提示是 `/usr/local`，以提示为准）：
```bash
echo 'eval "$(/usr/local/bin/brew shellenv)"' >> ~/.zprofile
eval "$(/usr/local/bin/brew shellenv)"
```

验证：
```bash
brew --version
```

如果仍然提示 `command not found: brew`，请先执行上面的 `brew shellenv` 两行，再重开一个终端窗口重试。

### 2. 安装 tetr

```bash
brew tap maydayv/tap
brew install maydayv/tap/tetr
```

验证安装：
```bash
tetr --version
```

升级：
```bash
brew update
brew upgrade tetr
```

卸载：
```bash
brew uninstall tetr
```

## 快速开始

### 1. 打开配置向导

```bash
tetr config
```

建议至少完成以下配置：
- Provider（`deepseek` / `openai-compatible` / `mock`）
- 模型名
- API Base URL
- API Key

### 2. 测试模型连通性（推荐）

```bash
tetr config test
```

连通成功会输出模型、延迟与输出预览。

### 3. 启动实时翻译

```bash
tetr
```

仅终端模式（不启动浮窗）：
```bash
tetr --no-ui
```

结束会话：
- 在子 Shell 输入 `exit`；或
- 点击翻译窗口左上角关闭按钮（会同步结束当前 `tetr` 会话）。

## 配置优先级

运行时配置覆盖顺序（高 -> 低）：

1. 命令行参数
2. 环境变量
3. 本地配置文件（`tetr set` / `tetr config` 写入）
4. 内置默认值

配置文件位置：
- macOS 默认：`~/.config/tetr/config.json`
- 可通过以下命令查看实际路径：

```bash
tetr config path
```

## 常用配置命令

```bash
# 交互向导
tetr config

# 读写配置
tetr config set <key> <value>
tetr config get <key>
tetr config unset <key>
tetr config list

# 与 config set 等价的快捷写法
tetr set <key> <value>

# 连通性测试
tetr config test
```

## 关键配置项

### 模型与 Provider

- `provider`
- `deepseek_model`
- `openai_model`
- `model`（通用模型字段）

### API 与密钥

- `deepseek_base_url`
- `deepseek_api_key`
- `openai_base_url`
- `openai_api_key`
- `api_base_url`（通用）
- `api_key`（通用）

### 翻译行为

- `translate_idle_ms`
- `truncation_max_chars`
- `truncation_tail_lines`
- `truncation_max_error_lines`

### UI 默认显示项

- `ui_history_enabled`：默认是否展示“记录区”
- `ui_source_enabled`：默认是否展示“原文”

### UI 样式

- `ui_dock_icon`：是否显示 Dock 图标（开/关）
- `ui_font_size`：字体大小（9-22）
- `ui_bg_color`：背景色（HEX）
- `ui_bg_opacity`：背景透明度（0-100）
- `ui_bin`：自定义 `tetr-ui` 可执行文件路径

### UI 高度

- `ui_window_height`：窗口总高度（80-900）
- `ui_realtime_height`：实时翻译区高度（80-900）
- `ui_history_height`：记录区高度（80-900）

说明：
- 未设置 `ui_window_height` 时，当前默认总高度为固定值（260px），不再按终端高度比例计算。

## 常用环境变量

### Provider / API

- `TETR_PROVIDER`
- `TETR_DEEPSEEK_API_KEY` / `TETR_API_KEY`
- `TETR_DEEPSEEK_BASE_URL` / `TETR_API_BASE_URL`
- `TETR_DEEPSEEK_MODEL` / `TETR_MODEL`
- `TETR_OPENAI_API_KEY` / `TETR_API_KEY` / `OPENAI_API_KEY`
- `TETR_OPENAI_BASE_URL` / `TETR_API_BASE_URL`
- `TETR_OPENAI_MODEL` / `TETR_MODEL`

### 翻译行为

- `TETR_TRANSLATE_IDLE_MS`
- `TETR_TRUNCATION_MAX_CHARS`
- `TETR_TRUNCATION_TAIL_LINES`
- `TETR_TRUNCATION_MAX_ERROR_LINES`

### UI

- `TETR_UI_BIN`
- `TETR_UI_HISTORY_ENABLED`
- `TETR_UI_SOURCE_ENABLED`
- `TETR_UI_FONT_SIZE`
- `TETR_UI_BG_COLOR`
- `TETR_UI_BG_OPACITY`
- `TETR_UI_WINDOW_HEIGHT`
- `TETR_UI_REALTIME_HEIGHT`
- `TETR_UI_HISTORY_HEIGHT`
- `TETR_UI_DOCK_ICON`：`1/true/yes/on` 时显示 Dock 图标；默认隐藏

## macOS 浮窗行为

- 仅当前台是终端应用（Terminal / iTerm2 / WezTerm 等）时显示浮窗。
- 浮窗与终端窗口同宽，并贴在终端下方，跟随移动与缩放。
- 切换到非终端应用时自动隐藏，减少遮挡。
- 默认不占用 Dock 图标（可通过 `TETR_UI_DOCK_ICON` 开启）。

## 配置示例

### 示例 1：DeepSeek

```bash
tetr set provider deepseek
tetr set deepseek_base_url https://api.deepseek.com
tetr set deepseek_api_key <YOUR_KEY>
tetr set deepseek_model deepseek-chat

tetr config test
tetr
```

### 示例 2：OpenAI Compatible

```bash
tetr set provider openai-compatible
tetr set openai_base_url https://your-compatible-host/v1
tetr set openai_api_key <YOUR_KEY>
tetr set openai_model gpt-4o-mini

tetr config test
tetr
```

### 示例 3：仅调整 UI 样式与高度

```bash
tetr set ui_font_size 10
tetr set ui_bg_color '#0f1b2d'
tetr set ui_bg_opacity 90
tetr set ui_window_height 300
tetr set ui_realtime_height 190
tetr set ui_history_height 110
```

## 从源码开发

### 依赖

- Rust（stable）
- Node.js 20+
- pnpm 10+

### 构建

```bash
pnpm --dir apps/tetr-ui install
pnpm --dir apps/tetr-ui build
cargo build -p tetr-cli -p tetr-ui
```

### 测试

```bash
cargo test -p tetr-core
cargo test -p tetr-cli
cargo test -p tetr-ui
```

### 本地运行

```bash
cargo run -p tetr-cli -- config
cargo run -p tetr-cli --
```

## 发布到 Homebrew（维护者）

自动发布工作流：[`release-homebrew.yml`](./.github/workflows/release-homebrew.yml)

触发方式：推送 `v*` 标签（如 `v0.1.10`）。

工作流会自动：
1. 构建 macOS arm64 二进制（`tetr` + `tetr-ui`）
2. 发布 GitHub Release 附件
3. 生成校验文件
4. 更新 `MaydayV/homebrew-tap` 中的 `Formula/tetr.rb`

发布前需配置仓库 Secret：
- `HOMEBREW_TAP_TOKEN`（可写 `MaydayV/homebrew-tap`）

## 项目结构

- `crates/tetr-core`：PTY 转发、捕获状态机、翻译器、截断策略
- `apps/tetr-cli`：CLI 入口、配置系统、会话编排
- `apps/tetr-ui`：前端界面（React）
- `apps/tetr-ui/src-tauri`：Tauri 后端与 IPC 桥接

## 已知限制

- Homebrew 当前仅提供 macOS arm64 发布包。
- `vim/top/less` 等全屏交互程序默认仅透传，不做翻译。
- 提示符识别是启发式策略，极少数自定义 Prompt 可能需要手动适配。
