//! dn-sandbox (ADR-0021 §2-§3, #435): runs a skill's script in a fresh OS sandbox per run.
//!
//! - **Run folder:** `runs/run-…` (0700) in the app's data folder, with `in/` (copies of the
//!   inputs) and `work/out/` and `work/tmp/`. On macOS, `work/` is a size-capped disk image
//!   attached for the run, so the disk cap is hard.
//! - **Launcher:** the `dn-sandbox-launch` binary gets the run's [`spec::LaunchSpec`], applies
//!   it to itself (new session, file-size cap, no inherited descriptors; on Linux Landlock and
//!   seccomp) and `exec`s the runtime (on macOS through `sandbox-exec` and a Seatbelt
//!   profile). Anything not applied in full refuses the run.
//! - **Watchdog:** this library polls the run for wall clock, memory, disk and output, and
//!   kills it at a limit. Scripts can't start processes (fork is denied on every OS), so the
//!   run is one process.
//! - **Outputs** are read from `out/` only after the run is confirmed dead, regular files
//!   only, never through a link. Checking them by type (§3) is the caller's next step.
//!
//! There's no unsandboxed mode: where the sandbox can't be set up, [`Sandbox::new`] says why.

pub mod spec;

#[cfg(target_os = "linux")]
#[doc(hidden)]
pub mod linux;
#[cfg(target_os = "macos")]
mod macos;

use serde::Serialize;
use spec::{LaunchSpec, REFUSED_EXIT, REFUSED_PREFIX};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// A run's limits (ADR-0021 §2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Limits {
    /// 60 s by default; a manifest may ask for up to [`MAX_WALL`].
    pub wall: Duration,
    /// Resident memory, in bytes.
    pub memory: u64,
    /// `out/` and `tmp/` together, in bytes.
    pub disk: u64,
    /// stdout and stderr, each, in bytes.
    pub output: usize,
}

pub const MAX_WALL: Duration = Duration::from_secs(300);

impl Default for Limits {
    fn default() -> Self {
        Self {
            wall: Duration::from_secs(60),
            memory: 1024 * 1024 * 1024,
            disk: 100 * 1024 * 1024,
            output: 1024 * 1024,
        }
    }
}

/// How each limit is enforced on this OS, for Settings › Skills. Nothing is claimed that
/// isn't enforced.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct Enforced {
    pub wall_clock: How,
    pub memory: How,
    pub disk: How,
    pub processes: How,
    pub output: How,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum How {
    /// The run is killed when the limit is passed.
    Killed,
    /// Polled every [`POLL`]; the run is killed above the limit, after a short overshoot.
    PolledAndKilled,
    /// The kernel stops the run at the limit (a capped volume: "disk full").
    Hard,
    /// Each file is stopped at the limit by the kernel; the total is polled, and the run killed
    /// above it, after a short overshoot.
    PerFileHardTotalPolled,
    /// None: scripts can't start processes at all.
    NoneAllowed,
    /// Cut at the limit; the run is killed.
    CutAndKilled,
}

pub const POLL: Duration = Duration::from_millis(50);

/// The runtime a script runs with: its executable, every executable it re-execs (a framework
/// Python starts its app bundle's binary), and the folders it reads (its install prefix).
#[derive(Clone, Debug)]
pub struct Runtime {
    pub program: PathBuf,
    pub executables: Vec<PathBuf>,
    pub read_dirs: Vec<PathBuf>,
}

/// An input file, copied into `in/` under `name`.
#[derive(Clone, Debug)]
pub struct Input {
    pub name: String,
    pub path: PathBuf,
}

#[derive(Clone, Debug)]
pub struct RunSpec {
    pub runtime: Runtime,
    /// The skill's folder: readable, never writable.
    pub skill_dir: PathBuf,
    /// The runtime's arguments (for example `-I`, the script, then its arguments). The script
    /// finds its folders in `DN_RUN_IN`, `DN_RUN_OUT` and `DN_RUN_TMP`.
    pub args: Vec<String>,
    /// Extra environment for the runtime, on top of the minimal one. Never secrets.
    pub env: Vec<(String, String)>,
    pub inputs: Vec<Input>,
    pub limits: Limits,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Killed {
    WallClock,
    Memory,
    Disk,
    Output,
}

/// A regular file the run left in `out/`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Output {
    /// Relative to `out/`, with `/` between folders.
    pub name: String,
    pub bytes: Vec<u8>,
}

