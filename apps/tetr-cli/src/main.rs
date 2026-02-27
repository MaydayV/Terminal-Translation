use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use crossbeam_channel::{unbounded, Receiver, Sender};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::env;
use std::fs;
use std::io::{self, BufRead, BufReader, IsTerminal, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use terminal_size::{terminal_size, Height, Width};
use tetr_core::capture_state::{CaptureState, TriggerReason, TriggeredCapture};
use tetr_core::pty_bridge::{OutputChunk, PtyBridge};
use tetr_core::translator::deepseek::DeepSeekTranslator;
use tetr_core::translator::mock::MockTranslator;
use tetr_core::translator::{TranslateError, TranslationMeta, Translator};
use tetr_core::truncation::{truncate_for_translation, TruncationConfig};

#[derive(Debug, Parser)]
#[command(name = "tetr", version, about = "Terminal Translation Assistant")]
struct CliArgs {
    #[command(subcommand)]
    command: Option<CliCommand>,

    #[arg(long)]
    provider: Option<String>,

    #[arg(long, default_value_t = false)]
    no_ui: bool,
}

#[derive(Debug, Subcommand)]
enum CliCommand {
    Config {
        #[command(subcommand)]
        action: Option<ConfigCommand>,
    },
    Set {
        key: String,
        value: String,
    },
}

#[derive(Debug, Subcommand)]
enum ConfigCommand {
    Set { key: String, value: String },
    Get { key: String },
    Unset { key: String },
    Path,
    List,
    Test,
}

#[derive(Debug, Clone)]
struct AppConfig {
    provider: String,
    idle_ms: u64,
    truncation: TruncationConfig,
    deepseek_api_key: Option<String>,
    deepseek_base_url: String,
    deepseek_model: String,
    openai_api_key: Option<String>,
    openai_base_url: String,
    openai_model: String,
    ui_bin: Option<String>,
    ui_terminal_bundle_ids: Option<String>,
    ui_dock_icon: Option<bool>,
    ui_font_size: Option<u16>,
    ui_bg_color: Option<String>,
    ui_bg_opacity: Option<u8>,
    ui_window_height: Option<u16>,
    ui_realtime_height: Option<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct PersistedConfig {
    provider: Option<String>,
    translate_idle_ms: Option<u64>,
    deepseek_api_key: Option<String>,
    deepseek_base_url: Option<String>,
    deepseek_model: Option<String>,
    openai_api_key: Option<String>,
    openai_base_url: Option<String>,
    openai_model: Option<String>,
    api_key: Option<String>,
    api_base_url: Option<String>,
    model: Option<String>,
    truncation_max_chars: Option<usize>,
    truncation_tail_lines: Option<usize>,
    truncation_max_error_lines: Option<usize>,
    ui_bin: Option<String>,
    ui_terminal_bundle_ids: Option<String>,
    ui_dock_icon: Option<bool>,
    ui_font_size: Option<u16>,
    ui_bg_color: Option<String>,
    ui_bg_opacity: Option<u8>,
    ui_window_height: Option<u16>,
    ui_realtime_height: Option<u16>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct IpcEnvelope {
    event: String,
    payload: Value,
}

struct IpcServer {
    port: u16,
    event_tx: Sender<IpcEnvelope>,
    control_rx: Receiver<String>,
}

#[derive(Debug)]
struct ConnectivityProbeResult {
    output: String,
    delta_count: usize,
    meta: TranslationMeta,
}

impl IpcServer {
    fn start() -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).context("failed to bind IPC socket")?;
        let port = listener.local_addr()?.port();

        let (event_tx, event_rx) = unbounded::<IpcEnvelope>();
        let (control_tx, control_rx) = unbounded::<String>();

        thread::spawn(move || {
            let Ok((stream, _)) = listener.accept() else {
                return;
            };

            if stream.set_nodelay(true).is_err() {
                return;
            }

            if let Ok(reader_stream) = stream.try_clone() {
                let control_tx_reader = control_tx.clone();
                thread::spawn(move || {
                    let reader = BufReader::new(reader_stream);
                    for line in reader.lines() {
                        let Ok(line) = line else {
                            break;
                        };
                        let Ok(envelope) = serde_json::from_str::<IpcEnvelope>(&line) else {
                            continue;
                        };
                        let _ = control_tx_reader.send(envelope.event);
                    }
                });
            }

            let mut writer = std::io::BufWriter::new(stream);
            for envelope in event_rx {
                let Ok(line) = serde_json::to_string(&envelope) else {
                    continue;
                };

                if writeln!(writer, "{line}").is_err() {
                    break;
                }
                if writer.flush().is_err() {
                    break;
                }
            }
        });

        Ok(Self {
            port,
            event_tx,
            control_rx,
        })
    }

    fn port(&self) -> u16 {
        self.port
    }

    fn send_event(&self, event: &str, payload: Value) {
        let _ = self.event_tx.send(IpcEnvelope {
            event: event.to_string(),
            payload,
        });
    }

    fn try_recv_control(&self) -> Option<String> {
        self.control_rx.try_recv().ok()
    }
}

struct RawModeGuard {
    enabled: bool,
}

impl RawModeGuard {
    fn acquire() -> Result<Self> {
        if std::io::stdin().is_terminal() {
            enable_raw_mode().context("failed to enable raw mode")?;
            Ok(Self { enabled: true })
        } else {
            Ok(Self { enabled: false })
        }
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        if self.enabled {
            let _ = disable_raw_mode();
        }
    }
}

fn main() -> Result<()> {
    env_logger::init();

    let args = CliArgs::parse();
    if let Some(command) = args.command {
        return run_config_command(command);
    }

    let cfg = AppConfig::from_sources(args.provider.clone())?;
    let translator = build_translator(&cfg)?;

    let mut ui_child: Option<Child> = None;
    let mut ipc_server: Option<IpcServer> = None;

    if !args.no_ui {
        match IpcServer::start() {
            Ok(ipc) => {
                match spawn_ui_process(
                    ipc.port(),
                    cfg.ui_bin.as_deref(),
                    cfg.ui_window_height,
                    cfg.ui_dock_icon,
                    cfg.ui_terminal_bundle_ids.as_deref(),
                ) {
                    Ok(child) => {
                        ui_child = Some(child);
                        ipc.send_event("session.started", build_session_started_payload(&cfg));
                        ipc_server = Some(ipc);
                    }
                    Err(err) => {
                        eprintln!("[tetr] failed to spawn UI: {err}");
                    }
                }
            }
            Err(err) => {
                eprintln!("[tetr] failed to initialize UI IPC: {err}");
            }
        }
    }

    let _raw_guard = RawModeGuard::acquire()?;

    let shell = choose_shell();
    let (mut cols, mut rows) = detect_terminal_size();

    let bridge = PtyBridge::start(&shell, cols, rows)
        .map_err(|err| anyhow!("failed to start wrapped shell: {err}"))?;

    let mut capture = CaptureState::new(cfg.idle_ms);
    let truncation_config = cfg.truncation.clone();

    let mut running = true;
    while running {
        while let Ok(command) = bridge.command_rx.try_recv() {
            capture.note_user_command(&command);
        }

        match bridge.output_rx.recv_timeout(Duration::from_millis(50)) {
            Ok(chunk) => {
                handle_output_chunk(
                    &mut capture,
                    chunk,
                    translator.as_ref(),
                    &truncation_config,
                    ipc_server.as_ref(),
                );
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                running = false;
            }
        }

        if let Some(triggered) = capture.flush_if_idle(Instant::now()) {
            process_triggered_capture(
                triggered,
                translator.as_ref(),
                &truncation_config,
                ipc_server.as_ref(),
            );
        }

        if bridge.exit_rx.try_recv().is_ok() {
            running = false;
        }

        if let Some(ipc) = ipc_server.as_ref() {
            while let Some(control_event) = ipc.try_recv_control() {
                if control_event == "control.stop" {
                    bridge.terminate();
                    running = false;
                    break;
                }
            }
        }

        let (new_cols, new_rows) = detect_terminal_size();
        if new_cols != cols || new_rows != rows {
            let _ = bridge.resize(new_cols, new_rows);
            cols = new_cols;
            rows = new_rows;
        }
    }

    bridge.terminate();

    if let Some(ipc) = ipc_server.as_ref() {
        ipc.send_event("session.stopped", json!({}));
        thread::sleep(Duration::from_millis(60));
    }

    if let Some(mut child) = ui_child {
        let _ = child.kill();
    }

    Ok(())
}

impl AppConfig {
    fn from_sources(cli_provider: Option<String>) -> Result<Self> {
        let file_cfg = load_persisted_config()?;

        let provider = cli_provider
            .or_else(|| read_env_nonempty(&["TETR_PROVIDER"]))
            .or_else(|| file_cfg.provider.clone())
            .unwrap_or_else(|| "deepseek".to_string())
            .to_ascii_lowercase();

        let idle_ms = read_env_u64(&["TETR_TRANSLATE_IDLE_MS"])
            .or(file_cfg.translate_idle_ms)
            .unwrap_or(300);

        if idle_ms < 50 {
            return Err(anyhow!(
                "TETR_TRANSLATE_IDLE_MS must be >= 50, got {}",
                idle_ms
            ));
        }

        let truncation = TruncationConfig {
            max_chars: read_env_usize(&["TETR_TRUNCATION_MAX_CHARS"])
                .or(file_cfg.truncation_max_chars)
                .unwrap_or(3_500),
            tail_lines: read_env_usize(&["TETR_TRUNCATION_TAIL_LINES"])
                .or(file_cfg.truncation_tail_lines)
                .unwrap_or(20),
            max_error_lines: read_env_usize(&["TETR_TRUNCATION_MAX_ERROR_LINES"])
                .or(file_cfg.truncation_max_error_lines)
                .unwrap_or(20),
        };

        let deepseek_api_key = read_env_nonempty(&["TETR_DEEPSEEK_API_KEY", "TETR_API_KEY"])
            .or_else(|| file_cfg.deepseek_api_key.clone())
            .or_else(|| file_cfg.api_key.clone());

        let deepseek_base_url = read_env_nonempty(&["TETR_DEEPSEEK_BASE_URL", "TETR_API_BASE_URL"])
            .or_else(|| file_cfg.deepseek_base_url.clone())
            .or_else(|| file_cfg.api_base_url.clone())
            .unwrap_or_else(|| "https://api.deepseek.com".to_string());

        let deepseek_model = read_env_nonempty(&["TETR_DEEPSEEK_MODEL", "TETR_MODEL"])
            .or_else(|| file_cfg.deepseek_model.clone())
            .or_else(|| file_cfg.model.clone())
            .unwrap_or_else(|| "deepseek-chat".to_string());

        let openai_api_key =
            read_env_nonempty(&["TETR_OPENAI_API_KEY", "TETR_API_KEY", "OPENAI_API_KEY"])
                .or_else(|| file_cfg.openai_api_key.clone())
                .or_else(|| file_cfg.api_key.clone());

        let openai_base_url = read_env_nonempty(&["TETR_OPENAI_BASE_URL", "TETR_API_BASE_URL"])
            .or_else(|| file_cfg.openai_base_url.clone())
            .or_else(|| file_cfg.api_base_url.clone())
            .unwrap_or_else(|| "https://api.openai.com/v1".to_string());

        let openai_model = read_env_nonempty(&["TETR_OPENAI_MODEL", "TETR_MODEL"])
            .or_else(|| file_cfg.openai_model.clone())
            .or_else(|| file_cfg.model.clone())
            .unwrap_or_else(|| "gpt-4o-mini".to_string());

        let ui_bin = read_env_nonempty(&["TETR_UI_BIN"]).or_else(|| file_cfg.ui_bin.clone());
        let ui_terminal_bundle_ids = read_env_nonempty(&["TETR_UI_TERMINAL_BUNDLE_IDS"])
            .and_then(|v| normalize_terminal_bundle_id_list(&v))
            .or_else(|| {
                file_cfg
                    .ui_terminal_bundle_ids
                    .as_deref()
                    .and_then(normalize_terminal_bundle_id_list)
            });
        let ui_dock_icon = read_env_bool(&["TETR_UI_DOCK_ICON"]).or(file_cfg.ui_dock_icon);
        let ui_font_size = read_env_u16(&["TETR_UI_FONT_SIZE"]).or(file_cfg.ui_font_size);
        let ui_bg_color = read_env_nonempty(&["TETR_UI_BG_COLOR"])
            .and_then(|v| parse_ui_bg_color(&v))
            .or(file_cfg.ui_bg_color.clone());
        let ui_bg_opacity = read_env_nonempty(&["TETR_UI_BG_OPACITY"])
            .and_then(|v| parse_ui_bg_opacity(&v))
            .or(file_cfg.ui_bg_opacity);
        let ui_window_height =
            read_env_u16_height(&["TETR_UI_WINDOW_HEIGHT"]).or(file_cfg.ui_window_height);
        let ui_realtime_height =
            read_env_u16_height(&["TETR_UI_REALTIME_HEIGHT"]).or(file_cfg.ui_realtime_height);

        Ok(Self {
            provider,
            idle_ms,
            truncation,
            deepseek_api_key,
            deepseek_base_url,
            deepseek_model,
            openai_api_key,
            openai_base_url,
            openai_model,
            ui_bin,
            ui_terminal_bundle_ids,
            ui_dock_icon,
            ui_font_size,
            ui_bg_color,
            ui_bg_opacity,
            ui_window_height,
            ui_realtime_height,
        })
    }
}

fn build_translator(cfg: &AppConfig) -> Result<Box<dyn Translator>> {
    match cfg.provider.as_str() {
        "deepseek" => {
            let api_key = cfg.deepseek_api_key.clone().ok_or_else(|| {
                anyhow!("缺少 DeepSeek API Key，请设置 TETR_DEEPSEEK_API_KEY/TETR_API_KEY 或使用 tetr set")
            })?;
            let translator = DeepSeekTranslator::new(
                "deepseek",
                cfg.deepseek_base_url.clone(),
                api_key,
                cfg.deepseek_model.clone(),
                Duration::from_secs(30),
            )
            .map_err(map_translate_error)?;
            Ok(Box::new(translator))
        }
        "openai" | "openai-compatible" | "compatible" => {
            let api_key = cfg.openai_api_key.clone().ok_or_else(|| {
                anyhow!(
                    "缺少兼容 OpenAI 的 API Key，请设置 TETR_OPENAI_API_KEY/TETR_API_KEY/OPENAI_API_KEY 或使用 tetr set"
                )
            })?;
            let translator = DeepSeekTranslator::new(
                "openai-compatible",
                cfg.openai_base_url.clone(),
                api_key,
                cfg.openai_model.clone(),
                Duration::from_secs(30),
            )
            .map_err(map_translate_error)?;
            Ok(Box::new(translator))
        }
        "mock" => Ok(Box::new(MockTranslator)),
        other => Err(anyhow!(
            "unsupported provider: {other}. available providers: deepseek, openai-compatible, mock"
        )),
    }
}

fn map_translate_error(err: TranslateError) -> anyhow::Error {
    anyhow!(err.to_string())
}

fn read_env_nonempty(keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Ok(value) = env::var(key) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

fn read_env_u64(keys: &[&str]) -> Option<u64> {
    read_env_nonempty(keys).and_then(|v| v.parse::<u64>().ok())
}

fn read_env_usize(keys: &[&str]) -> Option<usize> {
    read_env_nonempty(keys).and_then(|v| v.parse::<usize>().ok())
}

fn read_env_bool(keys: &[&str]) -> Option<bool> {
    read_env_nonempty(keys).and_then(|v| parse_bool_value(&v))
}

fn read_env_u16(keys: &[&str]) -> Option<u16> {
    read_env_nonempty(keys).and_then(|v| parse_ui_font_size(&v))
}

fn read_env_u16_height(keys: &[&str]) -> Option<u16> {
    read_env_nonempty(keys).and_then(|v| parse_ui_window_height(&v))
}

fn run_config_command(command: CliCommand) -> Result<()> {
    match command {
        CliCommand::Set { key, value } => {
            let mut cfg = load_persisted_config()?;
            set_config_value(&mut cfg, &key, &value)?;
            save_persisted_config(&cfg)?;
            println!("已设置: {key}");
            Ok(())
        }
        CliCommand::Config { action } => match action {
            Some(ConfigCommand::Set { key, value }) => {
                let mut cfg = load_persisted_config()?;
                set_config_value(&mut cfg, &key, &value)?;
                save_persisted_config(&cfg)?;
                println!("已设置: {key}");
                Ok(())
            }
            Some(ConfigCommand::Get { key }) => {
                let cfg = load_persisted_config()?;
                let value = get_config_value(&cfg, &key)?;
                match value {
                    Some(v) => println!("{v}"),
                    None => println!("未设置"),
                }
                Ok(())
            }
            Some(ConfigCommand::Unset { key }) => {
                let mut cfg = load_persisted_config()?;
                unset_config_value(&mut cfg, &key)?;
                save_persisted_config(&cfg)?;
                println!("已清除: {key}");
                Ok(())
            }
            Some(ConfigCommand::Path) => {
                println!("{}", config_file_path()?.display());
                Ok(())
            }
            Some(ConfigCommand::List) => {
                let cfg = load_persisted_config()?;
                let path = config_file_path()?;
                println!("配置文件: {}", path.display());
                println!(
                    "{}",
                    serde_json::to_string_pretty(&masked_config_for_display(&cfg))?
                );
                println!("可配置键: {}", supported_config_keys().join(", "));
                Ok(())
            }
            Some(ConfigCommand::Test) => run_config_connectivity_test(),
            None => run_config_wizard(),
        },
    }
}

fn run_config_connectivity_test() -> Result<()> {
    let cfg = AppConfig::from_sources(None)?;
    let translator = build_translator(&cfg)?;

    println!("开始测试模型连通性...");
    println!("Provider: {}", cfg.provider);
    let probe = run_connectivity_probe(translator.as_ref())?;
    println!(
        "连通性测试通过：model={} latency={}ms deltas={}",
        probe.meta.model, probe.meta.latency_ms, probe.delta_count
    );
    println!("输出预览: {}", preview_text(&probe.output, 120));
    Ok(())
}

fn run_connectivity_probe(translator: &dyn Translator) -> Result<ConnectivityProbeResult> {
    const PROBE_INPUT: &str = "Connection check: summarize status in one short sentence.";

    let mut output = String::new();
    let mut delta_count = 0usize;
    let mut on_delta = |delta: &str| {
        if delta.is_empty() {
            return;
        }
        delta_count += 1;
        output.push_str(delta);
    };

    let meta = translator
        .stream_translate(PROBE_INPUT, &mut on_delta)
        .map_err(map_translate_error)?;

    if delta_count == 0 || output.trim().is_empty() {
        return Err(anyhow!("连通性测试失败：未收到流式输出"));
    }

    Ok(ConnectivityProbeResult {
        output,
        delta_count,
        meta,
    })
}

fn preview_text(input: &str, max_chars: usize) -> String {
    let compact = input.replace('\n', " ").trim().to_string();
    if compact.chars().count() <= max_chars {
        return compact;
    }

    let mut clipped = String::new();
    for ch in compact.chars().take(max_chars) {
        clipped.push(ch);
    }
    clipped.push('…');
    clipped
}

fn run_config_wizard() -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(anyhow!(
            "当前不是交互终端，无法进入配置向导。可使用 `tetr config set <key> <value>`"
        ));
    }

    let mut cfg = load_persisted_config()?;
    loop {
        let provider = effective_provider(&cfg);
        println!();
        println!("====== tetr 配置向导 ======");
        println!("当前 Provider: {provider}");
        println!("1) 切换 Provider");
        println!("2) 设置当前 Provider 的模型");
        println!("3) 设置当前 Provider 的接口地址");
        println!("4) 设置当前 Provider 的 API Key");
        println!("5) 设置 UI 布局尺寸（总高度/实时翻译）");
        println!("6) 设置 UI 样式（字号/背景色/透明度/终端列表）");
        println!("7) 设置触发空闲时间（毫秒）");
        println!("8) 设置输出截断参数");
        println!("9) 测试当前模型连通性");
        println!("10) 查看当前配置");
        println!("11) 清除一个配置项");
        println!("12) 显示配置文件路径");
        println!("0) 退出");

        let choice = prompt_line("请选择: ")?;
        match choice.as_str() {
            "1" => {
                configure_provider(&mut cfg)?;
                save_persisted_config(&cfg)?;
            }
            "2" => {
                configure_provider_model(&mut cfg)?;
                save_persisted_config(&cfg)?;
            }
            "3" => {
                configure_provider_base_url(&mut cfg)?;
                save_persisted_config(&cfg)?;
            }
            "4" => {
                configure_provider_api_key(&mut cfg)?;
                save_persisted_config(&cfg)?;
            }
            "5" => {
                configure_ui_heights(&mut cfg)?;
                save_persisted_config(&cfg)?;
            }
            "6" => {
                configure_ui_styles(&mut cfg)?;
                save_persisted_config(&cfg)?;
            }
            "7" => {
                configure_idle_ms(&mut cfg)?;
                save_persisted_config(&cfg)?;
            }
            "8" => {
                configure_truncation(&mut cfg)?;
                save_persisted_config(&cfg)?;
            }
            "9" => {
                run_config_connectivity_test()?;
            }
            "10" => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&masked_config_for_display(&cfg))?
                );
            }
            "11" => {
                configure_unset_key(&mut cfg)?;
                save_persisted_config(&cfg)?;
            }
            "12" => {
                println!("配置文件: {}", config_file_path()?.display());
            }
            "0" | "q" | "quit" | "exit" => {
                println!("已退出配置向导");
                break;
            }
            _ => println!("无效选项，请重试"),
        }
    }
    Ok(())
}

