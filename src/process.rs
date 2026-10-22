use anyhow::{bail, Context, Result};
use std::{
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::{Duration, Instant},
};
static WORKER: OnceLock<std::path::PathBuf> = OnceLock::new();
pub fn set_worker_executable(path: std::path::PathBuf) {
    let _ = WORKER.set(path);
}
pub fn worker_executable() -> Result<std::path::PathBuf> {
    Ok(WORKER.get().cloned().unwrap_or(std::env::current_exe()?))
}
fn runtime() -> Result<&'static tokio::runtime::Runtime> {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    if let Some(runtime) = RUNTIME.get() {
        return Ok(runtime);
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let _ = RUNTIME.set(runtime);
    Ok(RUNTIME.get().unwrap())
}
#[derive(Clone, Default)]
pub struct Shutdown {
    at: Arc<Mutex<Option<Instant>>>,
}
impl Shutdown {
    pub fn install() -> Result<Self> {
        let s = Self::default();
        let runtime = runtime()?;
        #[cfg(unix)]
        runtime.block_on(async {
            use tokio::signal::unix::{signal, SignalKind};
            for kind in [SignalKind::interrupt(), SignalKind::terminate()] {
                let mut signals = signal(kind)?;
                let shutdown = s.clone();
                tokio::spawn(async move {
                    while signals.recv().await.is_some() {
                        shutdown.request();
                    }
                });
            }
            let mut hup = signal(SignalKind::hangup())?;
            tokio::spawn(async move {
                while hup.recv().await.is_some() {
                    eprintln!("SIGHUP ignored; configuration changes require restart");
                }
            });
            Ok::<(), std::io::Error>(())
        })?;
        #[cfg(not(unix))]
        {
            let shutdown = s.clone();
            runtime.spawn(async move {
                while tokio::signal::ctrl_c().await.is_ok() {
                    shutdown.request();
                }
            });
        }
        Ok(s)
    }
    pub(crate) fn request(&self) {
        let mut at = self.at.lock().unwrap();
        if at.is_none() {
            *at = Some(Instant::now());
            eprintln!("shutdown requested; draining active work");
        } else {
            *at = Some(Instant::now() - Duration::from_secs(86400));
        }
    }
    pub fn requested(&self) -> bool {
        self.at.lock().unwrap().is_some()
    }
    pub fn expired(&self, grace: Duration) -> bool {
        self.at
            .lock()
            .unwrap()
            .is_some_and(|at| at.elapsed() >= grace)
    }
}
pub fn pause(delay: Duration, shutdown: &Shutdown) -> Result<()> {
    runtime()?.block_on(async {
        let start = Instant::now();
        while start.elapsed() < delay && !shutdown.requested() {
            tokio::time::sleep(
                Duration::from_millis(50).min(delay.saturating_sub(start.elapsed())),
            )
            .await;
        }
    });
    Ok(())
}
pub struct Output {
    pub code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}