/// Something in `out/` that isn't collected (a link, a FIFO, a device, too deep).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skipped {
    pub name: String,
    pub reason: &'static str,
}

#[derive(Clone, Debug)]
pub struct RunResult {
    /// `None` when the process ended by a signal (a kill included).
    pub exit_code: Option<i32>,
    pub killed: Option<Killed>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub peak_memory: u64,
    pub peak_disk: u64,
    pub elapsed: Duration,
    pub outputs: Vec<Output>,
    pub skipped: Vec<Skipped>,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Scripts can't run on this computer; the reason is for the person.
    #[error("{0}")]
    Unavailable(String),
    /// This run's sandbox couldn't be applied in full, so the run didn't start.
    #[error("the sandbox couldn't be set up for this run: {0}")]
    Refused(String),
    #[error("input {name}: {reason}")]
    Input { name: String, reason: String },
    #[error("{0}")]
    Limits(String),
    /// The run's process group couldn't be confirmed gone, so its outputs aren't read.
    #[error("the run's processes couldn't be stopped")]
    NotStopped,
    #[error("{what}: {source}")]
    Io {
        what: String,
        source: std::io::Error,
    },
}

fn io(what: impl Into<String>) -> impl FnOnce(std::io::Error) -> Error {
    let what = what.into();
    move |source| Error::Io { what, source }
}

/// The sandbox on this computer: where runs live and the launcher binary.
#[derive(Debug)]
pub struct Sandbox {
    launcher: PathBuf,
    runs: PathBuf,
}

impl Sandbox {
    /// Checks this OS can sandbox runs, and makes `<data_dir>/runs` (0700).
    pub fn new(launcher: PathBuf, data_dir: &Path) -> Result<Self, Error> {
        os_check(&launcher)?;
        let runs = data_dir.join("runs");
        make_private_dir_all(&runs).map_err(io("runs folder"))?;
        let runs = std::fs::canonicalize(&runs).map_err(io("runs folder"))?;
        Ok(Self { launcher, runs })
    }

    /// How each limit is enforced here.
    pub fn enforced() -> Enforced {
        Enforced {
            wall_clock: How::Killed,
            memory: How::PolledAndKilled,
            disk: if cfg!(target_os = "macos") {
                How::Hard
            } else {
                How::PerFileHardTotalPolled
            },
            processes: How::NoneAllowed,
            output: How::CutAndKilled,
        }
    }

    pub fn runs_dir(&self) -> &Path {
        &self.runs
    }

    /// Removes what runs left behind (after a crash they hold copies of the person's files).
    /// Returns how many run folders were removed.
    pub fn sweep(&self) -> Result<usize, Error> {
        #[cfg(target_os = "macos")]
        for mp in macos::attached_under(&self.runs) {
            macos::detach(&mp).map_err(Error::Refused)?;
        }
        let mut n = 0;
        for e in std::fs::read_dir(&self.runs).map_err(io("runs folder"))? {
            let e = e.map_err(io("runs folder"))?;
            let name = e.file_name();
            if name.to_string_lossy().starts_with("run-") {
                std::fs::remove_dir_all(e.path()).map_err(io("remove a run folder"))?;
                n += 1;
            }
        }
        Ok(n)
    }