fn effective_provider(cfg: &PersistedConfig) -> String {
    cfg.provider
        .clone()
        .unwrap_or_else(|| "deepseek".to_string())
}

fn configure_provider(cfg: &mut PersistedConfig) -> Result<()> {
    println!("选择 Provider:");
    println!("1) deepseek");
    println!("2) openai-compatible");
    println!("3) mock");
    let selected = prompt_line("输入编号: ")?;
    let provider = match selected.as_str() {
        "1" => "deepseek",
        "2" => "openai-compatible",
        "3" => "mock",
        _ => return Err(anyhow!("无效 Provider 选项")),
    };
    cfg.provider = Some(provider.to_string());
    println!("已设置 provider = {provider}");
    Ok(())
}

fn configure_provider_model(cfg: &mut PersistedConfig) -> Result<()> {
    let provider = effective_provider(cfg);
    let key = provider_model_key(&provider);
    let current = get_config_value(cfg, key)?.unwrap_or_else(|| "未设置".to_string());
    let value = prompt_line(&format!("输入模型（当前 {current}）: "))?;
    if value.is_empty() {
        return Err(anyhow!("模型不能为空"));
    }
    set_config_value(cfg, key, &value)?;
    println!("已设置 {key} = {value}");
    Ok(())
}

fn configure_provider_base_url(cfg: &mut PersistedConfig) -> Result<()> {
    let provider = effective_provider(cfg);
    if provider == "mock" {
        println!("mock provider 不需要接口地址");
        return Ok(());
    }
    let key = provider_base_url_key(&provider);
    let current = get_config_value(cfg, key)?.unwrap_or_else(|| "未设置".to_string());
    let value = prompt_line(&format!("输入接口地址（当前 {current}）: "))?;
    if value.is_empty() {
        return Err(anyhow!("接口地址不能为空"));
    }
    set_config_value(cfg, key, &value)?;
    println!("已设置 {key}");
    Ok(())
}

