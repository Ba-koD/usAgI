use std::io::{self, BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};

/// The same executable opens a settings window on its own main thread. Its
/// allocator, fonts and rendering buffers disappear when the window closes.
pub struct SettingsWindow {
    child: Child,
    outgoing: Sender<String>,
    incoming: Receiver<String>,
}

impl SettingsWindow {
    pub fn open(wake_host: impl Fn() + Send + 'static) -> io::Result<Self> {
        let mut child = Command::new(std::env::current_exe()?)
            .arg(super::SETTINGS_ARG)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let mut stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let (outgoing, requests) = mpsc::channel::<String>();
        std::thread::spawn(move || {
            for line in requests {
                if writeln!(stdin, "{line}").is_err() {
                    break;
                }
            }
        });
        let (responses, incoming) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if responses.send(line).is_err() {
                    return;
                }
                wake_host();
            }
            // EOF covers the title-bar close, an early startup error and a crash.
            let _ = responses.send(r#"{"cmd":"close"}"#.into());
            wake_host();
        });
        Ok(Self {
            child,
            outgoing,
            incoming,
        })
    }

    pub fn next_command(&self) -> Option<String> {
        self.incoming.try_recv().ok()
    }

    pub fn push(&self, state: String) {
        let _ = self.outgoing.send(state);
    }

    pub fn focus(&self) {
        let _ = self.outgoing.send(r#""focus""#.into());
    }
}

impl Drop for SettingsWindow {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