    /// Runs `spec` in a fresh sandbox and returns what it did and left in `out/`.
    pub fn run(&self, spec: &RunSpec) -> Result<RunResult, Error> {
        let l = &spec.limits;
        if l.wall.is_zero() || l.wall > MAX_WALL {
            return Err(Error::Limits(format!(
                "the time limit must be between 1 s and {} s",
                MAX_WALL.as_secs()
            )));
        }
        if l.disk == 0 || l.memory == 0 || l.output == 0 {
            return Err(Error::Limits("limits must be above zero".into()));
        }
        let folder = RunFolder::create(&self.runs, l.disk)?;
        for input in &spec.inputs {
            copy_input(input, &folder.input)?;
        }
        let launch = self.launch_spec(spec, &folder)?;
        let json = serde_json::to_string(&launch).map_err(|e| Error::Refused(e.to_string()))?;
        let start = Instant::now();
        let mut child = Command::new(&self.launcher)
            .arg(json)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(io("start the launcher"))?;
        let over = Arc::new(AtomicBool::new(false));
        let out = drain(child.stdout.take(), l.output, over.clone());
        let err = drain(child.stderr.take(), l.output, over.clone());
        let watched = watch(&mut child, &folder.work, l, &over, start);
        let stopped = stop_group(&child);
        let (stdout, stdout_truncated) = out.join().unwrap_or_default();
        let (stderr, stderr_truncated) = err.join().unwrap_or_default();
        let (status, killed, peak_memory, peak_disk) = watched?;
        if !stopped {
            return Err(Error::NotStopped);
        }
        let exit_code = status.code();
        if let Some(reason) = refusal(exit_code, &stderr) {
            return Err(Error::Refused(reason));
        }
        let (outputs, skipped) = collect(&folder.output)?;
        drop(folder);
        Ok(RunResult {
            exit_code,
            killed,
            stdout,
            stderr,
            stdout_truncated,
            stderr_truncated,
            peak_memory,
            peak_disk,
            elapsed: start.elapsed(),
            outputs,
            skipped,
        })
    }