fn configure_provider_api_key(cfg: &mut PersistedConfig) -> Result<()> {
    let provider = effective_provider(cfg);
    if provider == "mock" {
        println!("mock provider 不需要 API Key");
        return Ok(());
    }
    let key = provider_api_key_key(&provider);
    let current = get_config_value(cfg, key)?.unwrap_or_else(|| "未设置".to_string());
    let value = prompt_line(&format!("输入 API Key（当前 {current}）: "))?;
    if value.is_empty() {
        return Err(anyhow!("API Key 不能为空"));
    }
    set_config_value(cfg, key, &value)?;
    println!("已设置 {key}");
    Ok(())
}

fn configure_idle_ms(cfg: &mut PersistedConfig) -> Result<()> {
    let current = cfg
        .translate_idle_ms
        .map(|v| v.to_string())
        .unwrap_or_else(|| "300(默认)".to_string());
    let value = prompt_line(&format!("输入毫秒值（当前 {current}）: "))?;
    let idle_ms = value.parse::<u64>().context("请输入整数")?;
    if idle_ms < 50 {
        return Err(anyhow!("必须 >= 50"));
    }
    cfg.translate_idle_ms = Some(idle_ms);
    println!("已设置 translate_idle_ms = {idle_ms}");
    Ok(())
}

