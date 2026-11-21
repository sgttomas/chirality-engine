//! One run of a scenario: its folders, the engine, and either the real Codex
//! against the stub model (live or recording) or a recorded transcript.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::sync::broadcast;

use super::apptools::{self, TestTools};
use super::record::{Recorder, Replay};
use super::stub::{Script, Stub};
use super::transcript::{self, Places};
use crate::api::*;
use crate::transport::{Launch, ProcessConnector};
use crate::Engine;

/// The pinned Codex, named by `CHIRALITY_TEST_CODEX`; the tests that run it
/// are skipped without it.
pub fn codex() -> Option<PathBuf> {
    std::env::var_os("CHIRALITY_TEST_CODEX").map(PathBuf::from).filter(|p| p.is_file())
}

pub fn codex_present() -> bool {
    codex().is_some()
}

/// Where runs against the pinned Codex keep their folders:
/// `CHIRALITY_TEST_SCRATCH`, or a folder in the system's temporary directory.
pub fn scratch() -> PathBuf {
    std::env::var_os("CHIRALITY_TEST_SCRATCH").map(PathBuf::from).unwrap_or_else(|| std::env::temp_dir().join("chirality-engine-tests"))
}

/// A Chirality Library edition's `workflows` folder, named by
/// `CHIRALITY_TEST_LIBRARY`; the tests that read it are skipped without it.
pub fn library() -> Option<PathBuf> {
    std::env::var_os("CHIRALITY_TEST_LIBRARY").map(PathBuf::from).filter(|p| p.is_dir())
}

/// A fresh root holding the project, the Codex home, the engine's state,
/// the person's home and a temporary folder.
pub struct Run {
    pub root: PathBuf,
}

static RUNS: AtomicUsize = AtomicUsize::new(0);

impl Run {
    pub fn new(base: &Path, name: &str) -> Run {
        let n = RUNS.fetch_add(1, Ordering::SeqCst);
        let root = base.join(format!("{name}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for dir in ["project", "home", "state", "userhome", "tmp"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        Run { root: root.canonicalize().unwrap() }
    }

    pub fn project(&self) -> PathBuf {
        self.root.join("project")
    }

    fn config(&self, tools: Option<&Arc<TestTools>>) -> EngineConfig {
        EngineConfig {
            codex_bin: codex().unwrap_or_else(|| PathBuf::from("codex")),
            codex_home: Some(self.root.join("home")),
            state_dir: self.root.join("state"),
            workflow_roots: library().into_iter().collect(),
            codex_settings: Vec::new(),
            tools: tools.map(|t| t.app_tools()),
        }
    }

    fn places(&self, stub_port: u16) -> Places {
        let path = |p: PathBuf| p.display().to_string();
        let mut pairs = vec![
            ("<project>".to_string(), path(self.project())),
            ("<codex-home>".to_string(), path(self.root.join("home"))),
            ("<root>".to_string(), path(self.root.clone())),
        ];
        if let Some(library) = library() {
            pairs.push(("<library>".to_string(), path(library)));
        }
        if stub_port != 0 {
            pairs.push(("<stub>".to_string(), format!("127.0.0.1:{stub_port}")));
        }
        Places::new(pairs)
    }

    /// Codex's environment: nothing inherited; the network goes nowhere but loopback.
    fn environment(&self) -> Vec<(String, String)> {
        let mut env = vec![
            ("PATH".to_string(), "/usr/bin:/bin:/usr/sbin:/sbin".to_string()),
            ("HOME".to_string(), self.root.join("userhome").display().to_string()),
            ("TMPDIR".to_string(), format!("{}/", self.root.join("tmp").display())),
            ("TZ".to_string(), "UTC".to_string()),
            ("LANG".to_string(), "en_US.UTF-8".to_string()),
            ("NO_PROXY".to_string(), "127.0.0.1,localhost,::1".to_string()),
            ("no_proxy".to_string(), "127.0.0.1,localhost,::1".to_string()),
        ];
        for name in ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy"] {
            env.push((name.to_string(), "http://127.0.0.1:9".to_string()));
        }
        env
    }
}

enum Mode {
    Live { stub: Stub, recorder: Option<Recorder> },
    Replay(Arc<Replay>),
}

/// What a scenario works with.
pub struct Ctx {
    pub engine: Engine,
    pub events: broadcast::Receiver<Numbered>,
    pub run: Run,
    /// When set, keeps every event and each thread view the scenario reads.
    pub tape: Option<std::sync::Mutex<Tape>>,
    /// The application's tools, for the scenarios that have them.
    pub tools: Option<Arc<TestTools>>,
    mode: Mode,
    places: Places,
}

/// The events since taping began, and each view read with how many events preceded it.
pub struct Tape {
    pub rx: broadcast::Receiver<Numbered>,
    pub events: Vec<Event>,
    pub reads: Vec<(usize, ThreadView)>,
}

impl Tape {
    pub fn drain(&mut self) {
        loop {
            match self.rx.try_recv() {
                Ok(numbered) => self.events.push(numbered.event),
                Err(broadcast::error::TryRecvError::Lagged(n)) => panic!("{n} events lost"),
                Err(_) => return,
            }
        }
    }
}

impl Ctx {
    /// The real Codex against the stub; with `record`, the frames are kept.
    pub async fn live(name: &str, record: bool) -> Ctx {
        Ctx::live_with(name, record, None).await
    }

