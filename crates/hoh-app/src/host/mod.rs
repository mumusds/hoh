//! The Engine host of one project (REQ-022, REQ-036, REQ-041, REQ-062; docs/adr/ADR-016.md).
//!
//! Start, in order: the database location is checked read-only; the project lock (a `flock` on the `.hoh` directory,
//! the holder's pid in `.hoh/host.lock`) is taken; the store is opened (the location checked again, the WAL self-test,
//! settings, migrations) and the lock handed to its writer; the host epoch moves on after an integrity check; the
//! readers are checked; only then are the loopback port and the Unix socket `.hoh/host.sock` bound and
//! `.hoh/host.json` written.
//! Any failure before that ends the start with nothing served, and no database file is made when the location or the
//! WAL self-test refuses.

pub mod agents;
mod cgroup;
mod control;
mod http;
mod processes;
pub mod product;
mod reconcile;
mod runner;
pub use processes::ATTEMPT_ENV;
pub use reconcile::ReconcileSettings;
pub use runner::{Executor, Gate, Outcome, Refusal, RunnerConfig, RunnerPoint, Started, Work};

/// Set by the client on a host it starts directly, not in a delegated scope: why (docs/adr/ADR-021.md decision 2). The
/// host records it in host.json, so `hoh doctor` reports a running host's actual reason (CR-012, CR12-03).
pub const UNSCOPED_ENV: &str = "HOH_HOST_UNSCOPED";

use hoh_core::persistence::{self, Options, PersistError, Reader, Store, location};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// How the writer queue and the readers are sized (REQ-018 `writer_queue_capacity`, REQ-062).
const WRITER_QUEUE: usize = 256;
const READERS: usize = 4;
/// Unix socket paths longer than this do not fit `sockaddr_un`.
const SOCKET_PATH_MAX: usize = 107;

/// What a host serves and how.
#[derive(Clone, Debug)]
pub struct HostConfig {
    /// The project directory; the host's files are in its `.hoh/`.
    pub project: PathBuf,
    /// The loopback port; 0 takes a free one (written to `host.json`).
    pub port: u16,
    /// The store's settings (writer queue, busy timeout, WAL probe, fault seams).
    pub store: Options,
    /// The mount table to check the location against; `None` reads `/proc/self/mountinfo`. Set only by tests.
    #[doc(hidden)]
    pub mountinfo: Option<String>,
    /// Fault injection points; the product's [`NoHostFaults`] does nothing (CR-006).
    pub hooks: Arc<dyn HostHooks>,
    /// How the reconcile at the start stops old processes and looks for effects (ADR-019 decision 5).
    pub reconcile: ReconcileSettings,
    /// The runner, where dispatched work runs; `None` (the product until agents are bound to roles, M2-T07): no
    /// runner, nothing is dispatched by the host (ADR-019 decision 8).
    pub runner: Option<RunnerConfig>,
    /// The time leases are renewed, expired and granted and retries come due at: in the product the system time at
    /// the start moved on monotonically, its wall clock for absolute deadlines (ADR-020 decision 4); tests pass their
    /// own (CR-006 F2).
    pub clock: HostClock,
    /// The seed of the retries' jitter (REQ-042, ADR-020 decision 2): `None` in the product, drawn from the system at
    /// the start; tests fix it so their delays replay.
    pub jitter_seed: Option<u64>,
    /// The cancel ladder's graces (REQ-043 decision 1: the native cancel's and SIGTERM's are
    /// `execution.cancel_native_grace_seconds` and `cancel_term_grace_seconds`, M2T07R-254).
    pub cancel: hoh_core::cancel::Graces,
    /// The effective configuration at the start (docs/adr/ADR-022.md decisions 1 and 6): its process settings are
    /// this host's ([`HostConfig::product`]); a new run reads the file again for its team, budget and scheduler.
    pub config: hoh_core::config::Config,
}

/// The host's TCP port unless another is configured (docs/11-deployment.md: `ssh -L 8787`).
pub const DEFAULT_PORT: u16 = 8787;

/// The host's clock (a port, [`hoh_core::delivery::Clock`]).
#[derive(Clone)]
pub struct HostClock(pub Arc<dyn hoh_core::delivery::Clock>);