fn configure_truncation(cfg: &mut PersistedConfig) -> Result<()> {
    println!("设置截断参数（直接回车表示保持当前值）");

    let max_chars_current = cfg
        .truncation_max_chars
        .map(|v| v.to_string())
        .unwrap_or_else(|| "3500(默认)".to_string());
    let max_chars = prompt_line(&format!("truncation_max_chars [{max_chars_current}]: "))?;
    if !max_chars.is_empty() {
        cfg.truncation_max_chars = Some(max_chars.parse::<usize>().context("必须是整数")?);
    }

    let tail_lines_current = cfg
        .truncation_tail_lines
        .map(|v| v.to_string())
        .unwrap_or_else(|| "20(默认)".to_string());
    let tail_lines = prompt_line(&format!("truncation_tail_lines [{tail_lines_current}]: "))?;
    if !tail_lines.is_empty() {
        cfg.truncation_tail_lines = Some(tail_lines.parse::<usize>().context("必须是整数")?);
    }

    let max_error_lines_current = cfg
        .truncation_max_error_lines
        .map(|v| v.to_string())
        .unwrap_or_else(|| "20(默认)".to_string());
    let max_error_lines = prompt_line(&format!(
        "truncation_max_error_lines [{max_error_lines_current}]: "
    ))?;
    if !max_error_lines.is_empty() {
        cfg.truncation_max_error_lines =
            Some(max_error_lines.parse::<usize>().context("必须是整数")?);
    }

    println!("截断参数已更新");
    Ok(())
}

fn configure_unset_key(cfg: &mut PersistedConfig) -> Result<()> {
    let keys = supported_config_keys();
    println!("可清除键：");
    for (index, key) in keys.iter().enumerate() {
        println!("{}) {}", index + 1, key);
    }
    let selected = prompt_line("输入编号或键名: ")?;
    let key = if let Ok(index) = selected.parse::<usize>() {
        if index == 0 || index > keys.len() {
            return Err(anyhow!("编号超出范围"));
        }
        keys[index - 1].to_string()
    } else {
        selected
    };
    unset_config_value(cfg, &key)?;
    println!("已清除: {key}");
    Ok(())
}

fn configure_ui_styles(cfg: &mut PersistedConfig) -> Result<()> {
    println!("设置 UI 样式（直接回车表示保持当前值）");

    let dock_icon_current = cfg
        .ui_dock_icon
        .map(format_bool_switch_state)
        .unwrap_or_else(|| "0(关，默认)".to_string());
    let dock_icon_input = prompt_line(&format!(
        "Dock 图标 ui_dock_icon [{dock_icon_current}]（1=开，0=关）: "
    ))?;
    if !dock_icon_input.is_empty() {
        let parsed = parse_bool_switch_input(&dock_icon_input)
            .ok_or_else(|| anyhow!("ui_dock_icon 仅支持 1 或 0"))?;
        cfg.ui_dock_icon = Some(parsed);
    }

    let font_current = cfg
        .ui_font_size
        .map(|v| v.to_string())
        .unwrap_or_else(|| "11(默认)".to_string());
    let font_input = prompt_line(&format!(
        "字号 ui_font_size [{font_current}]（建议 9-22）: "
    ))?;
    if !font_input.is_empty() {
        let parsed =
            parse_ui_font_size(&font_input).ok_or_else(|| anyhow!("字号仅支持 9-22 的整数"))?;
        cfg.ui_font_size = Some(parsed);
    }

    let bg_current = cfg
        .ui_bg_color
        .clone()
        .unwrap_or_else(|| "#121b2d(默认)".to_string());
    let bg_input = prompt_line(&format!(
        "背景色 ui_bg_color [{bg_current}]（HEX，如 #121b2d）: "
    ))?;
    if !bg_input.is_empty() {
        let parsed = parse_ui_bg_color(&bg_input)
            .ok_or_else(|| anyhow!("ui_bg_color 仅支持 HEX 颜色，如 #121b2d 或 #abc"))?;
        cfg.ui_bg_color = Some(parsed);
    }

    let opacity_current = cfg
        .ui_bg_opacity
        .map(|v| v.to_string())
        .unwrap_or_else(|| "100(默认)".to_string());
    let opacity_input = prompt_line(&format!(
        "背景透明度 ui_bg_opacity [{opacity_current}]（0-100）: "
    ))?;
    if !opacity_input.is_empty() {
        let parsed = parse_ui_bg_opacity(&opacity_input)
            .ok_or_else(|| anyhow!("ui_bg_opacity 仅支持 0-100 的整数"))?;
        cfg.ui_bg_opacity = Some(parsed);
    }

    let terminal_bundle_current = cfg
        .ui_terminal_bundle_ids
        .clone()
        .unwrap_or_else(|| "内置列表(默认)".to_string());
    let terminal_bundle_input = prompt_line(&format!(
        "附加终端 bundle 列表 ui_terminal_bundle_ids [{terminal_bundle_current}]（逗号分隔，输入 - 清空）: "
    ))?;
    if terminal_bundle_input.trim() == "-" {
        cfg.ui_terminal_bundle_ids = None;
    } else if !terminal_bundle_input.is_empty() {
        let parsed = normalize_terminal_bundle_id_list(&terminal_bundle_input).ok_or_else(|| {
            anyhow!("ui_terminal_bundle_ids 仅支持 bundle id 列表，如 com.termius.mac,com.example.Terminal")
        })?;
        cfg.ui_terminal_bundle_ids = Some(parsed);
    }

    println!("UI 样式配置已更新");
    Ok(())
}

fn configure_ui_heights(cfg: &mut PersistedConfig) -> Result<()> {
    println!("设置 UI 高度（像素，直接回车表示保持当前值）");

    let window_current = cfg
        .ui_window_height
        .map(|v| v.to_string())
        .unwrap_or_else(|| "自动(默认)".to_string());
    let window_input = prompt_line(&format!(
        "窗口总高度 ui_window_height [{window_current}]（建议 80-900）: "
    ))?;
    if !window_input.is_empty() {
        let parsed = parse_ui_window_height(&window_input)
            .ok_or_else(|| anyhow!("ui_window_height 仅支持 80-900 的整数"))?;
        cfg.ui_window_height = Some(parsed);
    }

    let realtime_current = cfg
        .ui_realtime_height
        .map(|v| v.to_string())
        .unwrap_or_else(|| "自动(默认)".to_string());
    let realtime_input = prompt_line(&format!(
        "实时翻译区高度 ui_realtime_height [{realtime_current}]（建议 80-900）: "
    ))?;
    if !realtime_input.is_empty() {
        let parsed = parse_ui_window_height(&realtime_input)
            .ok_or_else(|| anyhow!("ui_realtime_height 仅支持 80-900 的整数"))?;
        cfg.ui_realtime_height = Some(parsed);
    }

    println!("UI 高度配置已更新");
    Ok(())
}

fn prompt_line(prompt: &str) -> Result<String> {
    print!("{prompt}");
    io::stdout().flush().context("输出提示失败")?;

    let mut line = String::new();
    io::stdin().read_line(&mut line).context("读取输入失败")?;
    Ok(line.trim().to_string())
}

fn provider_model_key(provider: &str) -> &'static str {
    match provider {
        "openai" | "openai-compatible" | "compatible" => "openai_model",
        "mock" => "model",
        _ => "deepseek_model",
    }
}

fn provider_base_url_key(provider: &str) -> &'static str {
    match provider {
        "openai" | "openai-compatible" | "compatible" => "openai_base_url",
        _ => "deepseek_base_url",
    }
}

fn provider_api_key_key(provider: &str) -> &'static str {
    match provider {
        "openai" | "openai-compatible" | "compatible" => "openai_api_key",
        _ => "deepseek_api_key",
    }
}

fn config_file_path() -> Result<PathBuf> {
    if cfg!(windows) {
        if let Some(appdata) = env::var_os("APPDATA") {
            return Ok(PathBuf::from(appdata).join("tetr").join("config.json"));
        }
    }

    if let Some(xdg) = env::var_os("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(xdg).join("tetr").join("config.json"));
    }

    if let Some(home) = home_dir() {
        return Ok(home.join(".config").join("tetr").join("config.json"));
    }

    Err(anyhow!(
        "无法确定配置目录，请设置 HOME / XDG_CONFIG_HOME / APPDATA"
    ))
}

fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("USERPROFILE").map(PathBuf::from))
}

fn load_persisted_config() -> Result<PersistedConfig> {
    let path = config_file_path()?;
    if !path.exists() {
        return Ok(PersistedConfig::default());
    }
    let raw =
        fs::read_to_string(&path).with_context(|| format!("读取配置失败: {}", path.display()))?;
    serde_json::from_str(&raw).with_context(|| format!("配置文件格式错误: {}", path.display()))
}

fn save_persisted_config(cfg: &PersistedConfig) -> Result<()> {
    let path = config_file_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("创建目录失败: {}", parent.display()))?;
    }
    let content = serde_json::to_string_pretty(cfg)?;
    fs::write(&path, content).with_context(|| format!("写入配置失败: {}", path.display()))
}

fn normalize_config_key(key: &str) -> String {
    key.trim().to_ascii_lowercase().replace(['.', '-'], "_")
}

