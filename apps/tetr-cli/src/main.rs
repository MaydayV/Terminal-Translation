use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use crossbeam_channel::{unbounded, Receiver, Sender};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
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
use tetr_core::translator::{TranslateError, Translator};
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
            Ok(ipc) => match spawn_ui_process(ipc.port(), cfg.ui_bin.as_deref()) {
                Ok(child) => {
                    ui_child = Some(child);
                    ipc.send_event("session.started", json!({ "provider": cfg.provider }));
                    ipc_server = Some(ipc);
                }
                Err(err) => {
                    eprintln!("[tetr] failed to spawn UI: {err}");
                }
            },
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
            None => run_config_wizard(),
        },
    }
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
        println!("5) 设置触发空闲时间（毫秒）");
        println!("6) 设置输出截断参数");
        println!("7) 查看当前配置");
        println!("8) 清除一个配置项");
        println!("9) 显示配置文件路径");
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
                configure_idle_ms(&mut cfg)?;
                save_persisted_config(&cfg)?;
            }
            "6" => {
                configure_truncation(&mut cfg)?;
                save_persisted_config(&cfg)?;
            }
            "7" => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&masked_config_for_display(&cfg))?
                );
            }
            "8" => {
                configure_unset_key(&mut cfg)?;
                save_persisted_config(&cfg)?;
            }
            "9" => {
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

    if let Some(ipc) = ipc {
        ipc.send_event(
            "translation.started",
            json!({
                "reason": trigger_reason_text(triggered.reason),
                "truncated": truncated.truncated,
                "originalChars": truncated.original_chars,
                "selectedChars": truncated.text.chars().count(),
            }),
        );
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
            if let Some(ipc) = ipc {
                ipc.send_event(
                    "translation.done",
                    json!({
                        "translation": assembled,
                        "meta": meta,
                    }),
                );
            } else {
                eprintln!("\n[tetr][translation]\n{}\n", assembled);
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

fn spawn_ui_process(port: u16, configured_ui_bin: Option<&str>) -> Result<Child> {
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
        command
            .env("TETR_IPC_PORT", port.to_string())
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
