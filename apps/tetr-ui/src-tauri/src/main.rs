#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde::{Deserialize, Serialize};
use serde_json::json;
#[cfg(target_os = "macos")]
use std::collections::HashSet;
use std::env;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
#[cfg(target_os = "macos")]
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tauri::{Emitter, Manager};

#[cfg(target_os = "macos")]
use core_foundation::base::{CFType, TCFType};
#[cfg(target_os = "macos")]
use core_foundation::dictionary::CFDictionary;
#[cfg(target_os = "macos")]
use core_foundation::number::CFNumber;
#[cfg(target_os = "macos")]
use core_foundation::string::CFString;
#[cfg(target_os = "macos")]
use core_graphics::window;

#[derive(Debug, Serialize, Deserialize, Clone)]
struct IpcEnvelope {
    event: String,
    payload: serde_json::Value,
}

#[derive(Clone, Default)]
struct SharedWriter {
    inner: Arc<Mutex<Option<TcpStream>>>,
}

#[derive(Clone, Default)]
struct FrontendBridge {
    ready: Arc<AtomicBool>,
    pending: Arc<Mutex<Vec<IpcEnvelope>>>,
    notification_pending: Arc<AtomicBool>,
    notifier: Arc<Mutex<Option<tauri::AppHandle>>>,
}

impl FrontendBridge {
    fn set_notifier(&self, app_handle: tauri::AppHandle) {
        if let Ok(mut guard) = self.notifier.lock() {
            *guard = Some(app_handle);
        }
    }

    fn mark_ready(&self) {
        self.ready.store(true, Ordering::SeqCst);
        self.notify_events_available_if_needed(self.pending_len());
    }

    fn is_ready(&self) -> bool {
        self.ready.load(Ordering::SeqCst)
    }

    fn enqueue(&self, event: &str, payload: serde_json::Value) {
        self.enqueue_envelope(IpcEnvelope {
            event: event.to_string(),
            payload,
        });
    }

    fn enqueue_envelope(&self, envelope: IpcEnvelope) {
        let pending_len = match self.pending.lock() {
            Ok(mut guard) => {
                guard.push(envelope);
                guard.len()
            }
            Err(poisoned) => {
                eprintln!("[tetr-ui] pending queue lock poisoned during enqueue; recovering");
                let mut guard = poisoned.into_inner();
                guard.push(envelope);
                guard.len()
            }
        };

        self.notify_events_available_if_needed(pending_len);
    }

    fn drain_events(&self) -> Vec<IpcEnvelope> {
        match self.pending.lock() {
            Ok(mut guard) => {
                self.notification_pending.store(false, Ordering::SeqCst);
                std::mem::take(&mut *guard)
            }
            Err(poisoned) => {
                eprintln!("[tetr-ui] pending queue lock poisoned during drain; recovering");
                let mut guard = poisoned.into_inner();
                self.notification_pending.store(false, Ordering::SeqCst);
                std::mem::take(&mut *guard)
            }
        }
    }

    fn pending_len(&self) -> usize {
        match self.pending.lock() {
            Ok(guard) => guard.len(),
            Err(poisoned) => {
                eprintln!("[tetr-ui] pending queue lock poisoned during pending_len; recovering");
                let guard = poisoned.into_inner();
                guard.len()
            }
        }
    }

    fn notify_events_available_if_needed(&self, pending_len: usize) {
        if !should_emit_events_available(
            self.is_ready(),
            self.notification_pending.load(Ordering::SeqCst),
            pending_len,
        ) {
            return;
        }

        if self.notification_pending.swap(true, Ordering::SeqCst) {
            return;
        }

        let app_handle = match self.notifier.lock() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => {
                eprintln!("[tetr-ui] notifier lock poisoned during notify; recovering");
                poisoned.into_inner().clone()
            }
        };

        let Some(app_handle) = app_handle else {
            self.notification_pending.store(false, Ordering::SeqCst);
            return;
        };

        if let Err(err) = app_handle.emit(EVENTS_AVAILABLE_EVENT, json!({})) {
            eprintln!("[tetr-ui] failed to emit events-available signal: {err}");
            self.notification_pending.store(false, Ordering::SeqCst);
        }
    }
}

