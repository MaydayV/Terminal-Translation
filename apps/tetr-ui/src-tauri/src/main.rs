#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde::{Deserialize, Serialize};
use serde_json::json;
use std::env;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
#[cfg(target_os = "macos")]
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tauri::{Emitter, Manager};
#[cfg(target_os = "windows")]
use window_vibrancy::apply_acrylic;
#[cfg(target_os = "macos")]
use window_vibrancy::{apply_vibrancy, NSVisualEffectMaterial};

#[derive(Debug, Serialize, Deserialize)]
struct IpcEnvelope {
    event: String,
    payload: serde_json::Value,
}

#[derive(Clone, Default)]
struct SharedWriter {
    inner: Arc<Mutex<Option<TcpStream>>>,
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

fn main() {
    let writer = SharedWriter::default();

    tauri::Builder::default()
        .manage(writer.clone())
        .invoke_handler(tauri::generate_handler![request_stop])
        .setup(move |app| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.set_always_on_top(true);
                let _ = pin_window_to_bottom(&window);
                #[cfg(target_os = "macos")]
                let _ = apply_vibrancy(&window, NSVisualEffectMaterial::HudWindow, None, None);
                #[cfg(target_os = "windows")]
                let _ = apply_acrylic(&window, Some((18, 24, 28, 180)));
            }

            #[cfg(target_os = "macos")]
            start_macos_window_tracker(app.handle().clone());

            let Some(port) = env::var("TETR_IPC_PORT")
                .ok()
                .and_then(|value| value.parse::<u16>().ok())
            else {
                let _ = app.emit(
                    "session.error",
                    json!({ "message": "missing TETR_IPC_PORT" }),
                );
                return Ok(());
            };

            let app_handle = app.handle().clone();
            let writer_clone = writer.clone();
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
                    let _ = app_handle.emit(
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
                    let Ok(line) = line else {
                        break;
                    };
                    let Ok(envelope) = serde_json::from_str::<IpcEnvelope>(&line) else {
                        continue;
                    };

                    let _ = app_handle.emit(&envelope.event, envelope.payload);
                }

                let _ = app_handle.emit(
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
    UnsupportedTerminal,
    OtherApp,
    Unknown,
}

#[cfg(target_os = "macos")]
fn start_macos_window_tracker(app_handle: tauri::AppHandle) {
    thread::spawn(move || {
        let mut last_geometry: Option<(i32, i32, u32, u32)> = None;
        let mut hidden = false;

        loop {
            let Some(window) = app_handle.get_webview_window("main") else {
                break;
            };

            match query_front_app_layout() {
                FrontAppLayout::TerminalBounds {
                    x,
                    y,
                    width,
                    height,
                } => {
                    if hidden {
                        let _ = window.show();
                        hidden = false;
                    }

                    let desired = (x, y, width, height);
                    if last_geometry != Some(desired) {
                        let _ = window.set_size(tauri::Size::Physical(tauri::PhysicalSize::new(
                            width, height,
                        )));
                        let _ = window.set_position(tauri::Position::Physical(
                            tauri::PhysicalPosition::new(x, y),
                        ));
                        last_geometry = Some(desired);
                    }
                }
                FrontAppLayout::UnsupportedTerminal => {
                    if hidden {
                        let _ = window.show();
                        hidden = false;
                    }
                    let _ = pin_window_to_bottom(&window);
                }
                FrontAppLayout::OtherApp => {
                    if !hidden {
                        let _ = window.hide();
                        hidden = true;
                    }
                }
                FrontAppLayout::Unknown => {
                    if hidden {
                        let _ = window.show();
                        hidden = false;
                    }
                    let _ = pin_window_to_bottom(&window);
                }
            }

            thread::sleep(Duration::from_millis(350));
        }
    });
}

#[cfg(target_os = "macos")]
fn query_front_app_layout() -> FrontAppLayout {
    let Some(front_bundle_id) = front_app_bundle_id() else {
        return FrontAppLayout::Unknown;
    };

    if !is_terminal_bundle(&front_bundle_id) {
        return FrontAppLayout::OtherApp;
    }

    let bounds = if front_bundle_id == "com.apple.Terminal" {
        query_terminal_bounds()
    } else if front_bundle_id == "com.googlecode.iterm2" {
        query_iterm_bounds()
    } else {
        None
    };

    let Some((x1, y1, x2, y2)) = bounds else {
        return FrontAppLayout::UnsupportedTerminal;
    };

    let term_width = (x2 - x1).max(420) as u32;
    let term_height = (y2 - y1).max(240);
    let panel_height = ((term_height as f32 * 0.36).round() as i32).clamp(190, 360);
    let panel_y = y2 - panel_height - 8;

    FrontAppLayout::TerminalBounds {
        x: x1,
        y: panel_y,
        width: term_width,
        height: panel_height as u32,
    }
}

#[cfg(target_os = "macos")]
fn front_app_bundle_id() -> Option<String> {
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
        .args(["info", "-only", "bundleid", asn])
        .output()
        .ok()?;
    if !info_output.status.success() {
        return None;
    }

    let info_text = String::from_utf8_lossy(&info_output.stdout);
    for line in info_text.lines() {
        if line.contains("CFBundleIdentifier") {
            let mut segments = line.split('"');
            let _ = segments.next();
            let _ = segments.next();
            let _ = segments.next();
            let value = segments.next()?.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }

    None
}

#[cfg(target_os = "macos")]
fn is_terminal_bundle(bundle_id: &str) -> bool {
    matches!(
        bundle_id,
        "com.apple.Terminal"
            | "com.googlecode.iterm2"
            | "dev.warp.Warp-Stable"
            | "com.github.wez.wezterm"
            | "com.mitchellh.ghostty"
            | "org.alacritty"
            | "net.kovidgoyal.kitty"
            | "co.zeit.hyper"
            | "org.tabby"
            | "com.github.rprichard.cygnus"
    )
}

#[cfg(target_os = "macos")]
fn query_terminal_bounds() -> Option<(i32, i32, i32, i32)> {
    let script = r#"
tell application "Terminal"
  if (count of windows) is 0 then return ""
  set b to bounds of front window
  return (item 1 of b) & "|" & (item 2 of b) & "|" & (item 3 of b) & "|" & (item 4 of b)
end tell
"#;
    query_bounds_by_script(script)
}

#[cfg(target_os = "macos")]
fn query_iterm_bounds() -> Option<(i32, i32, i32, i32)> {
    let script = r#"
tell application "iTerm2"
  if (count of windows) is 0 then return ""
  set b to bounds of current window
  return (item 1 of b) & "|" & (item 2 of b) & "|" & (item 3 of b) & "|" & (item 4 of b)
end tell
"#;
    query_bounds_by_script(script)
}

#[cfg(target_os = "macos")]
fn query_bounds_by_script(script: &str) -> Option<(i32, i32, i32, i32)> {
    let output = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let raw = String::from_utf8_lossy(&output.stdout);
    let line = raw.trim();
    if line.is_empty() {
        return None;
    }

    let mut parts = line.split('|');
    let x1 = parts.next()?.trim().parse::<i32>().ok()?;
    let y1 = parts.next()?.trim().parse::<i32>().ok()?;
    let x2 = parts.next()?.trim().parse::<i32>().ok()?;
    let y2 = parts.next()?.trim().parse::<i32>().ok()?;
    Some((x1, y1, x2, y2))
}