impl std::fmt::Debug for HostClock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HostClock")
    }
}

impl HostConfig {
    /// The product's settings for `project`.
    pub fn new(project: &Path) -> HostConfig {
        HostConfig {
            project: project.to_path_buf(),
            port: DEFAULT_PORT,
            store: Options {
                queue: WRITER_QUEUE,
                ..Options::default()
            },
            mountinfo: None,
            hooks: Arc::new(NoHostFaults),
            reconcile: ReconcileSettings::default(),
            runner: None,
            clock: HostClock(Arc::new(hoh_core::delivery::MonotonicClock::new())),
            jitter_seed: None,
            cancel: hoh_core::cancel::GRACES,
            config: hoh_core::config::Config::default(),
        }
    }

    /// The product's host for `project` with its effective configuration `config` (docs/adr/ADR-022.md decision 17):
    /// its runner runs the roles' agents with the product executor, found on `path` and started through the gate
    /// `gate` (`hoh` itself); the configuration's process settings apply (the writer queue, the cancel graces, the
    /// reader slots, the port).
    pub fn product(
        project: &Path,
        config: hoh_core::config::Config,
        gate: std::path::PathBuf,
        path: std::ffi::OsString,
    ) -> HostConfig {
        let mut host = HostConfig::new(project);
        let execution = &config.execution;
        let graces = (
            std::time::Duration::from_secs(u64::from(execution.cancel_native_grace_seconds)),
            std::time::Duration::from_secs(u64::from(execution.cancel_term_grace_seconds)),
        );
        host.port = config.web.port;
        host.store.queue =
            usize::try_from(config.persistence.writer_queue_capacity).unwrap_or(WRITER_QUEUE);
        host.reconcile.native_grace = graces.0;
        host.reconcile.term_grace = graces.1;
        host.cancel = hoh_core::cancel::Graces {
            native: graces.0,
            term: graces.1,
            ..hoh_core::cancel::GRACES
        };
        host.runner = Some(RunnerConfig {
            executor: Arc::new(product::ProductExecutor::new(
                Arc::new(agents::Registry::new(path)),
                project.to_path_buf(),
                gate,
                graces,
            )),
            context: hoh_core::decision::Context {
                reader_slots: Some(execution.reader_slots),
                ..hoh_core::decision::Context::default()
            },
        });
        host.config = config;
        host
    }
}

/// The host's fault injection points (REQ-036, CR-006, approved by the project owner): the product's
/// implementation, [`NoHostFaults`], does nothing; implementations that make faults are test code, passed in process
/// through [`HostConfig::hooks`].
pub trait HostHooks: Send + Sync + std::fmt::Debug {
    /// After a command committed, before its response is sent (AC-041-04: a test's implementation drops the
    /// connection here).
    fn after_commit(&self) {}

    /// At `point` of an attempt the runner runs (AC-041-01: a test's implementation ends the process here).
    fn runner(&self, _point: RunnerPoint) {}

    /// Just after the host announced itself (host.json written), before it returns from its start.
    fn announced(&self) {}

    /// Between reading the team's revision and submitting the configuration file's revision over it (a test's
    /// implementation moves the team on here, M2T07R-454).
    fn before_file_revision(&self) {}

    /// Where the runner lists processes for the termination proof of an attempt whose agent ended, instead of `/proc`
    /// (a test's tree makes the proof fail, M2T06R-251).
    fn proc_root(&self) -> Option<std::path::PathBuf> {
        None
    }

    /// Whether the host may use a delegated cgroup it is in (a test of the /proc mode says no, so it does not change
    /// mode with the cgroup it happens to run in, M2T06R-452).
    fn cgroups_allowed(&self) -> bool {
        true
    }
}

/// The product's host hooks: none.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoHostFaults;

impl HostHooks for NoHostFaults {}