impl SharedWriter {
    fn set_stream(&self, stream: TcpStream) {
        if let Ok(mut guard) = self.inner.lock() {
            *guard = Some(stream);
        }
    }

    fn send_control(&self, event: &str) -> Result<(), String> {
        let envelope = IpcEnvelope {
            event: event.to_string(),
            payload: json!({}),
        };

        let line = serde_json::to_string(&envelope).map_err(|err| err.to_string())?;

        let mut guard = self
            .inner
            .lock()
            .map_err(|_| "writer mutex poisoned".to_string())?;
        let Some(stream) = guard.as_mut() else {
            return Err("CLI session is not connected".to_string());
        };

        writeln!(stream, "{line}").map_err(|err| err.to_string())?;
        stream.flush().map_err(|err| err.to_string())
    }
}

#[tauri::command]
fn request_stop(writer: tauri::State<'_, SharedWriter>) -> Result<(), String> {
    writer.send_control("control.stop")
}

fn is_supported_control_event(event: &str) -> bool {
    matches!(
        event,
        "control.stop" | "control.pause-toggle" | "control.snap"
    )
}

#[tauri::command]
fn send_control_event(event: String, writer: tauri::State<'_, SharedWriter>) -> Result<(), String> {
    let event = event.trim();
    if !is_supported_control_event(event) {
        return Err("unsupported control event".to_string());
    }
    writer.send_control(event)
}

#[tauri::command]
fn frontend_ready(frontend_bridge: tauri::State<'_, FrontendBridge>) {
    frontend_bridge.mark_ready();
}

#[tauri::command]
fn drain_events(frontend_bridge: tauri::State<'_, FrontendBridge>) -> Vec<IpcEnvelope> {
    frontend_bridge.drain_events()
}