    /// Live, with `app_tools` in place of the scenario's own; the engine's
    /// start-up check of them is skipped, so Codex's own can be seen.
    pub async fn live_with(name: &str, record: bool, app_tools: Option<AppTools>) -> Ctx {
        let run = Run::new(&scratch(), name);
        let stub = Stub::start().await;
        let tools = apptools::for_scenario(name);
        let mut config = run.config(tools.as_ref());
        if app_tools.is_some() {
            config.tools = app_tools;
        }
        let mut settings = vec!["analytics.enabled=false".to_string(), "model_provider=\"openai\"".to_string(), format!("openai_base_url=\"{}\"", stub.base_url())];
        settings.extend(crate::transport::settings(&config));
        let launch = Launch { bin: config.codex_bin.clone(), home: config.codex_home.clone(), settings, env: Some(run.environment()), cwd: Some(run.project()) };
        let recorder = record.then(Recorder::new);
        let mut connector: Arc<dyn crate::transport::Connector> = Arc::new(ProcessConnector { launch });
        let mut tap = None;
        if let Some(recorder) = &recorder {
            connector = recorder.connector(connector);
            tap = Some(recorder.tap());
        }
        let places = run.places(stub.port);
        let engine = Engine::start_with(config, connector, tap).await.expect("the engine starts");
        let events = engine.subscribe();
        Ctx { engine, events, run, tape: None, tools, mode: Mode::Live { stub, recorder }, places }
    }

    /// The recorded transcript for `name`, played through the real client.
    pub async fn replay(name: &str) -> Ctx {
        let run = Run::new(&std::env::temp_dir().join("chirality-engine-replay"), name);
        let places = run.places(0);
        let replay = Replay::new(transcript::read(name), places.clone());
        let tools = apptools::for_scenario(name);
        let engine = match Engine::start_with(run.config(tools.as_ref()), replay.clone(), None).await {
            Ok(engine) => engine,
            Err(e) => panic!("the engine did not start: {e}; {:?}", replay.failure()),
        };
        let events = engine.subscribe();
        Ctx { engine, events, run, tape: None, tools, mode: Mode::Replay(replay), places }
    }

    /// Reads a thread, keeping the view on the tape if there is one.
    pub async fn read_thread(&self, thread: &ThreadId) -> ThreadView {
        let view = self.engine.read_thread(thread).await.expect("the thread reads");
        if let Some(tape) = &self.tape {
            let mut tape = tape.lock().unwrap();
            tape.drain();
            let at = tape.events.len();
            tape.reads.push((at, view.clone()));
        }
        view
    }

    pub fn is_live(&self) -> bool {
        matches!(self.mode, Mode::Live { .. })
    }

    /// Scripts the stub model for turns whose message carries `STUB[key]`.
    pub fn script(&self, key: &str, script: Script) {
        if let Mode::Live { stub, .. } = &self.mode {
            stub.script(key, script);
        }
    }

    /// Ends Codex as if it had died (in a replay, the transcript does).
    pub fn kill_codex(&self) {
        self.engine.inner().kill_codex();
    }

    /// The next event `pick` accepts, skipping others.
    pub async fn wait<T>(&mut self, what: &str, mut pick: impl FnMut(&Event) -> Option<T>) -> T {
        let limit = if self.is_live() { Duration::from_secs(60) } else { Duration::from_secs(10) };
        let deadline = tokio::time::Instant::now() + limit;
        loop {
            if let Some(failure) = self.replay_failure() {
                panic!("replay failed waiting for {what}: {failure}");
            }
            let step = (tokio::time::Instant::now() + Duration::from_millis(200)).min(deadline);
            let event = match tokio::time::timeout_at(step, self.events.recv()).await {
                Ok(Ok(numbered)) => numbered.event,
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
                Ok(Err(e)) => panic!("events ended waiting for {what}: {e}"),
                Err(_) if tokio::time::Instant::now() < deadline => continue,
                Err(_) => panic!("timed out waiting for {what}"),
            };
            if let Some(found) = pick(&event) {
                return found;
            }
        }
    }

    pub async fn turn_ended(&mut self, thread: &ThreadId) -> (TurnId, TurnState) {
        self.wait("the turn to end", |e| match e {
            Event::TurnEnded { thread: t, turn, state } if t == thread => Some((turn.clone(), state.clone())),
            _ => None,
        })
        .await
    }

    /// The application's tools this scenario runs with.
    pub fn app_tools(&self) -> Arc<TestTools> {
        self.tools.clone().expect("this scenario has the application's tools")
    }

    pub async fn pending(&mut self, thread: &ThreadId) -> PendingRequest {
        self.wait("a request", |e| match e {
            Event::Pending { request } if &request.thread == thread => Some(request.clone()),
            _ => None,
        })
        .await
    }

    fn replay_failure(&self) -> Option<String> {
        match &self.mode {
            Mode::Replay(replay) => replay.failure(),
            Mode::Live { .. } => None,
        }
    }

    /// Ends the run: shuts the engine down, writes or checks the transcript.
    pub async fn finish(self, name: &str) {
        match &self.mode {
            Mode::Live { recorder: Some(recorder), .. } => {
                recorder.stop();
                self.engine.shutdown().await;
                transcript::write(name, &recorder.finish(name, self.places.clone()));
            }
            Mode::Live { recorder: None, .. } => self.engine.shutdown().await,
            Mode::Replay(replay) => {
                self.engine.shutdown().await;
                replay.finish();
            }
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        let _ = std::fs::remove_dir_all(&self.run.root);
    }
}

/// The settings scenarios run with; the stub accepts any model.
pub fn settings(access: Access) -> TurnSettings {
    TurnSettings { model: "gpt-6.2-sol".into(), effort: Some(Effort("low".into())), access }
}

pub fn text(text: &str) -> Message {
    Message { text: text.into(), attachments: Vec::new(), workflow: None }
}