fn set_config_value(cfg: &mut PersistedConfig, key: &str, value: &str) -> Result<()> {
    let key = normalize_config_key(key);
    let value = value.trim();
    if value.is_empty() {
        return Err(anyhow!("配置值不能为空"));
    }

    match key.as_str() {
        "provider" => cfg.provider = Some(value.to_ascii_lowercase()),
        "translate_idle_ms" | "idle_ms" | "idle" => {
            cfg.translate_idle_ms = Some(value.parse::<u64>().context("idle_ms 必须是整数")?)
        }
        "deepseek_api_key" => cfg.deepseek_api_key = Some(value.to_string()),
        "deepseek_base_url" => cfg.deepseek_base_url = Some(value.to_string()),
        "deepseek_model" => cfg.deepseek_model = Some(value.to_string()),
        "openai_api_key" => cfg.openai_api_key = Some(value.to_string()),
        "openai_base_url" => cfg.openai_base_url = Some(value.to_string()),
        "openai_model" => cfg.openai_model = Some(value.to_string()),
        "api_key" => cfg.api_key = Some(value.to_string()),
        "api_base_url" => cfg.api_base_url = Some(value.to_string()),
        "model" => cfg.model = Some(value.to_string()),
        "truncation_max_chars" => {
            cfg.truncation_max_chars = Some(value.parse::<usize>().context("必须是整数")?)
        }
        "truncation_tail_lines" => {
            cfg.truncation_tail_lines = Some(value.parse::<usize>().context("必须是整数")?)
        }
        "truncation_max_error_lines" => {
            cfg.truncation_max_error_lines = Some(value.parse::<usize>().context("必须是整数")?)
        }
        "ui_bin" => cfg.ui_bin = Some(value.to_string()),
        "ui_terminal_bundle_ids" => {
            cfg.ui_terminal_bundle_ids = Some(
                normalize_terminal_bundle_id_list(value).ok_or_else(|| {
                    anyhow!(
                        "ui_terminal_bundle_ids 仅支持 bundle id 列表，如 com.termius.mac,com.googlecode.iterm2"
                    )
                })?,
            )
        }
        "ui_dock_icon" => {
            cfg.ui_dock_icon = Some(
                parse_bool_value(value)
                    .ok_or_else(|| anyhow!("ui_dock_icon 仅支持 1/0（兼容 true/false on/off）"))?,
            )
        }
        "ui_font_size" | "ui_font_size_px" => {
            cfg.ui_font_size = Some(
                parse_ui_font_size(value)
                    .ok_or_else(|| anyhow!("ui_font_size 仅支持 9-22 的整数"))?,
            )
        }
        "ui_bg_color" | "ui_background_color" => {
            cfg.ui_bg_color = Some(
                parse_ui_bg_color(value)
                    .ok_or_else(|| anyhow!("ui_bg_color 仅支持 HEX 颜色，如 #121b2d 或 #abc"))?,
            )
        }
        "ui_bg_opacity" | "ui_background_opacity" => {
            cfg.ui_bg_opacity = Some(
                parse_ui_bg_opacity(value)
                    .ok_or_else(|| anyhow!("ui_bg_opacity 仅支持 0-100 的整数"))?,
            )
        }
        "ui_window_height" | "ui_window_height_px" => {
            cfg.ui_window_height = Some(
                parse_ui_window_height(value)
                    .ok_or_else(|| anyhow!("ui_window_height 仅支持 80-900 的整数"))?,
            )
        }
        "ui_realtime_height" | "ui_realtime_height_px" => {
            cfg.ui_realtime_height = Some(
                parse_ui_window_height(value)
                    .ok_or_else(|| anyhow!("ui_realtime_height 仅支持 80-900 的整数"))?,
            )
        }
        _ => {
            return Err(anyhow!(
                "不支持的配置键: {key}。可用键: {}",
                supported_config_keys().join(", ")
            ));
        }
    }

    Ok(())
}

fn unset_config_value(cfg: &mut PersistedConfig, key: &str) -> Result<()> {
    let key = normalize_config_key(key);
    match key.as_str() {
        "provider" => cfg.provider = None,
        "translate_idle_ms" | "idle_ms" | "idle" => cfg.translate_idle_ms = None,
        "deepseek_api_key" => cfg.deepseek_api_key = None,
        "deepseek_base_url" => cfg.deepseek_base_url = None,
        "deepseek_model" => cfg.deepseek_model = None,
        "openai_api_key" => cfg.openai_api_key = None,
        "openai_base_url" => cfg.openai_base_url = None,
        "openai_model" => cfg.openai_model = None,
        "api_key" => cfg.api_key = None,
        "api_base_url" => cfg.api_base_url = None,
        "model" => cfg.model = None,
        "truncation_max_chars" => cfg.truncation_max_chars = None,
        "truncation_tail_lines" => cfg.truncation_tail_lines = None,
        "truncation_max_error_lines" => cfg.truncation_max_error_lines = None,
        "ui_bin" => cfg.ui_bin = None,
        "ui_terminal_bundle_ids" => cfg.ui_terminal_bundle_ids = None,
        "ui_dock_icon" => cfg.ui_dock_icon = None,
        "ui_font_size" | "ui_font_size_px" => cfg.ui_font_size = None,
        "ui_bg_color" | "ui_background_color" => cfg.ui_bg_color = None,
        "ui_bg_opacity" | "ui_background_opacity" => cfg.ui_bg_opacity = None,
        "ui_window_height" | "ui_window_height_px" => cfg.ui_window_height = None,
        "ui_realtime_height" | "ui_realtime_height_px" => cfg.ui_realtime_height = None,
        _ => {
            return Err(anyhow!(
                "不支持的配置键: {key}。可用键: {}",
                supported_config_keys().join(", ")
            ));
        }
    }
    Ok(())
}

fn get_config_value(cfg: &PersistedConfig, key: &str) -> Result<Option<String>> {
    let key = normalize_config_key(key);
    let value = match key.as_str() {
        "provider" => cfg.provider.clone(),
        "translate_idle_ms" | "idle_ms" | "idle" => cfg.translate_idle_ms.map(|v| v.to_string()),
        "deepseek_api_key" => cfg.deepseek_api_key.clone().map(mask_secret),
        "deepseek_base_url" => cfg.deepseek_base_url.clone(),
        "deepseek_model" => cfg.deepseek_model.clone(),
        "openai_api_key" => cfg.openai_api_key.clone().map(mask_secret),
        "openai_base_url" => cfg.openai_base_url.clone(),
        "openai_model" => cfg.openai_model.clone(),
        "api_key" => cfg.api_key.clone().map(mask_secret),
        "api_base_url" => cfg.api_base_url.clone(),
        "model" => cfg.model.clone(),
        "truncation_max_chars" => cfg.truncation_max_chars.map(|v| v.to_string()),
        "truncation_tail_lines" => cfg.truncation_tail_lines.map(|v| v.to_string()),
        "truncation_max_error_lines" => cfg.truncation_max_error_lines.map(|v| v.to_string()),
        "ui_bin" => cfg.ui_bin.clone(),
        "ui_terminal_bundle_ids" => cfg.ui_terminal_bundle_ids.clone(),
        "ui_dock_icon" => cfg.ui_dock_icon.map(|v| v.to_string()),
        "ui_font_size" | "ui_font_size_px" => cfg.ui_font_size.map(|v| v.to_string()),
        "ui_bg_color" | "ui_background_color" => cfg.ui_bg_color.clone(),
        "ui_bg_opacity" | "ui_background_opacity" => cfg.ui_bg_opacity.map(|v| v.to_string()),
        "ui_window_height" | "ui_window_height_px" => cfg.ui_window_height.map(|v| v.to_string()),
        "ui_realtime_height" | "ui_realtime_height_px" => {
            cfg.ui_realtime_height.map(|v| v.to_string())
        }
        _ => {
            return Err(anyhow!(
                "不支持的配置键: {key}。可用键: {}",
                supported_config_keys().join(", ")
            ));
        }
    };
    Ok(value)
}

fn mask_secret(raw: String) -> String {
    if raw.chars().count() <= 6 {
        return "******".to_string();
    }
    let tail: String = raw
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<char>>()
        .into_iter()
        .rev()
        .collect();
    format!("******{tail}")
}

