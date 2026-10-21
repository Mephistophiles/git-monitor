use anyhow::{bail, Context, Result};
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
    let writer = input.map(|bytes| {
        let mut stdin = child.stdin.take().unwrap();
        std::thread::spawn(move || stdin.write_all(&bytes))
    });
    let exceeded = Arc::new(AtomicBool::new(false));
    fn reader<R: Read + Send + 'static>(
        mut r: R,
        cap: usize,
        exceeded: Arc<AtomicBool>,
    ) -> std::thread::JoinHandle<std::io::Result<Vec<u8>>> {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let mut buffer = [0; 8192];
            loop {
                let n = r.read(&mut buffer)?;
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
    let out = reader(child.stdout.take().unwrap(), limit, exceeded.clone());
    let err = reader(child.stderr.take().unwrap(), 65536, exceeded.clone());
    let start = Instant::now();
    let mut failure = None;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
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