/// Why a host did not start, and the exit code for it.
#[derive(Debug)]
pub struct StartError {
    pub exit_code: u8,
    pub message: String,
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for StartError {}

impl StartError {
    /// The location or the database refuses (DB_ON_NETWORK_FS, DB_WAL_UNSUPPORTED, settings, schema): 78, the
    /// environment is not usable as configured.
    fn store(e: PersistError) -> StartError {
        let exit_code = match e {
            PersistError::OnNetworkFs { .. }
            | PersistError::WalUnsupported { .. }
            | PersistError::Pragma { .. }
            | PersistError::SchemaTooNew { .. } => 78,
            _ => 1,
        };
        StartError {
            exit_code,
            message: e.to_string(),
        }
    }

    fn other(message: impl Into<String>) -> StartError {
        StartError {
            exit_code: 1,
            message: message.into(),
        }
    }
}

/// A running host.
pub struct Host {
    pub project_id: String,
    pub host_epoch: i64,
    pub port: u16,
    pub socket: PathBuf,
    pub token: String,
    shared: Arc<http::Shared>,
    stop: tokio::sync::watch::Sender<bool>,
    served: Vec<tokio::task::JoinHandle<()>>,
    /// host.json and the socket, removed at the stop or when the Host is dropped.
    host_files: HostFiles,
}

impl std::fmt::Debug for Host {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Host")
            .field("project_id", &self.project_id)
            .field("host_epoch", &self.host_epoch)
            .field("port", &self.port)
            .field("socket", &self.socket)
            .finish_non_exhaustive()
    }
}

/// The connection settings a host checked at start, for diagnostics (REQ-062).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostics {
    pub writer: persistence::Pragmas,
    pub readers: Vec<persistence::Pragmas>,
    /// Every reader refused a write.
    pub readers_read_only: bool,
}

impl Host {
    /// Start the host of `config.project` (see the module text for the order).
    pub async fn start(config: HostConfig) -> Result<Host, StartError> {
        let files = config.project.join(".hoh");
        let mountinfo = match &config.mountinfo {
            Some(text) => text.clone(),
            None => std::fs::read_to_string("/proc/self/mountinfo")
                .map_err(|e| StartError::other(format!("cannot read /proc/self/mountinfo: {e}")))?,
        };
        // Read-only first: nothing is made on a location that will be refused.
        let existing =
            location::existing_ancestor(&files).map_err(|e| StartError::other(e.to_string()))?;
        location::check(&existing, &mountinfo).map_err(|fstype| {
            StartError::store(PersistError::OnNetworkFs {
                path: existing.clone(),
                fstype,
            })
        })?;
        std::fs::create_dir_all(&files)
            .map_err(|e| StartError::other(format!("cannot make {}: {e}", files.display())))?;
        let lock = take_lock(&files)?;
        let store =
            Store::open_with_mountinfo(&files.join("state.db"), config.store.clone(), &mountinfo)
                .map_err(StartError::store)?;
        // The writer holds the lock from here on: it lets the project go only after its last write (ADR-016
        // decision 12). A start that fails from here waits for that before it returns.
        let ended = store.writer_ended();
        if let Err(e) = store.keep_until_closed(Box::new(lock)).await {
            drop(store);
            ended.await;
            return Err(StartError::store(e));
        }
        match Self::serve_store(config, files, store).await {
            Ok(host) => Ok(host),
            Err(e) => {
                ended.await;
                Err(e)
            }
        }
    }