fn masked_config_for_display(cfg: &PersistedConfig) -> serde_json::Value {
    json!({
        "provider": cfg.provider,
        "translate_idle_ms": cfg.translate_idle_ms,
        "deepseek_api_key": cfg.deepseek_api_key.clone().map(mask_secret),
        "deepseek_base_url": cfg.deepseek_base_url,
        "deepseek_model": cfg.deepseek_model,
        "openai_api_key": cfg.openai_api_key.clone().map(mask_secret),
        "openai_base_url": cfg.openai_base_url,
        "openai_model": cfg.openai_model,
        "api_key": cfg.api_key.clone().map(mask_secret),
        "api_base_url": cfg.api_base_url,
        "model": cfg.model,
        "truncation_max_chars": cfg.truncation_max_chars,
        "truncation_tail_lines": cfg.truncation_tail_lines,
        "truncation_max_error_lines": cfg.truncation_max_error_lines,
        "ui_bin": cfg.ui_bin,
        "ui_terminal_bundle_ids": cfg.ui_terminal_bundle_ids,
        "ui_dock_icon": cfg.ui_dock_icon,
        "ui_font_size": cfg.ui_font_size,
        "ui_bg_color": cfg.ui_bg_color,
        "ui_bg_opacity": cfg.ui_bg_opacity,
        "ui_window_height": cfg.ui_window_height,
        "ui_realtime_height": cfg.ui_realtime_height,
    })
}

fn supported_config_keys() -> Vec<&'static str> {
    vec![
        "provider",
        "translate_idle_ms",
        "deepseek_api_key",
        "deepseek_base_url",
        "deepseek_model",
        "openai_api_key",
        "openai_base_url",
        "openai_model",
        "api_key",
        "api_base_url",
        "model",
        "truncation_max_chars",
        "truncation_tail_lines",
        "truncation_max_error_lines",
        "ui_bin",
        "ui_terminal_bundle_ids",
        "ui_dock_icon",
        "ui_font_size",
        "ui_bg_color",
        "ui_bg_opacity",
        "ui_window_height",
        "ui_realtime_height",
    ]
}

fn handle_output_chunk(
    capture: &mut CaptureState,
    chunk: OutputChunk,
    translator: &dyn Translator,
    truncation_config: &TruncationConfig,
    ipc: Option<&IpcServer>,
) {
    let now = Instant::now();
    if let Some(triggered) = capture.ingest_chunk(&chunk.raw, &chunk.clean, now) {
        process_triggered_capture(triggered, translator, truncation_config, ipc);
    }
}

fn process_triggered_capture(
    triggered: TriggeredCapture,
    translator: &dyn Translator,
    truncation_config: &TruncationConfig,
    ipc: Option<&IpcServer>,
) {
    let truncated = truncate_for_translation(&triggered.text, truncation_config);
    if truncated.text.trim().is_empty() {
        return;
    }

    if !should_translate_text(&truncated.text) {
        return;
    }

    if let Some(ipc) = ipc {
        let started_payload = build_translation_started_payload(
            triggered.reason,
            truncated.truncated,
            truncated.original_chars,
            truncated.text.chars().count(),
            &truncated.text,
        );
        ipc.send_event("translation.started", started_payload);
    }

    let mut assembled = String::new();
    let mut emit_delta = |delta: &str| {
        assembled.push_str(delta);
        if let Some(ipc) = ipc {
            ipc.send_event("translation.delta", json!({ "delta": delta }));
        }
    };

    match translator.stream_translate(&truncated.text, &mut emit_delta) {
        Ok(mut meta) => {
            meta.truncated = truncated.truncated;
            let aligned_translation = align_translation_line_layout(&truncated.text, &assembled);
            if let Some(ipc) = ipc {
                ipc.send_event(
                    "translation.done",
                    json!({
                        "translation": aligned_translation,
                        "meta": meta,
                    }),
                );
            } else {
                eprintln!("\n[tetr][translation]\n{}\n", aligned_translation);
            }
        }
        Err(err) => {
            report_translation_error(err, ipc);
        }
    }
}

fn report_translation_error(err: TranslateError, ipc: Option<&IpcServer>) {
    if let Some(ipc) = ipc {
        ipc.send_event(
            "session.error",
            json!({
                "message": err.to_string(),
            }),
        );
    } else {
        eprintln!("[tetr] translation error: {err}");
    }
}

fn trigger_reason_text(reason: TriggerReason) -> &'static str {
    match reason {
        TriggerReason::Prompt => "prompt",
        TriggerReason::Idle => "idle",
    }
}

fn build_translation_started_payload(
    reason: TriggerReason,
    truncated: bool,
    original_chars: usize,
    selected_chars: usize,
    source: &str,
) -> Value {
    json!({
        "reason": trigger_reason_text(reason),
        "truncated": truncated,
        "originalChars": original_chars,
        "selectedChars": selected_chars,
        "source": source,
    })
}

fn build_session_started_payload(cfg: &AppConfig) -> Value {
    let mut payload = serde_json::Map::new();
    payload.insert("provider".to_string(), json!(cfg.provider));
    if let Some(font_size) = cfg.ui_font_size {
        payload.insert("uiFontSizePx".to_string(), json!(font_size));
    }
    if let Some(bg_color) = cfg.ui_bg_color.as_ref() {
        payload.insert("uiBgColor".to_string(), json!(bg_color));
    }
    if let Some(bg_opacity) = cfg.ui_bg_opacity {
        payload.insert("uiBgOpacityPercent".to_string(), json!(bg_opacity));
    }
    if let Some(height) = cfg.ui_window_height {
        payload.insert("uiWindowHeightPx".to_string(), json!(height));
    }
    if let Some(height) = cfg.ui_realtime_height {
        payload.insert("uiRealtimeHeightPx".to_string(), json!(height));
    }
    Value::Object(payload)
}

fn should_translate_text(input: &str) -> bool {
    if input
        .chars()
        .any(|ch| ('\u{4E00}'..='\u{9FFF}').contains(&ch))
    {
        return false;
    }

    input.chars().any(|ch| ch.is_ascii_alphabetic())
}

fn align_translation_line_layout(source: &str, translation: &str) -> String {
    let source_lines: Vec<String> = source
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect();

    let translation_lines: Vec<String> = translation
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect();

    if source_lines.len() <= 1 || translation_lines.is_empty() {
        return translation.trim().to_string();
    }

    if translation_lines.len() < source_lines.len() {
        let merged = translation_lines.join(" ");
        return split_single_line_by_source_weights(&merged, &source_lines).join("\n");
    }

    if translation_lines.len() == source_lines.len() {
        return translation_lines.join("\n");
    }

    if translation_lines.len() > source_lines.len() {
        let rows = bucketize_lines(&translation_lines, source_lines.len())
            .into_iter()
            .map(|bucket| bucket.join(" ").trim().to_string())
            .collect::<Vec<String>>();
        return rows.join("\n");
    }

    split_single_line_by_source_weights(&translation_lines[0], &source_lines).join("\n")
}

fn bucketize_lines(items: &[String], bucket_count: usize) -> Vec<Vec<String>> {
    let mut buckets = vec![Vec::new(); bucket_count];
    if bucket_count == 0 || items.is_empty() {
        return buckets;
    }

    for (index, item) in items.iter().enumerate() {
        let bucket_index = ((index * bucket_count) / items.len()).min(bucket_count - 1);
        buckets[bucket_index].push(item.clone());
    }

    buckets
}

fn split_single_line_by_source_weights(text: &str, source_lines: &[String]) -> Vec<String> {
    if source_lines.is_empty() {
        return vec![text.trim().to_string()];
    }

    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return vec![String::new(); source_lines.len()];
    }

    let weights: Vec<usize> = source_lines
        .iter()
        .map(|line| line.chars().count().max(1))
        .collect();
    let mut remaining_weight: usize = weights.iter().sum();
    let mut cursor = 0usize;
    let mut result: Vec<String> = Vec::with_capacity(source_lines.len());

    for (idx, weight) in weights.iter().enumerate() {
        if idx == weights.len() - 1 {
            let tail: String = chars[cursor..].iter().collect();
            result.push(tail.trim().to_string());
            break;
        }

        let remaining_chars = chars.len().saturating_sub(cursor);
        let lines_left_after = weights.len().saturating_sub(idx + 1);
        if remaining_chars <= lines_left_after {
            let one_char = chars
                .get(cursor)
                .copied()
                .map(|ch| ch.to_string())
                .unwrap_or_default();
            result.push(one_char.trim().to_string());
            cursor = (cursor + 1).min(chars.len());
            remaining_weight = remaining_weight.saturating_sub(*weight);
            continue;
        }

        let desired_len = ((remaining_chars as f64) * (*weight as f64 / remaining_weight as f64))
            .round() as usize;
        let max_len = remaining_chars - lines_left_after;
        let target_len = desired_len.clamp(1, max_len);
        let split_at = find_nearest_split_index(&chars, cursor, cursor + target_len);

        let piece: String = chars[cursor..split_at].iter().collect();
        result.push(piece.trim().to_string());
        cursor = split_at;
        remaining_weight = remaining_weight.saturating_sub(*weight);
    }

    while result.len() < source_lines.len() {
        result.push(String::new());
    }

    result
}