pub fn run(
    command: &mut Command,
    timeout: Duration,
    limit: usize,
    shutdown: &Shutdown,
    grace: Duration,
    memory: Option<u64>,
) -> Result<Output> {
    run_inner(command, timeout, limit, shutdown, grace, memory, None)
}
pub fn run_with_input(
    command: &mut Command,
    input: Vec<u8>,
    timeout: Duration,
    limit: usize,
    shutdown: &Shutdown,
    grace: Duration,
) -> Result<Output> {
    run_inner(command, timeout, limit, shutdown, grace, None, Some(input))
}
fn run_inner(
    command: &mut Command,
    timeout: Duration,
    limit: usize,
    shutdown: &Shutdown,
    grace: Duration,
    memory: Option<u64>,
    input: Option<Vec<u8>>,
) -> Result<Output> {
    if std::path::Path::new(command.get_program())
        .file_name()
        .is_some_and(|p| p == "git")
    {
        crate::metrics::update(|c| c.git_processes += 1);
    }
    let mut command_async = tokio::process::Command::new(command.get_program());
    command_async.args(command.get_args());
    for (key, value) in command.get_envs() {
        if let Some(value) = value {
            command_async.env(key, value);
        } else {
            command_async.env_remove(key);
        }
    }
    if let Some(directory) = command.get_current_dir() {
        command_async.current_dir(directory);
    }
    command_async
        .kill_on_drop(true)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let group = std::env::var_os("GIT_MONITOR_WORKER").is_none();
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        if group {
            command_async.as_std_mut().process_group(0);
        }
        if let Some(bytes) = memory {
            unsafe {
                command_async.as_std_mut().pre_exec(move || {
                    let limit = libc::rlimit {
                        rlim_cur: bytes as libc::rlim_t,
                        rlim_max: bytes as libc::rlim_t,
                    };
                    if libc::setrlimit(libc::RLIMIT_AS, &limit) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
    }
    #[cfg(not(unix))]
    if memory.is_some() {
        bail!("worker address-space limit is unsupported on this platform");
    }
    runtime()?.block_on(async {
        use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
        let mut child = command_async.spawn().context("start child process")?;
        let pid = child.id().context("child PID unavailable")?;
        let writer = input.map(|bytes| {
            let mut stdin = child.stdin.take().unwrap();
            tokio::spawn(async move { stdin.write_all(&bytes).await })
        });
        async fn read<R: AsyncRead + Unpin>(
            mut pipe: R,
            cap: usize,
            exceeded: Arc<AtomicBool>,
        ) -> std::io::Result<Vec<u8>> {
            let mut bytes = vec![];
            let mut buffer = [0; 8192];
            loop {
                let n = pipe.read(&mut buffer).await?;
                if n == 0 {
                    break;
                }
                if bytes.len().saturating_add(n) > cap {
                    exceeded.store(true, Ordering::Relaxed);
                } else {
                    bytes.extend_from_slice(&buffer[..n]);
                }
            }
            Ok(bytes)
        }
        let exceeded = Arc::new(AtomicBool::new(false));
        let out = tokio::spawn(read(child.stdout.take().unwrap(), limit, exceeded.clone()));
        let err = tokio::spawn(read(child.stderr.take().unwrap(), 65536, exceeded.clone()));
        let start = Instant::now();
        let (mut exited, mut failure) = (None, None);
        let status = loop {
            if exited.is_none() {
                exited = child.try_wait()?;
            }
            if let Some(status) = exited {
                if out.is_finished()
                    && err.is_finished()
                    && writer.as_ref().is_none_or(|w| w.is_finished())
                {
                    break status;
                }
            }
            if exceeded.load(Ordering::Relaxed)
                || start.elapsed() > timeout
                || shutdown.expired(grace)
            {
                failure = Some(if exceeded.load(Ordering::Relaxed) {
                    "process output budget exceeded"
                } else if shutdown.expired(grace) {
                    "shutdown deadline exceeded"
                } else {
                    "process timeout"
                });
                #[cfg(unix)]
                if group {
                    unsafe {
                        libc::kill(-(pid as i32), libc::SIGKILL);
                    }
                }
                let _ = child.start_kill();
                out.abort();
                err.abort();
                if let Some(writer) = &writer {
                    writer.abort();
                }
                break child.wait().await?;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        let stdout = out.await;
        let stderr = err.await;
        let written = if let Some(writer) = writer {
            Some(writer.await)
        } else {
            None
        };
        if let Some(reason) = failure {
            bail!("{reason}");
        }
        if exceeded.load(Ordering::Relaxed) {
            bail!("process output budget exceeded");
        }
        let stdout = stdout.context("stdout reader stopped")??;
        let stderr = stderr.context("stderr reader stopped")??;
        if status.success() {
            if let Some(written) = written {
                written.context("stdin writer stopped")??;
            }
        }
        Ok(Output {
            code: status.code().unwrap_or(-1),
            stdout,
            stderr,
        })
    })
}
