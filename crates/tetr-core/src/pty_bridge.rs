use crossbeam_channel::{unbounded, Receiver};
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use std::io::{Read, Write};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::thread;
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct OutputChunk {
    pub raw: String,
    pub clean: String,
}

#[derive(Debug, Error)]
pub enum BridgeError {
    #[error("pty operation failed: {0}")]
    Pty(String),

    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
}

pub struct PtyBridge {
    pub output_rx: Receiver<OutputChunk>,
    pub command_rx: Receiver<String>,
    pub exit_rx: Receiver<()>,
    master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    child: Arc<Mutex<Box<dyn portable_pty::Child + Send + Sync>>>,
    running: Arc<AtomicBool>,
}

impl PtyBridge {
    pub fn start(shell: &str, cols: u16, rows: u16) -> Result<Self, BridgeError> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|err| BridgeError::Pty(err.to_string()))?;

        let master = Arc::new(Mutex::new(pair.master));

        let mut cmd = CommandBuilder::new(shell);
        cmd.env("TERM", "xterm-256color");
        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|err| BridgeError::Pty(err.to_string()))?;

        let child = Arc::new(Mutex::new(child));
        let running = Arc::new(AtomicBool::new(true));

        let reader = master
            .lock()
            .map_err(|_| BridgeError::Pty("failed to lock PTY master".to_string()))?
            .try_clone_reader()
            .map_err(|err| BridgeError::Pty(err.to_string()))?;

        let mut writer = master
            .lock()
            .map_err(|_| BridgeError::Pty("failed to lock PTY master".to_string()))?
            .take_writer()
            .map_err(|err| BridgeError::Pty(err.to_string()))?;

        let (output_tx, output_rx) = unbounded();
        let (command_tx, command_rx) = unbounded();
        let (exit_tx, exit_rx) = unbounded();

        let stdout_running = Arc::clone(&running);
        let output_tx_thread = output_tx.clone();
        thread::spawn(move || {
            let mut reader = reader;
            let stdout = std::io::stdout();
            let mut stdout = stdout.lock();
            let mut buf = [0u8; 8192];

            loop {
                if !stdout_running.load(Ordering::Relaxed) {
                    break;
                }

                match reader.read(&mut buf) {
                    Ok(0) => {
                        let _ = exit_tx.send(());
                        break;
                    }
                    Ok(size) => {
                        if stdout.write_all(&buf[..size]).is_err() || stdout.flush().is_err() {
                            let _ = exit_tx.send(());
                            break;
                        }

                        let raw = String::from_utf8_lossy(&buf[..size]).to_string();
                        let stripped = strip_ansi_escapes::strip(&buf[..size]);
                        let clean = normalize_for_translation_text(
                            String::from_utf8_lossy(&stripped).to_string(),
                        );

                        if output_tx_thread.send(OutputChunk { raw, clean }).is_err() {
                            break;
                        }
                    }
                    Err(_) => {
                        let _ = exit_tx.send(());
                        break;
                    }
                }
            }
        });

        let stdin_running = Arc::clone(&running);
        thread::spawn(move || {
            let stdin = std::io::stdin();
            let mut stdin = stdin.lock();
            let mut byte = [0u8; 1];
            let mut current_command = Vec::new();

            loop {
                if !stdin_running.load(Ordering::Relaxed) {
                    break;
                }

                let Ok(read_size) = stdin.read(&mut byte) else {
                    break;
                };
                if read_size == 0 {
                    break;
                }

                if writer.write_all(&byte).is_err() || writer.flush().is_err() {
                    break;
                }

                match byte[0] {
                    b'\r' | b'\n' => {
                        let cmd = String::from_utf8_lossy(&current_command).trim().to_string();
                        if !cmd.is_empty() {
                            let _ = command_tx.send(cmd);
                        }
                        current_command.clear();
                    }
                    8 | 127 => {
                        current_command.pop();
                    }
                    value => {
                        current_command.push(value);
                    }
                }
            }
        });

        Ok(Self {
            output_rx,
            command_rx,
            exit_rx,
            master,
            child,
            running,
        })
    }

    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), BridgeError> {
        self.master
            .lock()
            .map_err(|_| BridgeError::Pty("failed to lock PTY master".to_string()))?
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|err| BridgeError::Pty(err.to_string()))
    }

    pub fn terminate(&self) {
        self.running.store(false, Ordering::Relaxed);

        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    pub fn running_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.running)
    }
}

fn normalize_for_translation_text(input: String) -> String {
    input
        .replace("\r\n", "\n")
        .lines()
        .map(|line| line.trim_end().to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::normalize_for_translation_text;

    #[test]
    fn normalizes_windows_line_endings() {
        let input = "line1\r\nline2\r\n".to_string();
        let output = normalize_for_translation_text(input);
        assert_eq!(output, "line1\nline2");
    }

    #[test]
    fn trims_trailing_whitespace() {
        let input = "line1    \nline2\t\n".to_string();
        let output = normalize_for_translation_text(input);
        assert_eq!(output, "line1\nline2");
    }
}