fn find_nearest_split_index(chars: &[char], start: usize, desired: usize) -> usize {
    if desired >= chars.len() {
        return chars.len();
    }

    let desired = desired.max(start + 1);
    const WINDOW: usize = 8;

    for offset in 0..=WINDOW {
        let right = desired.saturating_add(offset);
        if right < chars.len() && right > start && is_split_boundary(chars[right - 1]) {
            return right;
        }

        let left = desired.saturating_sub(offset);
        if left > start && left < chars.len() && is_split_boundary(chars[left - 1]) {
            return left;
        }
    }

    desired.min(chars.len())
}

fn is_split_boundary(ch: char) -> bool {
    matches!(
        ch,
        ' ' | '\t'
            | ','
            | '.'
            | '!'
            | '?'
            | ';'
            | ':'
            | '，'
            | '。'
            | '！'
            | '？'
            | '；'
            | '：'
            | '、'
    )
}

fn parse_bool_value(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "开" | "是" => Some(true),
        "关" | "否" => Some(false),
        "1" | "true" | "yes" | "on" | "enable" | "enabled" => Some(true),
        "0" | "false" | "no" | "off" | "disable" | "disabled" => Some(false),
        _ => None,
    }
}

fn parse_bool_switch_input(value: &str) -> Option<bool> {
    match value.trim() {
        "1" => Some(true),
        "0" => Some(false),
        _ => None,
    }
}

fn format_bool_switch_state(enabled: bool) -> String {
    if enabled {
        "1(开)".to_string()
    } else {
        "0(关)".to_string()
    }
}

fn parse_ui_font_size(value: &str) -> Option<u16> {
    let parsed = value.trim().parse::<u16>().ok()?;
    if (9..=22).contains(&parsed) {
        Some(parsed)
    } else {
        None
    }
}

fn parse_ui_bg_color(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }

    let with_hash = if trimmed.starts_with('#') {
        trimmed.to_string()
    } else {
        format!("#{trimmed}")
    };

    let raw = with_hash.strip_prefix('#')?;
    let normalized = match raw.len() {
        3 => {
            if !raw.chars().all(|ch| ch.is_ascii_hexdigit()) {
                return None;
            }
            let mut expanded = String::with_capacity(6);
            for ch in raw.chars() {
                expanded.push(ch);
                expanded.push(ch);
            }
            expanded
        }
        6 => {
            if !raw.chars().all(|ch| ch.is_ascii_hexdigit()) {
                return None;
            }
            raw.to_string()
        }
        _ => return None,
    };

    Some(format!("#{}", normalized.to_ascii_lowercase()))
}

fn parse_ui_bg_opacity(value: &str) -> Option<u8> {
    let parsed = value.trim().parse::<u8>().ok()?;
    if parsed <= 100 {
        Some(parsed)
    } else {
        None
    }
}

fn parse_ui_window_height(value: &str) -> Option<u16> {
    let parsed = value.trim().parse::<u16>().ok()?;
    if (80..=900).contains(&parsed) {
        Some(parsed)
    } else {
        None
    }
}

fn normalize_terminal_bundle_id_list(value: &str) -> Option<String> {
    let mut seen = HashSet::new();
    let mut normalized = Vec::new();

    for token in value.split(|ch| matches!(ch, ',' | ';' | '\n')) {
        let trimmed = token.trim();
        if trimmed.is_empty() {
            continue;
        }

        if !is_valid_terminal_bundle_id_token(trimmed) {
            return None;
        }

        let lowered = trimmed.to_ascii_lowercase();
        if seen.insert(lowered.clone()) {
            normalized.push(lowered);
        }
    }

    if normalized.is_empty() {
        None
    } else {
        Some(normalized.join(","))
    }
}

fn is_valid_terminal_bundle_id_token(value: &str) -> bool {
    value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_'))
}

fn detect_terminal_size() -> (u16, u16) {
    let Some((Width(cols), Height(rows))) = terminal_size() else {
        return (80, 24);
    };

    (cols, rows)
}

fn choose_shell() -> String {
    if cfg!(windows) {
        if command_exists("pwsh.exe") {
            "pwsh.exe".to_string()
        } else {
            "powershell.exe".to_string()
        }
    } else {
        env::var("SHELL")
            .ok()
            .filter(|shell| !shell.trim().is_empty())
            .unwrap_or_else(|| "zsh".to_string())
    }
}

fn command_exists(binary: &str) -> bool {
    let Some(path_var) = env::var_os("PATH") else {
        return false;
    };

    env::split_paths(&path_var).any(|path| {
        let direct = path.join(binary);
        if direct.is_file() {
            return true;
        }

        if cfg!(windows) {
            return path.join(format!("{binary}.exe")).is_file();
        }

        false
    })
}

fn ui_binary_name() -> &'static str {
    if cfg!(windows) {
        "tetr-ui.exe"
    } else {
        "tetr-ui"
    }
}

fn spawn_ui_process(
    port: u16,
    configured_ui_bin: Option<&str>,
    ui_window_height: Option<u16>,
    ui_dock_icon: Option<bool>,
    ui_terminal_bundle_ids: Option<&str>,
) -> Result<Child> {
    let mut candidates: Vec<PathBuf> = Vec::new();

    if let Some(path) = configured_ui_bin {
        candidates.push(PathBuf::from(path));
    }

    if let Ok(path) = env::var("TETR_UI_BIN") {
        candidates.push(PathBuf::from(path));
    }

    if let Ok(current_exe) = env::current_exe() {
        if let Some(dir) = current_exe.parent() {
            candidates.push(dir.join(ui_binary_name()));
        }
    }

    candidates.push(PathBuf::from(ui_binary_name()));

    for candidate in candidates {
        let mut command = Command::new(&candidate);
        command.env("TETR_IPC_PORT", port.to_string());
        if let Some(height) = ui_window_height {
            command.env("TETR_UI_WINDOW_HEIGHT", height.to_string());
        }
        if let Some(dock_icon_enabled) = ui_dock_icon {
            command.env(
                "TETR_UI_DOCK_ICON",
                if dock_icon_enabled { "1" } else { "0" },
            );
        }
        if let Some(bundle_ids) = ui_terminal_bundle_ids {
            command.env("TETR_UI_TERMINAL_BUNDLE_IDS", bundle_ids);
        }
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        if let Ok(child) = command.spawn() {
            return Ok(child);
        }
    }

    Err(anyhow!(
        "unable to launch UI binary. set TETR_UI_BIN to the tetr-ui executable path"
    ))
}

#[cfg(test)]
mod tests {
    use super::{
        align_translation_line_layout, build_session_started_payload,
        build_translation_started_payload, parse_bool_switch_input, parse_bool_value,
        normalize_terminal_bundle_id_list, parse_ui_bg_color, parse_ui_bg_opacity,
        parse_ui_font_size, parse_ui_window_height, run_connectivity_probe, set_config_value,
        should_translate_text, supported_config_keys, AppConfig, PersistedConfig, TriggerReason,
    };
    use tetr_core::translator::mock::MockTranslator;
    use tetr_core::translator::{TranslateError, TranslationMeta, Translator};

    #[test]
    fn translation_started_payload_includes_source_excerpt() {
        let payload = build_translation_started_payload(
            TriggerReason::Prompt,
            true,
            3200,
            540,
            "line1\nline2",
        );

        assert_eq!(
            payload.get("reason").and_then(|v| v.as_str()),
            Some("prompt")
        );
        assert_eq!(
            payload.get("truncated").and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            payload.get("originalChars").and_then(|v| v.as_u64()),
            Some(3200)
        );
        assert_eq!(
            payload.get("selectedChars").and_then(|v| v.as_u64()),
            Some(540)
        );
        assert_eq!(
            payload.get("source").and_then(|v| v.as_str()),
            Some("line1\nline2")
        );
    }

    #[test]
    fn skips_translation_for_chinese_only_text() {
        assert!(!should_translate_text("这是中文输出，不需要翻译"));
        assert!(!should_translate_text("✅ 构建成功，耗时 3 秒"));
    }

    #[test]
    fn translates_when_english_is_present() {
        assert!(should_translate_text("Error: file not found"));
        assert!(!should_translate_text("请求失败，请 retry with sudo"));
    }

    #[test]
    fn aligns_translation_line_count_with_source_when_model_merges_lines() {
        let source = "line one\nline two\nline three";
        let translation = "第一行的翻译第二行的翻译第三行的翻译";

        let aligned = align_translation_line_layout(source, translation);
        let aligned_lines: Vec<&str> = aligned.split('\n').collect();

        assert_eq!(aligned_lines.len(), 3);
        assert_eq!(aligned.replace('\n', ""), translation);
    }

