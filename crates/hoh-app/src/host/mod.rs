//! The Engine host of one project (REQ-022, REQ-036, REQ-041, REQ-062; docs/adr/ADR-016.md).
//!
//! Start, in order: the database location is checked read-only; the project lock `.hoh/host.lock` is taken; the store
//! is opened (the location checked again, the WAL self-test, settings, migrations); the host epoch moves on after an
//! integrity check; the readers are checked; only then are the Unix socket `.hoh/host.sock` and the loopback port
//! bound and `.hoh/host.json` written. Any failure before that ends the start with nothing served, and no database
//! file is made when the location or the WAL self-test refuses.

mod http;

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
}

impl HostConfig {
    /// The product's settings for `project`.
    pub fn new(project: &Path) -> HostConfig {
        HostConfig {
            project: project.to_path_buf(),
            port: 8787,
            store: Options {
                queue: WRITER_QUEUE,
                ..Options::default()
            },
            mountinfo: None,
            hooks: Arc::new(NoHostFaults),
        }
    }
}

/// The host's fault injection points (REQ-036, CR-006, approved by the project owner): the product's
/// implementation, [`NoHostFaults`], does nothing; implementations that make faults are test code, passed in process
/// through [`HostConfig::hooks`].
pub trait HostHooks: Send + Sync + std::fmt::Debug {
    /// After a command committed, before its response is sent (AC-041-04: a test's implementation drops the
    /// connection here).
    fn after_commit(&self) {}
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
    /// Held while the host runs: the project lock.
    _lock: std::fs::File,
    files: PathBuf,
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
        let lock = take_lock(&files.join("host.lock"))?;
        let store =
            Store::open_with_mountinfo(&files.join("state.db"), config.store.clone(), &mountinfo)
                .map_err(StartError::store)?;
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
        // Reconcile of leased work comes with M2-T05; nothing is leased before it.
        let socket = files.join("host.sock");
        if socket.as_os_str().len() > SOCKET_PATH_MAX {
            return Err(StartError::other(format!(
                "the socket path {} is longer than {SOCKET_PATH_MAX} bytes",
                socket.display()
            )));
        }
        remove_stale_socket(&socket)?;
        let unix = tokio::net::UnixListener::bind(&socket)
            .map_err(|e| StartError::other(format!("cannot bind {}: {e}", socket.display())))?;
        set_mode(&socket, 0o600)?;
        let tcp = tokio::net::TcpListener::bind(("127.0.0.1", config.port))
            .await
            .map_err(|e| {
                StartError::other(format!("cannot bind 127.0.0.1:{}: {e}", config.port))
            })?;
        let port = tcp
            .local_addr()
            .map_err(|e| StartError::other(e.to_string()))?
            .port();
        let token = token()?;
        let shared = Arc::new(http::Shared {
            store,
            readers: Mutex::new(readers),
            project_id: started.project_id.clone(),
            token: token.clone(),
            port,
            hooks: config.hooks.clone(),
        });
        let (stop, stopped) = tokio::sync::watch::channel(false);
        let served = vec![
            tokio::spawn(http::serve_tcp(tcp, shared.clone(), stopped.clone())),
            tokio::spawn(http::serve_unix(unix, shared.clone(), stopped)),
        ];
        write_host_json(
            &files,
            &serde_json::json!({
                "pid": std::process::id(),
                "project_id": started.project_id,
                "host_epoch": started.host_epoch,
                "port": port,
                "socket": socket,
                "token": token,
            }),
        )?;
        Ok(Host {
            project_id: started.project_id,
            host_epoch: started.host_epoch,
            port,
            socket,
            token,
            shared,
            stop,
            served,
            _lock: lock,
            files,
        })
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

    /// Stop serving, then remove `host.json` and the socket; the lock and the database stay.
    pub async fn shutdown(self) {
        let _ = self.stop.send(true);
        for task in self.served {
            let _ = task.await;
        }
        let _ = std::fs::remove_file(self.files.join("host.json"));
        let _ = std::fs::remove_file(&self.socket);
    }
}

/// Run the host of `project` until SIGTERM or SIGINT; the process exit code.
pub async fn run(config: HostConfig) -> u8 {
    let host = match Host::start(config).await {
        Ok(host) => host,
        Err(e) => {
            use std::io::Write as _;
            let _ = writeln!(std::io::stderr(), "hoh host: {e}");
            return e.exit_code;
        }
    };
    let signals = async {
        use tokio::signal::unix::{SignalKind, signal};
        match (
            signal(SignalKind::terminate()),
            signal(SignalKind::interrupt()),
        ) {
            (Ok(mut term), Ok(mut int)) => {
                tokio::select! {
                    _ = term.recv() => {}
                    _ = int.recv() => {}
                }
            }
            _ => std::future::pending::<()>().await,
        }
    };
    signals.await;
    host.shutdown().await;
    0
}

/// Take `.hoh/host.lock` without waiting, with this process's pid in it; a held lock names its holder.
fn take_lock(path: &Path) -> Result<std::fs::File, StartError> {
    use std::io::{Read as _, Seek as _, Write as _};
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|e| StartError::other(format!("cannot open {}: {e}", path.display())))?;
    if rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive).is_err() {
        let mut holder = String::new();
        let _ = file.read_to_string(&mut holder);
        return Err(StartError {
            exit_code: 69,
            message: format!(
                "another host holds {} (pid {})",
                path.display(),
                holder.trim()
            ),
        });
    }
    let written = file
        .set_len(0)
        .and_then(|()| file.rewind())
        .and_then(|()| file.write_all(std::process::id().to_string().as_bytes()));
    written.map_err(|e| StartError::other(format!("cannot write {}: {e}", path.display())))?;
    Ok(file)
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
