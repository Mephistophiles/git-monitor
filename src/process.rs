use anyhow::{bail, Context, Result};
static WORKER_EXE: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
pub fn set_worker_executable(path: std::path::PathBuf) {
    let _ = WORKER_EXE.set(path);
}
pub fn worker_executable() -> Result<std::path::PathBuf> {
    Ok(WORKER_EXE
        .get()
        .cloned()
        .unwrap_or(std::env::current_exe()?))
}
use std::{
    io::{Read, Write},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

#[derive(Clone, Default)]
pub struct Shutdown {
    at: Arc<Mutex<Option<Instant>>>,
}
impl Shutdown {
    pub fn install() -> Result<Self> {
        let s = Self::default();
        let signal = s.clone();
        ctrlc::set_handler(move || {
            let mut at = signal.at.lock().unwrap();
            if at.is_none() {
                *at = Some(Instant::now());
                eprintln!("shutdown requested; draining active work");
            } else {
                *at = Some(Instant::now() - Duration::from_secs(86400));
            }
        })?;
        #[cfg(unix)]
        unsafe {
            libc::signal(libc::SIGHUP, libc::SIG_IGN);
        }
        Ok(s)
    }
    pub fn requested(&self) -> bool {
        self.at.lock().unwrap().is_some()
    }
    pub fn expired(&self, grace: Duration) -> bool {
        self.at
            .lock()
            .unwrap()
            .is_some_and(|t| t.elapsed() >= grace)
    }
}

pub struct Output {
    pub code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[cfg(unix)]
fn nonblocking(pipe: &impl std::os::fd::AsRawFd) -> std::io::Result<()> {
    let fd = pipe.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags == -1 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(unix))]
fn nonblocking<T>(_: &T) -> std::io::Result<()> {
    Ok(())
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
    let group = std::env::var_os("GIT_MONITOR_WORKER").is_none();
    command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        if group {
            command.process_group(0);
        }
        if let Some(bytes) = memory {
            unsafe {
                command.pre_exec(move || {
                    let l = libc::rlimit {
                        rlim_cur: bytes as libc::rlim_t,
                        rlim_max: bytes as libc::rlim_t,
                    };
                    if libc::setrlimit(libc::RLIMIT_AS, &l) != 0 {
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
    let mut child = command.spawn().context("start child process")?;
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    nonblocking(&stdout)?;
    nonblocking(&stderr)?;
    if let Some(stdin) = &child.stdin {
        nonblocking(stdin)?;
    }
    let cancelled = Arc::new(AtomicBool::new(false));
    let writer = input.map(|bytes| {
        let mut stdin = child.stdin.take().unwrap();
        let cancelled = cancelled.clone();
        std::thread::spawn(move || {
            let mut remaining = bytes.as_slice();
            while !remaining.is_empty() {
                if cancelled.load(Ordering::Relaxed) {
                    return Err(std::io::Error::from(std::io::ErrorKind::Interrupted));
                }
                match stdin.write(remaining) {
                    Ok(0) => return Err(std::io::ErrorKind::WriteZero.into()),
                    Ok(n) => remaining = &remaining[n..],
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(e),
                }
            }
            Ok(())
        })
    });
    let exceeded = Arc::new(AtomicBool::new(false));
    fn reader<R: Read + Send + 'static>(
        mut r: R,
        cap: usize,
        exceeded: Arc<AtomicBool>,
        cancelled: Arc<AtomicBool>,
    ) -> std::thread::JoinHandle<std::io::Result<Vec<u8>>> {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let mut buffer = [0; 8192];
            loop {
                if cancelled.load(Ordering::Relaxed) {
                    break;
                }
                let n = match r.read(&mut buffer) {
                    Ok(n) => n,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(e),
                };
                if n == 0 {
                    break;
                }
                if bytes.len() + n > cap {
                    exceeded.store(true, Ordering::Relaxed);
                } else {
                    bytes.extend_from_slice(&buffer[..n]);
                }
            }
            Ok(bytes)
        })
    }
    let out = reader(stdout, limit, exceeded.clone(), cancelled.clone());
    let err = reader(stderr, 65536, exceeded.clone(), cancelled.clone());
    let start = Instant::now();
    let mut failure = None;
    let mut exited = None;
    let status = loop {
        if exited.is_none() {
            exited = child.try_wait()?;
        }
        // A transport/helper can outlive its parent while holding a pipe open.
        // Keep enforcing deadlines until every I/O thread has finished too.
        if let Some(status) = exited {
            if out.is_finished()
                && err.is_finished()
                && writer.as_ref().is_none_or(|w| w.is_finished())
            {
                break status;
            }
        }
        if exceeded.load(Ordering::Relaxed) || start.elapsed() > timeout || shutdown.expired(grace)
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
                    libc::kill(-(child.id() as i32), libc::SIGKILL);
                }
            }
            let _ = child.kill();
            cancelled.store(true, Ordering::Relaxed);
            break child.wait()?;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let stdout = out
        .join()
        .map_err(|_| anyhow::anyhow!("stdout reader panicked"))??;
    let stderr = err
        .join()
        .map_err(|_| anyhow::anyhow!("stderr reader panicked"))??;
    let written = writer.map(|w| {
        w.join()
            .map_err(|_| anyhow::anyhow!("stdin writer panicked"))
    });
    if let Some(reason) = failure {
        bail!("{reason}");
    }
    if exceeded.load(Ordering::Relaxed) {
        bail!("process output budget exceeded");
    }
    if status.success() {
        if let Some(result) = written {
            result??;
        }
    }
    Ok(Output {
        code: status.code().unwrap_or(-1),
        stdout,
        stderr,
    })
}
