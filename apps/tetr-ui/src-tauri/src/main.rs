#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde::{Deserialize, Serialize};
use serde_json::json;
use std::env;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
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
                let _ = pin_window_to_bottom(&window);
                #[cfg(target_os = "macos")]
                let _ = apply_vibrancy(&window, NSVisualEffectMaterial::HudWindow, None, None);
                #[cfg(target_os = "windows")]
                let _ = apply_acrylic(&window, Some((18, 24, 28, 180)));
            }

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
                for _ in 0..50 {
                    match TcpStream::connect(&addr) {
                        Ok(stream) => {
                            stream_opt = Some(stream);
                            break;
                        }
                        Err(_) => {
                            thread::sleep(Duration::from_millis(80));
                        }
                    }
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