    /// The rest of the start, with the store open and holding the lock; on an error the store is gone when it
    /// returns.
    async fn serve_store(
        config: HostConfig,
        files: PathBuf,
        store: Store,
    ) -> Result<Host, StartError> {
        let repo =
            std::fs::canonicalize(&config.project).map_err(|e| StartError::other(e.to_string()))?;
        let started = store
            .begin_host(&new_id(), &repo.to_string_lossy())
            .await
            .map_err(StartError::store)?;
        let mut readers = Vec::with_capacity(READERS);
        for _ in 0..READERS {
            let reader = store.reader().map_err(StartError::store)?;
            if !reader.refuses_writes() {
                return Err(StartError::other(
                    "a read-only connection accepted a write; the host refuses to start",
                ));
            }
            readers.push(reader);
        }
        let jitter = Arc::new(runner::Jitter::new(match config.jitter_seed {
            Some(seed) => seed,
            None => {
                let mut bytes = [0u8; 8];
                fill_random(&mut bytes)
                    .map_err(|e| StartError::other(format!("cannot seed the retry jitter: {e}")))?;
                u64::from_le_bytes(bytes)
            }
        }));
        // Every unfinished attempt is settled before anything is served (REQ-041 b1: no write command before it).
        reconcile::run(
            &store,
            &started.project_id,
            started.host_epoch,
            &config.reconcile,
            config.clock.0.as_ref(),
            &jitter,
        )
        .await
        .map_err(StartError::store)?;
        let socket = files.join("host.sock");
        if socket.as_os_str().len() > SOCKET_PATH_MAX {
            return Err(StartError::other(format!(
                "the socket path {} is longer than {SOCKET_PATH_MAX} bytes",
                socket.display()
            )));
        }
        // The port and the token first: nothing that can fail comes between binding the socket and the guard that
        // removes it (M2T12R-08, -153).
        // The default port taken (another project's host, a test) is no reason to fail: a free one is taken and
        // host.json names it; clients use the socket (docs/adr/ADR-021.md decision 3).
        let tcp = match tokio::net::TcpListener::bind(("127.0.0.1", config.port)).await {
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse && config.port == DEFAULT_PORT => {
                tokio::net::TcpListener::bind(("127.0.0.1", 0)).await
            }
            bound => bound,
        }
        .map_err(|e| StartError::other(format!("cannot bind 127.0.0.1:{}: {e}", config.port)))?;
        let port = tcp
            .local_addr()
            .map_err(|e| StartError::other(e.to_string()))?
            .port();
        let token = token()?;
        remove_stale_socket(&socket)?;
        let unix = tokio::net::UnixListener::bind(&socket)
            .map_err(|e| StartError::other(format!("cannot bind {}: {e}", socket.display())))?;
        let mut host_files = HostFiles {
            files: files.clone(),
            socket: socket.clone(),
            socket_id: file_id(&socket),
            token: token.clone(),
            armed: true,
        };
        if host_files.socket_id.is_none() {
            return Err(StartError::other(format!(
                "cannot tell the socket {} it bound",
                socket.display()
            )));
        }
        set_mode(&socket, 0o600)?;
        let (stop, stopped) = tokio::sync::watch::channel(false);
        // Whether its attempts get cgroups of their own, or why not (decision 8d, M2T06R-557).
        let delegated = if config.hooks.cgroups_allowed() {
            cgroup::Delegated::detect()
        } else {
            Err("its hooks do not allow cgroups (a test's hook, CR-006)".to_owned())
        };
        let shared = Arc::new(http::Shared {
            store,
            readers: Mutex::new(readers),
            project_id: started.project_id.clone(),
            token: token.clone(),
            port,
            hooks: config.hooks.clone(),
            stop: stopped.clone(),
            subscribers: std::sync::atomic::AtomicUsize::new(0),
            opened: std::sync::atomic::AtomicUsize::new(0),
            cgroups: delegated.as_ref().ok().cloned(),
            root: config.project.clone(),
        });
        // The project's configuration file as a team revision, once at the start (docs/adr/ADR-022.md decision 6).
        if let Err(e) = http::sync_config(&shared).await {
            use std::io::Write as _;
            let _ = writeln!(
                std::io::stderr(),
                "hoh host: the project's configuration is invalid, its team is not used: {e}"
            );
        }
        let mut served = vec![
            tokio::spawn(http::serve(
                http::Listener::Tcp(tcp),
                shared.clone(),
                stopped.clone(),
            )),
            tokio::spawn(http::serve(
                http::Listener::Unix(unix),
                shared.clone(),
                stopped.clone(),
            )),
        ];
        // The runner after the reconcile (every dispatch it makes or takes comes after reconcile.completed), stopped
        // with the listeners and before the store closes (ADR-019 decision 8).
        let inflight: control::Inflight = Arc::new(Mutex::new(std::collections::BTreeMap::new()));
        if let Some(runner) = config.runner.clone() {
            served.push(tokio::spawn(runner::run(
                shared.clone(),
                runner,
                runner::Setup {
                    clock: config.clock.0.clone(),
                    settings: config.reconcile.clone(),
                    host_epoch: started.host_epoch,
                    jitter: jitter.clone(),
                    inflight: inflight.clone(),
                },
                stopped.clone(),
            )));
        }
        // Pause and cancel move on with or without an executor (docs/adr/ADR-021.md decision 7).
        served.push(tokio::spawn(control::run(
            shared.clone(),
            config.clock.0.clone(),
            config.runner.as_ref().map(|r| r.executor.clone()),
            inflight,
            repo.clone(),
            config.cancel,
            stopped,
        )));
        let written = write_host_json(
            &files,
            &serde_json::json!({
                "pid": std::process::id(),
                "project_id": started.project_id,
                "host_epoch": started.host_epoch,
                "port": port,
                "socket": socket,
                "token": token,
                // How its attempts' processes are tracked, as decided at its start (docs/adr/ADR-021.md decision 8d,
                // M2T06R-402): `hoh doctor` reports this, not a guess.
                "process_tracking": if shared.cgroups.is_some() { "cgroup" } else { "proc" },
                "process_tracking_reason": match (&delegated, std::env::var(UNSCOPED_ENV)) {
                    (Ok(_), _) => "its own cgroup is delegated: each attempt gets a cgroup of its own".to_owned(),
                    (Err(_), Ok(why)) if !why.is_empty() => format!(
                        "its own cgroup is not delegated: the client started it directly because {why}"
                    ),
                    // Its own finding (M2T06R-557): a scope or unit that did not delegate, an old kernel, a test's hook.
                    (Err(why), _) => why.clone(),
                },
            }),
        );
        if let Err(e) = written {
            // Stop serving; the files go with their guard and the store (and the lock with it) when this returns
            // (M2T12R-08).
            let _ = stop.send(true);
            for task in served.iter_mut() {
                let _ = task.await;
            }
            host_files.remove();
            return Err(e);
        }
        config.hooks.announced();
        Ok(Host {
            project_id: started.project_id,
            host_epoch: started.host_epoch,
            port,
            socket,
            token,
            shared,
            stop,
            served,
            host_files,
        })
    }