    fn launch_spec(&self, spec: &RunSpec, f: &RunFolder) -> Result<LaunchSpec, Error> {
        let real =
            |p: &Path| std::fs::canonicalize(p).map_err(io(format!("{} isn't there", p.display())));
        let program = real(&spec.runtime.program)?;
        let mut exes = vec![program.clone()];
        for e in &spec.runtime.executables {
            exes.push(real(e)?);
        }
        let mut reads = Vec::new();
        for d in &spec.runtime.read_dirs {
            reads.push(real(d)?);
        }
        reads.push(real(&spec.skill_dir)?);
        reads.push(f.input.clone());
        let writes = vec![f.output.clone(), f.tmp.clone()];
        let s = |p: &Path| p.to_string_lossy().into_owned();
        let mut env = vec![
            ("PATH".to_owned(), "/nonexistent".to_owned()),
            ("HOME".to_owned(), s(&f.tmp)),
            ("TMPDIR".to_owned(), s(&f.tmp)),
            ("LANG".to_owned(), "C.UTF-8".to_owned()),
            ("DN_RUN_IN".to_owned(), s(&f.input)),
            ("DN_RUN_OUT".to_owned(), s(&f.output)),
            ("DN_RUN_TMP".to_owned(), s(&f.tmp)),
        ];
        env.extend(spec.env.iter().cloned());
        #[allow(unused_mut)]
        let mut launch = LaunchSpec {
            program,
            args: spec.args.clone(),
            cwd: f.tmp.clone(),
            env,
            file_size_cap: spec.limits.disk,
            profile: None,
            reads: Vec::new(),
            writes: Vec::new(),
            execs: Vec::new(),
            devices: Vec::new(),
        };
        #[cfg(target_os = "macos")]
        {
            let profile = f.root.join("profile.sb");
            std::fs::write(&profile, macos::profile(&exes, &reads, &writes))
                .map_err(io("write the profile"))?;
            launch.profile = Some(profile);
        }
        #[cfg(target_os = "linux")]
        {
            let mut all = linux::system_reads();
            all.extend(reads);
            exes.extend(linux::loaders());
            launch.reads = all;
            launch.writes = writes;
            launch.execs = exes;
            launch.devices = linux::devices();
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        let _ = (exes, reads, writes);
        Ok(launch)
    }
}

#[cfg(target_os = "macos")]
fn os_check(_launcher: &Path) -> Result<(), Error> {
    macos::check().map_err(Error::Unavailable)
}

#[cfg(target_os = "linux")]
fn os_check(launcher: &Path) -> Result<(), Error> {
    // The launcher restricts a throwaway copy of itself to find the kernel's Landlock ABI.
    let out = Command::new(launcher)
        .arg("--check")
        .env_clear()
        .output()
        .map_err(io("start the launcher"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(Error::Unavailable(
            String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        ))
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn os_check(_launcher: &Path) -> Result<(), Error> {
    Err(Error::Unavailable(
        "scripts can't run in a sandbox on this OS yet".into(),
    ))
}

/// The launcher's or `sandbox-exec`'s refusal, if that's why the run ended.
fn refusal(code: Option<i32>, stderr: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(stderr);
    let first = text.lines().next().unwrap_or_default();
    if code == Some(REFUSED_EXIT)
        && let Some(r) = first.strip_prefix(REFUSED_PREFIX)
    {
        return Some(r.to_owned());
    }
    // sandbox-exec reports a profile that didn't load on its first line and exits non-zero.
    if cfg!(target_os = "macos")
        && code.is_some_and(|c| c != 0)
        && let Some(r) = first.strip_prefix("sandbox-exec: ")
    {
        return Some(r.to_owned());
    }
    None
}

/// `runs/run-…/`: `in/`, `work/out/`, `work/tmp/`; removed (and its volume detached) on drop.
struct RunFolder {
    root: PathBuf,
    input: PathBuf,
    work: PathBuf,
    output: PathBuf,
    tmp: PathBuf,
    #[cfg(target_os = "macos")]
    attached: bool,
}

impl RunFolder {
    fn create(runs: &Path, disk: u64) -> Result<Self, Error> {
        static N: AtomicU64 = AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let root = loop {
            let n = N.fetch_add(1, Ordering::Relaxed);
            let p = runs.join(format!("run-{}-{nanos:x}-{n}", std::process::id()));
            match make_private_dir(&p) {
                Ok(()) => break p,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(io("make the run folder")(e)),
            }
        };
        #[allow(unused_mut)]
        let mut f = Self {
            input: root.join("in"),
            work: root.join("work"),
            output: root.join("work/out"),
            tmp: root.join("work/tmp"),
            root,
            #[cfg(target_os = "macos")]
            attached: false,
        };
        make_private_dir(&f.input).map_err(io("in/"))?;
        make_private_dir(&f.work).map_err(io("work/"))?;
        #[cfg(target_os = "macos")]
        {
            macos::attach(runs, disk, &f.root.join("volume.sparseimage"), &f.work)
                .map_err(Error::Refused)?;
            f.attached = true;
        }
        #[cfg(not(target_os = "macos"))]
        let _ = disk;
        make_private_dir(&f.output).map_err(io("out/"))?;
        make_private_dir(&f.tmp).map_err(io("tmp/"))?;
        Ok(f)
    }
}

impl Drop for RunFolder {
    fn drop(&mut self) {
        #[cfg(target_os = "macos")]
        if self.attached {
            // If this fails, sweep() detaches it at the next start.
            let _ = macos::detach(&self.work);
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn make_private_dir(p: &Path) -> std::io::Result<()> {
    #[allow(unused_mut)]
    let mut b = std::fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
    b.create(p)
}

fn make_private_dir_all(p: &Path) -> std::io::Result<()> {
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
    b.create(p)
}

/// A clean file name: no folders, no hidden files, no `..`.
fn clean_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && !name.starts_with('.')
        && !name.contains(['/', '\\', '\0', ':'])
}

#[cfg(unix)]
fn open_nofollow(p: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let flags = rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(flags.bits() as i32)
        .open(p)
}

#[cfg(not(unix))]
fn open_nofollow(p: &Path) -> std::io::Result<std::fs::File> {
    if std::fs::symlink_metadata(p)?.file_type().is_symlink() {
        return Err(std::io::Error::other("a link"));
    }
    std::fs::File::open(p)
}

/// Copies an input into `in/`: opened without following a link, a regular file with one
/// link only.
fn copy_input(input: &Input, dir: &Path) -> Result<(), Error> {
    let bad = |reason: &str| Error::Input {
        name: input.name.clone(),
        reason: reason.to_owned(),
    };
    if !clean_name(&input.name) {
        return Err(bad("not a plain file name"));
    }
    let mut src = open_nofollow(&input.path).map_err(|e| bad(&format!("can't be opened ({e})")))?;
    let meta = src.metadata().map_err(|e| bad(&e.to_string()))?;
    if !meta.is_file() {
        return Err(bad("not a regular file"));
    }
    #[cfg(unix)]
    if std::os::unix::fs::MetadataExt::nlink(&meta) != 1 {
        return Err(bad("a file with more than one link"));
    }
    let mut dst = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dir.join(&input.name))
        .map_err(|e| bad(&format!("can't be copied ({e})")))?;
    std::io::copy(&mut src, &mut dst).map_err(|e| bad(&format!("can't be copied ({e})")))?;
    Ok(())
}

/// Reads a stream to its end, keeping at most `cap` bytes; past that it sets `over` and
/// keeps draining, so a full pipe never stalls the run.
fn drain<R: Read + Send + 'static>(
    stream: Option<R>,
    cap: usize,
    over: Arc<AtomicBool>,
) -> JoinHandle<(Vec<u8>, bool)> {
    std::thread::spawn(move || {
        let Some(mut r) = stream else {
            return (Vec::new(), false);
        };
        let mut kept = Vec::new();
        let mut cut = false;
        let mut buf = [0u8; 64 * 1024];
        loop {
            match r.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let room = cap.saturating_sub(kept.len());
                    kept.extend_from_slice(&buf[..n.min(room)]);
                    if n > room {
                        cut = true;
                        over.store(true, Ordering::Relaxed);
                    }
                }
            }
        }
        (kept, cut)
    })
}

type Watched = (std::process::ExitStatus, Option<Killed>, u64, u64);

/// Polls the run until it ends or passes a limit, then kills it.
fn watch(
    child: &mut Child,
    work: &Path,
    l: &Limits,
    over: &AtomicBool,
    start: Instant,
) -> Result<Watched, Error> {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
    let pid = Pid::from_u32(child.id());
    let mut sys = System::new();
    let (mut peak_mem, mut peak_disk) = (0u64, 0u64);
    loop {
        if let Some(status) = child.try_wait().map_err(io("wait for the run"))? {
            return Ok((status, None, peak_mem, peak_disk));
        }
        sys.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[pid]),
            true,
            ProcessRefreshKind::nothing().with_memory(),
        );
        let mem = sys.process(pid).map(|p| p.memory()).unwrap_or(0);
        let disk = disk_use(work);
        peak_mem = peak_mem.max(mem);
        peak_disk = peak_disk.max(disk);
        let killed = if start.elapsed() > l.wall {
            Some(Killed::WallClock)
        } else if mem > l.memory {
            Some(Killed::Memory)
        } else if disk > l.disk {
            Some(Killed::Disk)
        } else if over.load(Ordering::Relaxed) {
            Some(Killed::Output)
        } else {
            None
        };
        if killed.is_some() {
            kill(child);
            let status = child.wait().map_err(io("wait for the run"))?;
            return Ok((status, killed, peak_mem, peak_disk));
        }
        std::thread::sleep(POLL);
    }
}

