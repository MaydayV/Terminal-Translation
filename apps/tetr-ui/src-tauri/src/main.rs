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
                let _ = window.set_decorations(true);

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
                FrontAppLayout::OtherApp => {
                    if !hidden {
                        let _ = window.hide();
                        hidden = true;
                    }
                }
                FrontAppLayout::TerminalWithoutBounds | FrontAppLayout::Unknown => {
                    if last_geometry.is_none() && !hidden {
                        let _ = window.hide();
                        hidden = true;
                    }
                }
            }

            thread::sleep(Duration::from_millis(220));
        }
    });
}

#[cfg(target_os = "macos")]
fn query_front_app_layout() -> FrontAppLayout {
    let Some(front) = front_app_info() else {
        return FrontAppLayout::Unknown;
    };

    if !is_terminal_bundle(&front.bundle_id) {
        return FrontAppLayout::OtherApp;
    }

    let Some((x, y, width, height)) = query_window_bounds_for_pid(front.pid) else {
        return FrontAppLayout::TerminalWithoutBounds;
    };

    let panel_height = ((height as f32 * 0.34).round() as i32).clamp(180, 320);
    let panel_y = y + height - panel_height;

    FrontAppLayout::TerminalBounds {
        x,
        y: panel_y,
        width: width as u32,
        height: panel_height as u32,
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
            | "com.openai.codex"
            | "com.anthropic.claude"
            | "com.anthropic.claudecode"
    )
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
        if best.is_none_or(|(_, _, _, _, best_area)| area > best_area) {
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
