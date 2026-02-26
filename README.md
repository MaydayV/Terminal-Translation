# tetr - 命令行实时翻译助手

`tetr` 是一个终端包装器：你在原终端里执行 `tetr` 后，会进入一个子 Shell 会话；命令输出仍然原样显示在当前终端，同时在底部浮动窗口实时流式展示中文翻译。

目标是“不改变终端习惯，只在需要时开启翻译”。

## 适用场景
- 阅读英文命令输出（构建日志、接口响应、报错信息）
- 排查问题时保留原始输出，同时快速理解中文语义
- 需要兼容 OpenAI 协议或 DeepSeek 的翻译接口

## 核心能力
- PTY 透传：保持原生终端交互体验
- 实时流式翻译：翻译结果按增量流式显示
- 输出智能触发：提示符触发 + 空闲超时触发
- 输出智能截断：超长日志优先保留错误与尾部关键信息
- 交互式配置：`tetr config` 向导式配置
- 脚本化配置：`tetr set` / `tetr config set`

## 当前支持
- 平台：macOS、Windows（MVP）
- Provider：`deepseek`、`openai-compatible`、`mock`

## 浮窗行为（macOS）
- 浮窗使用系统原生窗口装饰（含关闭/最小化按钮），点击关闭会结束当前 `tetr` 会话。
- 当 Terminal / iTerm2 处于前台时，浮窗会与终端窗口同宽，贴住底部边缘，并跟随移动与缩放。
- 切换到非终端应用时，浮窗会自动隐藏，避免遮挡。
- 对暂未适配窗口坐标读取的终端（如部分第三方终端），会回退为屏幕底部固定显示。

## 快速开始
### 1. 安装依赖（开发环境）
```bash
pnpm --dir apps/tetr-ui install
cargo build -p tetr-cli -p tetr-ui
```

### 2. Homebrew 安装（给其他用户）
当前自动发布产物为 `macOS arm64`（Apple 芯片）。

首次安装：
```bash
brew tap MaydayV/tap
brew install tetr
```

升级：
```bash
brew update
brew upgrade tetr
```

安装后即可直接运行：
```bash
tetr config
tetr
```

### 3. 打开交互配置向导（推荐）
```bash
target/debug/tetr config
```

你可以在向导里完成：
- 切换 provider
- 设置模型
- 设置接口地址
- 设置 API Key
- 设置翻译触发延迟
- 设置输出截断参数

### 4. 启动 tetr
```bash
target/debug/tetr
```

仅终端模式（不拉起浮窗）：
```bash
target/debug/tetr --no-ui
```

## 配置方式
`tetr` 支持三种配置来源，优先级如下：
1. 命令行参数
2. 环境变量
3. 本地持久化配置（`tetr set` / `tetr config` 写入）
4. 默认值

### 交互式配置
```bash
tetr config
```

### 脚本化配置
```bash
# 设置
tetr set provider openai-compatible
tetr set openai_base_url https://your-compatible-host/v1
tetr set openai_api_key sk-xxx
tetr set openai_model gpt-4o-mini

# 查询
tetr config get provider

# 查看全部（密钥脱敏）
tetr config list

# 删除配置
tetr config unset openai_model

# 查看配置文件路径
tetr config path
```

## 使用示例
### 示例 1：DeepSeek
```bash
tetr set provider deepseek
tetr set deepseek_base_url https://api.deepseek.com
tetr set deepseek_api_key your_key
tetr set deepseek_model deepseek-chat

# 启动
tetr
```

### 示例 2：兼容 OpenAI 协议
```bash
tetr set provider openai-compatible
tetr set openai_base_url https://your-compatible-host/v1
tetr set openai_api_key your_key
tetr set openai_model gpt-4o-mini

# 启动
tetr
```

### 示例 3：使用环境变量临时覆盖
```bash
TETR_PROVIDER=mock tetr --no-ui
```

## 可配置项
### Provider 与模型
- `provider`
- `deepseek_model`
- `openai_model`
- `model`（通用模型字段）

### 接口与密钥
- `deepseek_base_url`
- `deepseek_api_key`
- `openai_base_url`
- `openai_api_key`
- `api_base_url`（通用）
- `api_key`（通用）

### 行为参数
- `translate_idle_ms`
- `truncation_max_chars`
- `truncation_tail_lines`
- `truncation_max_error_lines`
- `ui_bin`（指定 `tetr-ui` 可执行文件路径）

## 常用环境变量
- `TETR_PROVIDER`
- `TETR_DEEPSEEK_API_KEY` / `TETR_API_KEY`
- `TETR_DEEPSEEK_BASE_URL` / `TETR_API_BASE_URL`
- `TETR_DEEPSEEK_MODEL` / `TETR_MODEL`
- `TETR_OPENAI_API_KEY` / `TETR_API_KEY` / `OPENAI_API_KEY`
- `TETR_OPENAI_BASE_URL` / `TETR_API_BASE_URL`
- `TETR_OPENAI_MODEL` / `TETR_MODEL`
- `TETR_TRANSLATE_IDLE_MS`
- `TETR_TRUNCATION_MAX_CHARS`
- `TETR_TRUNCATION_TAIL_LINES`
- `TETR_TRUNCATION_MAX_ERROR_LINES`
- `TETR_UI_BIN`

## 开发命令
```bash
# 核心测试
cargo test -p tetr-core

# CLI 构建
cargo build -p tetr-cli

# UI 构建
pnpm --dir apps/tetr-ui build
cargo build -p tetr-ui

# 一键冒烟
./scripts/smoke.sh
```

## 发布给 Homebrew（维护者）
仓库已包含自动发布工作流：[`.github/workflows/release-homebrew.yml`](./.github/workflows/release-homebrew.yml)  
触发方式：推送 `v*` 标签（例如 `v0.1.0`）。

工作流会自动完成：
1. 构建 macOS `arm64` 二进制包（含 `tetr`、`tetr-ui`）
2. 发布 GitHub Release 附件
3. 生成 `SHA256SUMS.txt`
4. 更新 `homebrew-tap` 仓库中的 `Formula/tetr.rb`（需要密钥）

发布前需要在主仓库配置 Secret：
- `HOMEBREW_TAP_TOKEN`：一个可写 `MaydayV/homebrew-tap` 的 GitHub Token

推荐发布流程：
```bash
git tag v0.1.0
git push origin v0.1.0
```

## 项目结构
- `crates/tetr-core`：状态机、PTY 转发、翻译器、截断策略
- `apps/tetr-cli`：`tetr` 命令入口与会话编排
- `apps/tetr-ui`：前端界面
- `apps/tetr-ui/src-tauri`：Tauri 后端与 IPC 桥接

## 已知限制（MVP）
- Linux 尚未完成正式支持
- `vim/top/less` 等全屏交互程序默认不翻译，仅透传
- 提示符识别是启发式策略，极少数自定义 Prompt 可能需要额外适配