    #[test]
    fn aligns_multi_line_translation_into_source_layout() {
        let source = "a\nb\nc\nd";
        let translation = "甲\n乙";

        let aligned = align_translation_line_layout(source, translation);
        let aligned_lines: Vec<&str> = aligned.split('\n').collect();

        assert_eq!(aligned_lines.len(), 4);
        assert_eq!(aligned.replace('\n', ""), "甲乙");
    }

    #[test]
    fn parses_boolean_values_for_config() {
        assert_eq!(parse_bool_value("on"), Some(true));
        assert_eq!(parse_bool_value("false"), Some(false));
        assert_eq!(parse_bool_value("unknown"), None);
    }

    #[test]
    fn parses_numeric_switch_values_for_wizard() {
        assert_eq!(parse_bool_switch_input("1"), Some(true));
        assert_eq!(parse_bool_switch_input("0"), Some(false));
        assert_eq!(parse_bool_switch_input("开"), None);
    }

    #[test]
    fn rejects_removed_history_and_source_config_keys() {
        let mut cfg = PersistedConfig::default();
        assert!(set_config_value(&mut cfg, "ui_history_enabled", "1").is_err());
        assert!(set_config_value(&mut cfg, "ui_source_enabled", "1").is_err());
        assert!(set_config_value(&mut cfg, "ui_history_height", "120").is_err());
        assert!(!supported_config_keys().contains(&"ui_history_enabled"));
        assert!(!supported_config_keys().contains(&"ui_source_enabled"));
        assert!(!supported_config_keys().contains(&"ui_history_height"));
    }

    #[test]
    fn supports_terminal_bundle_override_config_key() {
        let mut cfg = PersistedConfig::default();
        set_config_value(
            &mut cfg,
            "ui_terminal_bundle_ids",
            "com.termius.mac,com.googlecode.iterm2",
        )
        .expect("set should succeed");
        assert_eq!(
            cfg.ui_terminal_bundle_ids.as_deref(),
            Some("com.termius.mac,com.googlecode.iterm2")
        );
        assert!(supported_config_keys().contains(&"ui_terminal_bundle_ids"));
    }

    #[test]
    fn normalizes_terminal_bundle_override_list() {
        assert_eq!(
            normalize_terminal_bundle_id_list(
                " com.Termius.Mac , com.googlecode.iTerm2 ;\ncom.Termius.Mac "
            ),
            Some("com.termius.mac,com.googlecode.iterm2".to_string())
        );
        assert_eq!(normalize_terminal_bundle_id_list(""), None);
        assert_eq!(
            normalize_terminal_bundle_id_list("com.termius.mac,*invalid*"),
            None
        );
    }

    #[test]
    fn parses_chinese_boolean_values_for_config() {
        assert_eq!(parse_bool_value("开"), Some(true));
        assert_eq!(parse_bool_value("关"), Some(false));
        assert_eq!(parse_bool_value("是"), Some(true));
        assert_eq!(parse_bool_value("否"), Some(false));
    }

    #[test]
    fn parses_ui_font_size_with_bounds() {
        assert_eq!(parse_ui_font_size("10"), Some(10));
        assert_eq!(parse_ui_font_size("22"), Some(22));
        assert_eq!(parse_ui_font_size("8"), None);
        assert_eq!(parse_ui_font_size("99"), None);
    }

    #[test]
    fn parses_ui_window_height_with_bounds() {
        assert_eq!(parse_ui_window_height("220"), Some(220));
        assert_eq!(parse_ui_window_height("900"), Some(900));
        assert_eq!(parse_ui_window_height("70"), None);
        assert_eq!(parse_ui_window_height("1200"), None);
    }

    #[test]
    fn parses_ui_background_color_hex() {
        assert_eq!(parse_ui_bg_color("#112233"), Some("#112233".to_string()));
        assert_eq!(parse_ui_bg_color("abc"), Some("#aabbcc".to_string()));
        assert_eq!(parse_ui_bg_color("#xyz"), None);
    }

    #[test]
    fn parses_ui_background_opacity() {
        assert_eq!(parse_ui_bg_opacity("0"), Some(0));
        assert_eq!(parse_ui_bg_opacity("80"), Some(80));
        assert_eq!(parse_ui_bg_opacity("100"), Some(100));
        assert_eq!(parse_ui_bg_opacity("101"), None);
    }

    #[test]
    fn supports_ui_height_config_keys() {
        let mut cfg = PersistedConfig::default();
        set_config_value(&mut cfg, "ui_window_height", "260").expect("set should succeed");
        set_config_value(&mut cfg, "ui_realtime_height", "180").expect("set should succeed");

        assert_eq!(cfg.ui_window_height, Some(260));
        assert_eq!(cfg.ui_realtime_height, Some(180));
        assert!(supported_config_keys().contains(&"ui_window_height"));
        assert!(supported_config_keys().contains(&"ui_realtime_height"));
    }

    #[test]
    fn supports_ui_style_config_keys() {
        let mut cfg = PersistedConfig::default();
        set_config_value(&mut cfg, "ui_bg_color", "#123456").expect("set should succeed");
        set_config_value(&mut cfg, "ui_bg_opacity", "82").expect("set should succeed");

        assert_eq!(cfg.ui_bg_color.as_deref(), Some("#123456"));
        assert_eq!(cfg.ui_bg_opacity, Some(82));
        assert!(supported_config_keys().contains(&"ui_bg_color"));
        assert!(supported_config_keys().contains(&"ui_bg_opacity"));
    }

    #[test]
    fn supports_ui_dock_icon_config_key() {
        let mut cfg = PersistedConfig::default();
        set_config_value(&mut cfg, "ui_dock_icon", "on").expect("set should succeed");
        assert_eq!(cfg.ui_dock_icon, Some(true));

        set_config_value(&mut cfg, "ui_dock_icon", "off").expect("set should succeed");
        assert_eq!(cfg.ui_dock_icon, Some(false));
        assert!(supported_config_keys().contains(&"ui_dock_icon"));
    }

    #[test]
    fn session_started_payload_includes_ui_font_size() {
        let cfg = AppConfig {
            provider: "openai-compatible".to_string(),
            idle_ms: 300,
            truncation: Default::default(),
            deepseek_api_key: None,
            deepseek_base_url: String::new(),
            deepseek_model: String::new(),
            openai_api_key: None,
            openai_base_url: String::new(),
            openai_model: String::new(),
            ui_bin: None,
            ui_terminal_bundle_ids: None,
            ui_dock_icon: Some(false),
            ui_font_size: Some(10),
            ui_bg_color: Some("#123456".to_string()),
            ui_bg_opacity: Some(82),
            ui_window_height: Some(300),
            ui_realtime_height: Some(180),
        };

        let payload = build_session_started_payload(&cfg);
        assert_eq!(
            payload.get("uiFontSizePx").and_then(|v| v.as_u64()),
            Some(10)
        );
        assert_eq!(
            payload.get("uiBgColor").and_then(|v| v.as_str()),
            Some("#123456")
        );
        assert_eq!(
            payload.get("uiBgOpacityPercent").and_then(|v| v.as_u64()),
            Some(82)
        );
        assert_eq!(
            payload.get("uiWindowHeightPx").and_then(|v| v.as_u64()),
            Some(300)
        );
        assert_eq!(
            payload.get("uiRealtimeHeightPx").and_then(|v| v.as_u64()),
            Some(180)
        );
    }

    #[test]
    fn connectivity_probe_succeeds_with_streaming_output() {
        let translator = MockTranslator;
        let result = run_connectivity_probe(&translator).expect("probe should succeed");

        assert!(result.delta_count > 0);
        assert!(!result.output.trim().is_empty());
        assert_eq!(result.meta.provider, "mock");
    }

    #[test]
    fn connectivity_probe_fails_without_deltas() {
        struct SilentTranslator;

        impl Translator for SilentTranslator {
            fn provider_name(&self) -> &'static str {
                "silent"
            }

            fn stream_translate(
                &self,
                input: &str,
                _on_delta: &mut dyn FnMut(&str),
            ) -> Result<TranslationMeta, TranslateError> {
                Ok(TranslationMeta {
                    provider: self.provider_name().to_string(),
                    model: "silent-model".to_string(),
                    input_chars: input.chars().count(),
                    output_chars: 0,
                    latency_ms: 1,
                    truncated: false,
                })
            }
        }

        let translator = SilentTranslator;
        let error = run_connectivity_probe(&translator).expect_err("probe should fail");
        assert!(error.to_string().contains("未收到流式输出"));
    }
}