#[cfg(unix)]
fn kill(child: &mut Child) {
    let pid = rustix::process::Pid::from_child(child);
    let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
    let _ = child.kill();
}

#[cfg(not(unix))]
fn kill(child: &mut Child) {
    let _ = child.kill();
}

/// Kills whatever is left in the run's process group and confirms it's gone.
#[cfg(unix)]
fn stop_group(child: &Child) -> bool {
    use rustix::process::{Pid, Signal, kill_process_group, test_kill_process_group};
    let pid = Pid::from_child(child);
    for _ in 0..40 {
        match test_kill_process_group(pid) {
            Err(rustix::io::Errno::SRCH) => return true,
            _ => {
                let _ = kill_process_group(pid, Signal::KILL);
                std::thread::sleep(Duration::from_millis(25));
            }
        }
    }
    false
}

#[cfg(not(unix))]
fn stop_group(_child: &Child) -> bool {
    true
}

/// Bytes under `dir`, not following links.
fn disk_use(dir: &Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return 0;
    };
    rd.flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => disk_use(&e.path()),
            _ => e.metadata().map(|m| m.len()).unwrap_or(0),
        })
        .sum()
}

const MAX_DEPTH: usize = 8;

/// Regular files in `out/`, read without following links.
fn collect(out: &Path) -> Result<(Vec<Output>, Vec<Skipped>), Error> {
    let mut outputs = Vec::new();
    let mut skipped = Vec::new();
    walk(out, "", 0, &mut outputs, &mut skipped)?;
    outputs.sort_by(|a, b| a.name.cmp(&b.name));
    Ok((outputs, skipped))
}