    /// The host's store: the loops of the host write through it (the Commander's decisions, M2-T08), and so do tests
    /// of a host built in the test target (CR-006 F3).
    pub fn store(&self) -> &Store {
        &self.shared.store
    }

    /// How many event streams are open now (docs/adr/ADR-021.md decision 6: an attach that does not pick a run
    /// subscribes to none).
    pub fn subscribers(&self) -> usize {
        self.shared
            .subscribers
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// How many event streams were opened since the host started.
    pub fn streams_opened(&self) -> usize {
        self.shared.opened.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Whether the database writer still runs (it ends only at the stop, or on a panic, M2T12R-150).
    pub fn writer_alive(&self) -> bool {
        self.shared.store.writer_alive()
    }

    /// Resolves once the database writer has ended: before the stop only by a panic (false), which a caller treats
    /// as fatal the way [`run`] does (M2T12R-185).
    pub fn writer_ended(&self) -> impl std::future::Future<Output = bool> + Send + 'static {
        self.shared.store.writer_ended()
    }

    /// The settings of the writer and of every reader, and whether the readers refuse writes.
    pub async fn diagnostics(&self) -> Result<Diagnostics, PersistError> {
        let writer = self.shared.store.writer_pragmas().await?;
        let (readers, readers_read_only) = {
            let pool = self
                .shared
                .readers
                .lock()
                .map_err(|_| PersistError::Storage("the reader pool is poisoned".to_owned()))?;
            let mut settings = Vec::with_capacity(pool.len());
            for reader in pool.iter() {
                settings.push(reader.pragmas()?);
            }
            (settings, pool.iter().all(Reader::refuses_writes))
        };
        Ok(Diagnostics {
            writer,
            readers,
            readers_read_only,
        })
    }

    /// Stop (ADR-016 decision 12): no new connections, event streams end, every connection finishes or is ended
    /// after the grace; `host.json` and the socket go; then the store, and the writer ends after its last write and
    /// lets the project lock go. Returns after that: true when the writer finished, false when it had ended by a panic
    /// (the lock is then still held until the process ends). The database stays. A Host dropped without this stops
    /// the same way, without waiting.
    pub async fn shutdown(self) -> bool {
        let Host {
            shared,
            stop,
            served,
            mut host_files,
            ..
        } = self;
        let _ = stop.send(true);
        for task in served {
            let _ = task.await;
        }
        host_files.remove();
        let ended = shared.store.writer_ended();
        drop(shared);
        drop(stop);
        ended.await
    }
}

/// Run the host of `project` until SIGTERM or SIGINT; the process exit code.
pub async fn run(config: HostConfig) -> u8 {
    run_ready(config, || {}).await
}

/// [`run`], calling `ready` once the host serves (its `host.json` written). A hang-up (SIGHUP) is logged and changes
/// nothing: a host outlives the terminal and the shell it was started from (REQ-005 behaviour 2, docs/adr/ADR-021.md
/// decision 1).
pub async fn run_ready(config: HostConfig, ready: impl FnOnce()) -> u8 {
    // The stop signals are taken before the host announces itself (host.json): a SIGTERM that comes as soon as the file
    // is there stops the host as any other does, not by the default action (ADR-019 decision 10).
    use tokio::signal::unix::{SignalKind, signal};
    let stops = (
        signal(SignalKind::terminate()),
        signal(SignalKind::interrupt()),
    );
    if let Ok(mut hangups) = signal(SignalKind::hangup()) {
        tokio::spawn(async move {
            while hangups.recv().await.is_some() {
                use std::io::Write as _;
                let _ = writeln!(
                    std::io::stderr(),
                    "hoh host: a hang-up (SIGHUP) changes nothing"
                );
            }
        });
    }
    let host = match Host::start(config).await {
        Ok(host) => host,
        Err(e) => {
            use std::io::Write as _;
            let _ = writeln!(std::io::stderr(), "hoh host: {e}");
            return e.exit_code;
        }
    };
    ready();
    let signals = async {
        match stops {
            (Ok(mut term), Ok(mut int)) => {
                tokio::select! {
                    _ = term.recv() => {}
                    _ = int.recv() => {}
                }
            }
            _ => std::future::pending::<()>().await,
        }
    };
    // A writer that ended while the host was not stopping (a panic) is fatal: the host stops and the process exits
    // non-zero, so its supervisor sees the failure and starts a new host once the process has let the project go
    // (M2T12R-150). So is a panic of the writer during the stop (M2T12R-185).
    let writer_ended = host.writer_ended();
    let ended_first = tokio::select! {
        () = signals => false,
        _ = writer_ended => true,
    };
    let finished = host.shutdown().await;
    if ended_first || !finished {
        use std::io::Write as _;
        let _ = writeln!(
            std::io::stderr(),
            "hoh host: the database writer ended; the host stopped"
        );
        return 1;
    }
    0
}

/// The device and inode of `path`, not following a link.
fn file_id(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(path)
        .ok()
        .map(|m| (m.dev(), m.ino()))
}

/// A host's `host.json` and socket, removed once: at the stop, on a failed start, or when the Host is dropped.
/// `host.json` goes only if it is this host's (its token) and the socket only if it is the one this host bound (its
/// inode); a later host's files are never touched (M2T12R-120, -153).
struct HostFiles {
    files: PathBuf,
    socket: PathBuf,
    socket_id: Option<(u64, u64)>,
    token: String,
    armed: bool,
}

impl HostFiles {
    fn remove(&mut self) {
        if !std::mem::take(&mut self.armed) {
            return;
        }
        let host_json = self.files.join("host.json");
        let ours = std::fs::read(&host_json)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .is_some_and(|info| info["token"].as_str() == Some(self.token.as_str()));
        if ours {
            let _ = std::fs::remove_file(&host_json);
        }
        if self.socket_id.is_some() && file_id(&self.socket) == self.socket_id {
            let _ = std::fs::remove_file(&self.socket);
        }
    }
}

impl Drop for HostFiles {
    fn drop(&mut self) {
        self.remove();
    }
}

/// The project lock: an exclusive `flock` on the `.hoh` directory itself (a lock on a file in it could be deleted and
/// taken again by a second host, M2T12R-152); `host.lock` only names the holder's pid. A database file moved into a
/// new `.hoh` while a host runs is not covered: the host-epoch fencing of writes is (M2-T05, ADR-016 decision 1).
struct ProjectLock {
    _directory: std::fs::File,
    _pid: std::fs::File,
}

/// Take the project lock without waiting; a held lock names its holder. The directory is locked first: a start that is
/// refused only reads the pid file (it neither makes nor changes one), and a pid file that cannot be opened does not
/// hide a held project; only the holder (re)writes it (M2T12R-182).
fn take_lock(files: &Path) -> Result<ProjectLock, StartError> {
    use std::io::{Seek as _, Write as _};
    let directory = std::fs::File::open(files)
        .map_err(|e| StartError::other(format!("cannot open {}: {e}", files.display())))?;
    let pid_path = files.join("host.lock");
    if rustix::fs::flock(
        &directory,
        rustix::fs::FlockOperation::NonBlockingLockExclusive,
    )
    .is_err()
    {
        let holder = std::fs::read_to_string(&pid_path)
            .ok()
            .map(|pid| pid.trim().to_owned())
            .filter(|pid| !pid.is_empty())
            .unwrap_or_else(|| "unknown".to_owned());
        return Err(StartError {
            exit_code: 69,
            message: format!(
                "another host holds {} (pid {holder}); do not remove its files while it runs",
                files.display()
            ),
        });
    }
    let mut pid = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&pid_path)
        .map_err(|e| StartError::other(format!("cannot open {}: {e}", pid_path.display())))?;
    let written = pid
        .set_len(0)
        .and_then(|()| pid.rewind())
        .and_then(|()| pid.write_all(std::process::id().to_string().as_bytes()));
    written.map_err(|e| StartError::other(format!("cannot write {}: {e}", pid_path.display())))?;
    Ok(ProjectLock {
        _directory: directory,
        _pid: pid,
    })
}