fn main() {
    let writer = SharedWriter::default();
    let frontend_bridge = FrontendBridge::default();

    tauri::Builder::default()
        .manage(writer.clone())
        .manage(frontend_bridge.clone())
        .invoke_handler(tauri::generate_handler![
            request_stop,
            send_control_event,
            frontend_ready,
            drain_events
        ])
        .setup(move |app| {
            frontend_bridge.set_notifier(app.handle().clone());

            #[cfg(target_os = "macos")]
            {
                let policy = if should_show_dock_icon() {
                    tauri::ActivationPolicy::Regular
                } else {
                    tauri::ActivationPolicy::Accessory
                };
                let _ = app.set_activation_policy(policy);
            }

            if let Some(window) = app.get_webview_window("main") {
                let _ = window.set_always_on_top(true);
                let _ = window.set_decorations(true);
                let _ = window.set_zoom(1.0);

                #[cfg(target_os = "macos")]
                {
                    let _ = window.hide();
                }

                #[cfg(not(target_os = "macos"))]
                {
                    let _ = pin_window_to_bottom(&window);
                }
            }

            #[cfg(target_os = "macos")]
            start_macos_window_tracker(app.handle().clone());

            start_frontend_recovery_watchdog(app.handle().clone(), frontend_bridge.clone());

            let Some(port) = env::var("TETR_IPC_PORT")
                .ok()
                .and_then(|value| value.parse::<u16>().ok())
            else {
                frontend_bridge.enqueue(
                    "session.error",
                    json!({ "message": "missing TETR_IPC_PORT" }),
                );
                return Ok(());
            };

            let writer_clone = writer.clone();
            let frontend_bridge_clone = frontend_bridge.clone();
            thread::spawn(move || {
                let addr = format!("127.0.0.1:{port}");
                let mut stream_opt: Option<TcpStream> = None;
                let start = Instant::now();

                while start.elapsed() < Duration::from_secs(30) {
                    if let Ok(stream) = TcpStream::connect(&addr) {
                        stream_opt = Some(stream);
                        break;
                    }
                    thread::sleep(Duration::from_millis(120));
                }

                let Some(stream) = stream_opt else {
                    frontend_bridge_clone.enqueue(
                        "session.error",
                        json!({ "message": "failed to connect to CLI session" }),
                    );
                    return;
                };

                if let Ok(write_stream) = stream.try_clone() {
                    writer_clone.set_stream(write_stream);
                }

                let reader = BufReader::new(stream);
                for line in reader.lines() {
                    let line = match line {
                        Ok(line) => line,
                        Err(err) => {
                            eprintln!("[tetr-ui] IPC read error: {err}");
                            break;
                        }
                    };
                    let envelope = match serde_json::from_str::<IpcEnvelope>(&line) {
                        Ok(envelope) => envelope,
                        Err(err) => {
                            let clipped = clip_log_text(&line, 180);
                            eprintln!("[tetr-ui] IPC envelope parse error: {err}; line={clipped}");
                            continue;
                        }
                    };

                    frontend_bridge_clone.enqueue_envelope(envelope);
                }

                frontend_bridge_clone.enqueue(
                    "session.error",
                    json!({ "message": "CLI session disconnected" }),
                );
            });

            Ok(())
        })
        .on_window_event(|window, event| {
            if matches!(event, tauri::WindowEvent::CloseRequested { .. }) {
                let writer = window.app_handle().state::<SharedWriter>();
                let _ = writer.send_control("control.stop");
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tetr-ui");
}

const FRONTEND_RECOVERY_MAX_ATTEMPTS: u8 = 3;
const FRONTEND_RECOVERY_INTERVAL_MS: u64 = 1500;
const EVENTS_AVAILABLE_EVENT: &str = "tetr://events-available";

fn should_trigger_frontend_reload(frontend_ready: bool, attempt: u8) -> bool {
    !frontend_ready && attempt < FRONTEND_RECOVERY_MAX_ATTEMPTS
}

fn should_emit_events_available(
    frontend_ready: bool,
    notification_pending: bool,
    pending_len: usize,
) -> bool {
    frontend_ready && !notification_pending && pending_len > 0
}

fn clip_log_text(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        return input.to_string();
    }

    let mut clipped = String::new();
    for ch in input.chars().take(max_chars) {
        clipped.push(ch);
    }
    clipped.push('…');
    clipped
}

#[cfg(test)]
mod bridge_tests {
    use super::{is_supported_control_event, should_emit_events_available, FrontendBridge};
    use serde_json::json;
    use std::panic::{catch_unwind, AssertUnwindSafe};

    #[test]
    fn frontend_bridge_recovers_from_poisoned_pending_mutex() {
        let bridge = FrontendBridge::default();
        bridge.enqueue("event.one", json!({ "id": 1 }));

        let pending = bridge.pending.clone();
        let panic_result = catch_unwind(AssertUnwindSafe(move || {
            let _guard = pending.lock().expect("lock should succeed");
            panic!("force-poison");
        }));
        assert!(panic_result.is_err(), "mutex should be poisoned");

        bridge.enqueue("event.two", json!({ "id": 2 }));

        let events = bridge.drain_events();
        let names: Vec<&str> = events.iter().map(|e| e.event.as_str()).collect();
        assert_eq!(names, vec!["event.one", "event.two"]);
    }

    #[test]
    fn emits_notification_only_when_ready_and_not_pending_with_events() {
        assert!(!should_emit_events_available(false, false, 1));
        assert!(!should_emit_events_available(true, true, 1));
        assert!(!should_emit_events_available(true, false, 0));
        assert!(should_emit_events_available(true, false, 2));
    }

    #[test]
    fn validates_supported_control_events() {
        assert!(is_supported_control_event("control.stop"));
        assert!(is_supported_control_event("control.pause-toggle"));
        assert!(is_supported_control_event("control.snap"));
        assert!(!is_supported_control_event("control.unknown"));
    }
}

fn start_frontend_recovery_watchdog(app_handle: tauri::AppHandle, frontend_bridge: FrontendBridge) {
    thread::spawn(move || {
        for attempt in 0..FRONTEND_RECOVERY_MAX_ATTEMPTS {
            thread::sleep(Duration::from_millis(FRONTEND_RECOVERY_INTERVAL_MS));

            if !should_trigger_frontend_reload(frontend_bridge.is_ready(), attempt) {
                break;
            }

            let Some(window) = app_handle.get_webview_window("main") else {
                break;
            };

            let _ = window.clear_all_browsing_data();
            let _ = window.set_zoom(1.0);
            let _ = window.reload();
        }
    });
}

#[cfg(not(target_os = "macos"))]
fn pin_window_to_bottom(window: &tauri::WebviewWindow) -> tauri::Result<()> {
    let monitor = match window.current_monitor()? {
        Some(monitor) => monitor,
        None => return Ok(()),
    };

    let monitor_pos = monitor.position();
    let monitor_size = monitor.size();
    let window_size = window.outer_size()?;

    let x = monitor_pos.x + (monitor_size.width as i32 - window_size.width as i32) / 2;
    let y = monitor_pos.y + monitor_size.height as i32 - window_size.height as i32 - 24;

    window.set_position(tauri::Position::Physical(tauri::PhysicalPosition::new(
        x, y,
    )))
}

#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrontAppLayout {
    TerminalBounds {
        x: i32,
        y: i32,
        width: u32,
        height: u32,
    },
    OtherApp,
    TerminalWithoutBounds,
    Unknown,
}

#[cfg(target_os = "macos")]
#[derive(Debug, Clone)]
struct FrontAppInfo {
    bundle_id: String,
    pid: i32,
}

#[cfg(target_os = "macos")]
fn start_macos_window_tracker(app_handle: tauri::AppHandle) {
    thread::spawn(move || {
        let mut last_geometry: Option<(i32, i32, u32, u32)> = None;
        let mut hidden = true;
        let height_override = read_window_height_override_from_env();
        let terminal_bundle_overrides = read_terminal_bundle_overrides_from_env();

        loop {
            let Some(window) = app_handle.get_webview_window("main") else {
                break;
            };

            match query_front_app_layout(height_override, &terminal_bundle_overrides) {
                FrontAppLayout::TerminalBounds {
                    x,
                    y,
                    width,
                    height,
                } => {
                    let desired = (x, y, width, height);
                    if last_geometry != Some(desired) {
                        let _ = set_window_geometry_logical(&window, x, y, width, height);
                        last_geometry = Some(desired);
                    }

                    if hidden {
                        let _ = window.show();
                        hidden = false;
                    }
                }
                layout => {
                    if should_hide_for_layout(&layout) {
                        last_geometry = None;
                        if !hidden {
                            let _ = window.hide();
                            hidden = true;
                        }
                    }
                }
            }

            thread::sleep(Duration::from_millis(220));
        }
    });
}

#[cfg(target_os = "macos")]
fn set_window_geometry_logical(
    window: &tauri::WebviewWindow,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
) -> tauri::Result<()> {
    window.set_size(tauri::Size::Logical(tauri::LogicalSize::new(
        width as f64,
        height as f64,
    )))?;
    window.set_position(tauri::Position::Logical(tauri::LogicalPosition::new(
        x as f64, y as f64,
    )))
}

#[cfg(target_os = "macos")]
fn query_front_app_layout(
    height_override: Option<u32>,
    terminal_bundle_overrides: &[String],
) -> FrontAppLayout {
    let Some(front) = front_app_info() else {
        return FrontAppLayout::Unknown;
    };

    if !is_terminal_bundle(&front.bundle_id, terminal_bundle_overrides) {
        return FrontAppLayout::OtherApp;
    }

    let Some((x, y, width, height)) = query_window_bounds_for_pid(front.pid) else {
        return FrontAppLayout::TerminalWithoutBounds;
    };

    let (panel_x, panel_y, panel_width, panel_height) =
        compute_panel_geometry(x, y, width, height, height_override);

    FrontAppLayout::TerminalBounds {
        x: panel_x,
        y: panel_y,
        width: panel_width,
        height: panel_height,
    }
}

#[cfg(target_os = "macos")]
fn compute_panel_geometry(
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    height_override: Option<u32>,
) -> (i32, i32, u32, u32) {
    const DEFAULT_PANEL_HEIGHT: u32 = 260;
    let panel_height = height_override.unwrap_or(DEFAULT_PANEL_HEIGHT);
    // Stack panel under terminal: panel top edge touches terminal bottom edge.
    let panel_y = y + height;
    (x, panel_y, width as u32, panel_height)
}

#[cfg(target_os = "macos")]
fn read_window_height_override_from_env() -> Option<u32> {
    let raw = env::var("TETR_UI_WINDOW_HEIGHT").ok()?;
    let parsed = raw.trim().parse::<u32>().ok()?;
    if (80..=900).contains(&parsed) {
        Some(parsed)
    } else {
        None
    }
}

#[cfg(target_os = "macos")]
fn front_app_info() -> Option<FrontAppInfo> {
    let front_output = Command::new("lsappinfo").arg("front").output().ok()?;
    if !front_output.status.success() {
        return None;
    }

    let front_text = String::from_utf8_lossy(&front_output.stdout);
    let asn = front_text
        .lines()
        .find_map(|line| line.strip_suffix(':'))
        .map(str::trim)?;

    let info_output = Command::new("lsappinfo")
        .args(["info", "-only", "bundleid,pid", asn])
        .output()
        .ok()?;
    if !info_output.status.success() {
        return None;
    }

    let info_text = String::from_utf8_lossy(&info_output.stdout);

    let mut bundle_id: Option<String> = None;
    let mut pid: Option<i32> = None;

    for line in info_text.lines() {
        if line.contains("CFBundleIdentifier") {
            let mut segments = line.split('"');
            let _ = segments.next();
            let _ = segments.next();
            let _ = segments.next();
            let value = segments.next()?.trim();
            if !value.is_empty() {
                bundle_id = Some(value.to_string());
            }
        }

        if line.contains("pid") {
            let digits: String = line.chars().filter(|ch| ch.is_ascii_digit()).collect();
            if !digits.is_empty() {
                if let Ok(parsed) = digits.parse::<i32>() {
                    pid = Some(parsed);
                }
            }
        }
    }

    Some(FrontAppInfo {
        bundle_id: bundle_id?,
        pid: pid?,
    })
}

#[cfg(target_os = "macos")]
fn read_terminal_bundle_overrides_from_env() -> Vec<String> {
    parse_terminal_bundle_overrides(env::var("TETR_UI_TERMINAL_BUNDLE_IDS").ok().as_deref())
}

#[cfg(target_os = "macos")]
fn parse_terminal_bundle_overrides(raw: Option<&str>) -> Vec<String> {
    let Some(raw) = raw else {
        return Vec::new();
    };

    let mut seen = HashSet::new();
    let mut overrides = Vec::new();

    for token in raw.split(|ch| matches!(ch, ',' | ';' | '\n')) {
        let trimmed = token.trim();
        if trimmed.is_empty() || !is_valid_terminal_bundle_id(trimmed) {
            continue;
        }

        let lowered = trimmed.to_ascii_lowercase();
        if seen.insert(lowered.clone()) {
            overrides.push(lowered);
        }
    }

    overrides
}

#[cfg(target_os = "macos")]
fn is_valid_terminal_bundle_id(value: &str) -> bool {
    value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_'))
}

#[cfg(target_os = "macos")]
fn is_terminal_bundle(bundle_id: &str, terminal_bundle_overrides: &[String]) -> bool {
    const BUILTIN_TERMINAL_BUNDLES: &[&str] = &[
        "com.apple.terminal",
        "com.googlecode.iterm2",
        "com.termius.mac",
        "dev.warp.warp-stable",
        "com.github.wez.wezterm",
        "com.mitchellh.ghostty",
        "org.alacritty",
        "net.kovidgoyal.kitty",
        "co.zeit.hyper",
        "org.tabby",
        "com.github.rprichard.cygnus",
    ];

    let normalized = bundle_id.trim().to_ascii_lowercase();

    BUILTIN_TERMINAL_BUNDLES.contains(&normalized.as_str())
        || terminal_bundle_overrides
            .iter()
            .any(|entry| entry.eq_ignore_ascii_case(&normalized))
}

#[cfg(target_os = "macos")]
fn should_hide_for_layout(layout: &FrontAppLayout) -> bool {
    !matches!(layout, FrontAppLayout::TerminalBounds { .. })
}

#[cfg(target_os = "macos")]
fn should_show_dock_icon() -> bool {
    should_show_dock_icon_from_env(env::var("TETR_UI_DOCK_ICON").ok().as_deref())
}

#[cfg(target_os = "macos")]
fn should_show_dock_icon_from_env(raw: Option<&str>) -> bool {
    let Some(raw) = raw else {
        return false;
    };

    matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::{
        compute_panel_geometry, is_terminal_bundle, parse_terminal_bundle_overrides,
        should_hide_for_layout, should_show_dock_icon_from_env, should_trigger_frontend_reload,
        FrontAppLayout,
    };

    #[test]
    fn recognizes_real_terminal_bundles() {
        let overrides: Vec<String> = Vec::new();
        assert!(is_terminal_bundle("com.apple.Terminal", &overrides));
        assert!(is_terminal_bundle("com.googlecode.iterm2", &overrides));
        assert!(is_terminal_bundle("com.github.wez.wezterm", &overrides));
        assert!(is_terminal_bundle("com.termius.mac", &overrides));
    }

    #[test]
    fn does_not_treat_chat_clients_as_terminal_apps() {
        let overrides: Vec<String> = Vec::new();
        assert!(!is_terminal_bundle("com.openai.codex", &overrides));
        assert!(!is_terminal_bundle("com.anthropic.claude", &overrides));
        assert!(!is_terminal_bundle("com.anthropic.claudecode", &overrides));
    }

    #[test]
    fn allows_user_defined_terminal_bundle_overrides() {
        let overrides = parse_terminal_bundle_overrides(Some(
            "com.example.Terminal, com.termius.mac ; com.example.Terminal",
        ));
        assert!(is_terminal_bundle("com.example.terminal", &overrides));
        assert!(is_terminal_bundle("com.termius.mac", &overrides));
        assert!(!is_terminal_bundle("com.random.app", &overrides));
    }

    #[test]
    fn hides_window_when_layout_is_not_terminal_bounds() {
        assert!(should_hide_for_layout(&FrontAppLayout::OtherApp));
        assert!(should_hide_for_layout(
            &FrontAppLayout::TerminalWithoutBounds
        ));
        assert!(should_hide_for_layout(&FrontAppLayout::Unknown));
    }

    #[test]
    fn keeps_window_visible_when_layout_is_terminal_bounds() {
        assert!(!should_hide_for_layout(&FrontAppLayout::TerminalBounds {
            x: 10,
            y: 20,
            width: 640,
            height: 220,
        }));
    }

    #[test]
    fn panel_should_stack_below_terminal_without_overlap() {
        let (panel_x, panel_y, panel_width, panel_height) =
            compute_panel_geometry(100, 50, 1200, 600, None);
        assert_eq!(panel_x, 100);
        assert_eq!(panel_width, 1200);
        assert_eq!(panel_height, 260);
        assert_eq!(panel_y, 650);
    }

    #[test]
    fn panel_default_height_should_not_depend_on_terminal_height() {
        let (_, _, _, small_terminal_height) = compute_panel_geometry(100, 50, 1200, 240, None);
        let (_, _, _, large_terminal_height) = compute_panel_geometry(100, 50, 1200, 900, None);
        assert_eq!(small_terminal_height, large_terminal_height);
        assert_eq!(small_terminal_height, 260);
    }

    #[test]
    fn panel_height_override_should_take_priority() {
        let (_, panel_y, _, panel_height) = compute_panel_geometry(10, 20, 1000, 500, Some(300));
        assert_eq!(panel_height, 300);
        assert_eq!(panel_y, 520);
    }

    #[test]
    fn dock_icon_should_be_hidden_by_default() {
        assert!(!should_show_dock_icon_from_env(None));
        assert!(!should_show_dock_icon_from_env(Some("")));
        assert!(!should_show_dock_icon_from_env(Some("0")));
        assert!(!should_show_dock_icon_from_env(Some("off")));
    }

    #[test]
    fn dock_icon_can_be_enabled_by_env_toggle() {
        assert!(should_show_dock_icon_from_env(Some("1")));
        assert!(should_show_dock_icon_from_env(Some("true")));
        assert!(should_show_dock_icon_from_env(Some("yes")));
        assert!(should_show_dock_icon_from_env(Some("on")));
        assert!(should_show_dock_icon_from_env(Some(" TRUE ")));
    }

    #[test]
    fn frontend_reload_should_only_happen_before_ready_and_within_retry_budget() {
        assert!(should_trigger_frontend_reload(false, 0));
        assert!(should_trigger_frontend_reload(false, 2));
        assert!(!should_trigger_frontend_reload(false, 3));
        assert!(!should_trigger_frontend_reload(true, 0));
    }
}

#[cfg(target_os = "macos")]
fn query_window_bounds_for_pid(pid: i32) -> Option<(i32, i32, i32, i32)> {
    let window_ids = window::create_window_list(
        window::kCGWindowListOptionOnScreenOnly | window::kCGWindowListExcludeDesktopElements,
        window::kCGNullWindowID,
    )?;

    let descriptions = window::create_description_from_array(window_ids)?;

    let key_owner_pid = unsafe { CFString::wrap_under_get_rule(window::kCGWindowOwnerPID) };
    let key_layer = unsafe { CFString::wrap_under_get_rule(window::kCGWindowLayer) };
    let key_bounds = unsafe { CFString::wrap_under_get_rule(window::kCGWindowBounds) };

    let key_x = CFString::from_static_string("X");
    let key_y = CFString::from_static_string("Y");
    let key_w = CFString::from_static_string("Width");
    let key_h = CFString::from_static_string("Height");

    let mut best: Option<(i32, i32, i32, i32, i64)> = None;

    for dict in &descriptions {
        let Some(owner_pid) = dict
            .find(&key_owner_pid)
            .and_then(|v| v.downcast::<CFNumber>())
            .and_then(|n| n.to_i64())
        else {
            continue;
        };

        if owner_pid != pid as i64 {
            continue;
        }

        let layer = dict
            .find(&key_layer)
            .and_then(|v| v.downcast::<CFNumber>())
            .and_then(|n| n.to_i64())
            .unwrap_or(0);

        if layer != 0 {
            continue;
        }

        let Some(bounds_untyped) = dict
            .find(&key_bounds)
            .and_then(|v| v.downcast::<CFDictionary>())
        else {
            continue;
        };

        let bounds_dict: CFDictionary<CFString, CFType> =
            unsafe { CFDictionary::wrap_under_get_rule(bounds_untyped.as_concrete_TypeRef()) };

        let Some(x) = cf_number_from_dict(&bounds_dict, &key_x) else {
            continue;
        };
        let Some(y) = cf_number_from_dict(&bounds_dict, &key_y) else {
            continue;
        };
        let Some(w) = cf_number_from_dict(&bounds_dict, &key_w) else {
            continue;
        };
        let Some(h) = cf_number_from_dict(&bounds_dict, &key_h) else {
            continue;
        };

        if w < 200.0 || h < 120.0 {
            continue;
        }

        let x = x.round() as i32;
        let y = y.round() as i32;
        let w = w.round() as i32;
        let h = h.round() as i32;

        let area = i64::from(w) * i64::from(h);
        if best
            .map(|(_, _, _, _, best_area)| area > best_area)
            .unwrap_or(true)
        {
            best = Some((x, y, w, h, area));
        }
    }

    best.map(|(x, y, w, h, _)| (x, y, w, h))
}

#[cfg(target_os = "macos")]
fn cf_number_from_dict(dict: &CFDictionary<CFString, CFType>, key: &CFString) -> Option<f64> {
    let number = dict.find(key)?.downcast::<CFNumber>()?;
    number
        .to_f64()
        .or_else(|| number.to_i64().map(|v| v as f64))
}