fn walk(
    dir: &Path,
    prefix: &str,
    depth: usize,
    outputs: &mut Vec<Output>,
    skipped: &mut Vec<Skipped>,
) -> Result<(), Error> {
    for e in std::fs::read_dir(dir).map_err(io("read out/"))? {
        let e = e.map_err(io("read out/"))?;
        let name = format!("{prefix}{}", e.file_name().to_string_lossy());
        let t = e.file_type().map_err(io("read out/"))?;
        if t.is_dir() {
            if depth + 1 >= MAX_DEPTH {
                skipped.push(Skipped {
                    name,
                    reason: "folders too deep",
                });
            } else {
                walk(&e.path(), &format!("{name}/"), depth + 1, outputs, skipped)?;
            }
        } else if t.is_file() {
            let mut f = match open_nofollow(&e.path()) {
                Ok(f) => f,
                Err(_) => {
                    skipped.push(Skipped {
                        name,
                        reason: "couldn't be opened",
                    });
                    continue;
                }
            };
            if !f.metadata().map(|m| m.is_file()).unwrap_or(false) {
                skipped.push(Skipped {
                    name,
                    reason: "not a regular file",
                });
                continue;
            }
            let mut bytes = Vec::new();
            f.read_to_end(&mut bytes).map_err(io("read an output"))?;
            outputs.push(Output { name, bytes });
        } else {
            skipped.push(Skipped {
                name,
                reason: if t.is_symlink() {
                    "a link"
                } else {
                    "not a regular file"
                },
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_names_are_plain() {
        for ok in ["data.csv", "Report 2026.xlsx", "ü.txt"] {
            assert!(clean_name(ok), "{ok}");
        }
        for bad in ["", ".hidden", "..", "a/b", "a\\b", "c:x", "a\0b"] {
            assert!(!clean_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn a_refusal_is_read_only_from_the_launcher_or_sandbox_exec() {
        let r = format!("{REFUSED_PREFIX}Landlock wasn't enforced\nmore");
        assert_eq!(
            refusal(Some(REFUSED_EXIT), r.as_bytes()).as_deref(),
            Some("Landlock wasn't enforced")
        );
        assert_eq!(refusal(Some(1), r.as_bytes()), None, "other exit codes");
        assert_eq!(refusal(Some(REFUSED_EXIT), b"Traceback"), None);
        let sb = refusal(Some(65), b"sandbox-exec: invalid profile\n");
        assert_eq!(sb.is_some(), cfg!(target_os = "macos"));
    }

    #[test]
    fn limits_are_checked_before_anything_starts() {
        let dir = std::env::temp_dir().join(format!("dn-sandbox-limits-{}", std::process::id()));
        let sb = Sandbox {
            launcher: PathBuf::from("/nonexistent"),
            runs: dir.clone(),
        };
        let mut spec = RunSpec {
            runtime: Runtime {
                program: PathBuf::from("/bin/true"),
                executables: vec![],
                read_dirs: vec![],
            },
            skill_dir: dir.clone(),
            args: vec![],
            env: vec![],
            inputs: vec![],
            limits: Limits {
                wall: Duration::from_secs(301),
                ..Limits::default()
            },
        };
        assert!(matches!(sb.run(&spec), Err(Error::Limits(_))));
        spec.limits = Limits {
            disk: 0,
            ..Limits::default()
        };
        assert!(matches!(sb.run(&spec), Err(Error::Limits(_))));
        assert!(!dir.exists(), "nothing was made");
    }
}