/// A socket left by a host that ended without cleaning up is removed (under the lock, so no host uses it); anything
/// else at that path is not touched.
fn remove_stale_socket(path: &Path) -> Result<(), StartError> {
    use std::os::unix::fs::FileTypeExt;
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_socket() => std::fs::remove_file(path)
            .map_err(|e| StartError::other(format!("cannot remove {}: {e}", path.display()))),
        Ok(_) => Err(StartError::other(format!(
            "{} exists and is not a socket",
            path.display()
        ))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(StartError::other(format!("{}: {e}", path.display()))),
    }
}

fn set_mode(path: &Path, mode: u32) -> Result<(), StartError> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .map_err(|e| StartError::other(format!("cannot set the mode of {}: {e}", path.display())))
}

/// `host.json` written whole (a temporary file, synced, renamed), readable by the owner only.
fn write_host_json(files: &Path, value: &serde_json::Value) -> Result<(), StartError> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt;
    let final_path = files.join("host.json");
    let temporary = files.join(format!("host.json.{}", std::process::id()));
    let write = || -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(value.to_string().as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&temporary, &final_path)
    };
    write().map_err(|e| {
        let _ = std::fs::remove_file(&temporary);
        StartError::other(format!("cannot write {}: {e}", final_path.display()))
    })
}

/// 32 random bytes in hex: the per-host token (openapi `localToken`).
fn token() -> Result<String, StartError> {
    random_hex::<32>().map_err(|e| StartError::other(format!("no random bytes for the token: {e}")))
}

/// A random UUID (version 4) for ids the host makes.
pub(crate) fn new_id() -> String {
    let mut bytes = [0u8; 16];
    fill_random(&mut bytes).unwrap_or_else(|_| {
        // Without the kernel's random source, the time and pid still make a unique id in this database.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        bytes = (nanos ^ (u128::from(std::process::id()) << 96)).to_be_bytes();
    });
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

fn random_hex<const N: usize>() -> std::io::Result<String> {
    let mut bytes = [0u8; N];
    fill_random(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn fill_random(bytes: &mut [u8]) -> std::io::Result<()> {
    let mut filled = 0;
    while filled < bytes.len() {
        filled +=
            rustix::rand::getrandom(&mut bytes[filled..], rustix::rand::GetRandomFlags::empty())?;
    }
    Ok(())
}
