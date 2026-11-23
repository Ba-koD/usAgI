use std::io::{self, BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};

/// A settings executable from the app bundle, the build directory, or an
/// embedded payload extracted into a temporary directory.
pub struct SettingsProgram {
    path: PathBuf,
    _directory: Option<tempfile::TempDir>,
}

impl SettingsProgram {
    pub fn new(path: PathBuf) -> Self {
        Self { path, _directory: None }
    }

    pub fn temporary(path: PathBuf, directory: tempfile::TempDir) -> Self {
        Self { path, _directory: Some(directory) }
    }

    /// Keep supporting direct --settings-ui invocations with inherited pipes.
    pub fn run(self) -> io::Result<ExitStatus> {
        Command::new(&self.path).status()
    }
}

/// Hosts the settings process and its JSON lines without linking the UI.
pub struct SettingsWindow {
    child: Child,
    outgoing: Sender<String>,
    incoming: Receiver<String>,
    _program: SettingsProgram,
}

impl SettingsWindow {
    pub fn open(program: SettingsProgram, wake_host: impl Fn() + Send + 'static) -> io::Result<Self> {
        let mut child = Command::new(&program.path)
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
            _program: program,
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
