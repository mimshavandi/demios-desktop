//! The launcher (ADR-0021 §2): applies a run's sandbox to itself, verifies it, then `exec`s
//! the runtime. It's started by dn-sandbox with one argument, the run's JSON
//! [`LaunchSpec`]; anything it can't apply in full refuses the run (exit 125, a reason on
//! stderr). `--check` (Linux) reports whether this kernel can sandbox runs.

use dn_sandbox::spec::{LaunchSpec, REFUSED_EXIT, REFUSED_PREFIX};

fn refuse(reason: impl std::fmt::Display) -> ! {
    eprintln!("{REFUSED_PREFIX}{reason}");
    std::process::exit(REFUSED_EXIT)
}

fn main() {
    #[cfg(unix)]
    if let Err(e) = close_inherited_fds() {
        refuse(format!("closing inherited file descriptors: {e}"));
    }
    let arg = std::env::args().nth(1).unwrap_or_default();
    if arg == "--check" {
        check();
    }
    let spec: LaunchSpec = match serde_json::from_str(&arg) {
        Ok(s) => s,
        Err(e) => refuse(format!("the run's description couldn't be read: {e}")),
    };
    run(spec)
}

#[cfg(target_os = "linux")]
fn check() -> ! {
    match dn_sandbox::linux::check() {
        Ok(abi) => {
            println!("landlock abi {abi}");
            std::process::exit(0)
        }
        Err(reason) => {
            eprintln!("{reason}");
            std::process::exit(1)
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn check() -> ! {
    std::process::exit(0)
}

/// The descriptors above stderr that are open in this process, other than the listing's own.
#[cfg(unix)]
fn open_fds() -> Result<Vec<i32>, String> {
    use rustix::fs::{Dir, Mode, OFlags, open};
    use std::os::fd::AsRawFd;
    let list = if cfg!(target_os = "linux") {
        "/proc/self/fd"
    } else {
        "/dev/fd"
    };
    let fd = open(
        list,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|e| format!("{list}: {e}"))?;
    let own = fd.as_raw_fd();
    let mut found = Vec::new();
    // The listing's descriptor is closed when `dir` is dropped, at the end of this function.
    let dir = Dir::new(fd).map_err(|e| format!("{list}: {e}"))?;
    for entry in dir {
        let entry = entry.map_err(|e| format!("{list}: {e}"))?;
        if let Ok(n) = entry.file_name().to_string_lossy().parse::<i32>()
            && n > 2
            && n != own
        {
            found.push(n);
        }
    }
    Ok(found)
}

/// Closes every descriptor above stderr this process inherited without close-on-exec (a
/// leaky parent, a CI runner, cargo's jobserver), so none reaches the runtime (ADR-0021 §2).
/// It runs first in `main`, before this process opens anything of its own.
#[cfg(unix)]
#[allow(unsafe_code)]
fn close_inherited_fds() -> Result<(), String> {
    for n in open_fds()? {
        // SAFETY: closing a raw descriptor is only unsound if something in this process
        // still owns it (an `OwnedFd`, `File` or the like would later use or close a reused
        // number). Nothing does: this runs first in `main`, std opens no descriptor before
        // `main`, and `open_fds` has already closed its own listing. Every number here was
        // inherited across `exec` and is unknown to this program. An already-closed number
        // only gives EBADF, which `close` ignores.
        unsafe { rustix::io::close(n) };
    }
    Ok(())
}

/// Every descriptor above stderr must be closed by now; one still open would reach the
/// runtime, so the run is refused instead (the check after [`close_inherited_fds`]).
#[cfg(unix)]
fn no_inherited_fds() -> Result<(), String> {
    match open_fds()?.first() {
        Some(n) => Err(format!("file descriptor {n} would be inherited")),
        None => Ok(()),
    }
}

#[cfg(unix)]
fn run(spec: LaunchSpec) -> ! {
    use rustix::process::{Resource, Rlimit, setrlimit, setsid};
    use std::os::unix::process::CommandExt;
    use std::process::Command;
    // A new session: no controlling terminal (no TIOCSTI), and its own process group, which the
    // watchdog kills as a whole.
    if let Err(e) = setsid() {
        refuse(format!("new session: {e}"));
    }
    let cap = Rlimit {
        current: Some(spec.file_size_cap),
        maximum: Some(spec.file_size_cap),
    };
    if let Err(e) = setrlimit(Resource::Fsize, cap) {
        refuse(format!("file size limit: {e}"));
    }
    let none = Rlimit {
        current: Some(0),
        maximum: Some(0),
    };
    if let Err(e) = setrlimit(Resource::Core, none) {
        refuse(format!("core dump limit: {e}"));
    }
    if let Err(e) = no_inherited_fds() {
        refuse(e);
    }
    #[cfg(target_os = "linux")]
    let mut cmd = {
        // Files first, then syscalls: seccomp is last because it denies what setting up
        // Landlock doesn't need either way.
        if let Err(e) = dn_sandbox::linux::landlock(&spec) {
            refuse(e);
        }
        if let Err(e) = dn_sandbox::linux::seccomp(std::process::id()) {
            refuse(e);
        }
        let mut c = Command::new(&spec.program);
        c.args(&spec.args);
        c
    };
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let Some(profile) = &spec.profile else {
            refuse("no Seatbelt profile");
        };
        let mut c = Command::new("/usr/bin/sandbox-exec");
        c.arg("-f").arg(profile).arg(&spec.program).args(&spec.args);
        c
    };
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let mut cmd: Command = refuse("this OS has no sandbox yet");
    cmd.env_clear()
        .envs(spec.env.iter().map(|(k, v)| (k, v)))
        .current_dir(&spec.cwd);
    let e = cmd.exec();
    refuse(format!("start {}: {e}", spec.program.display()))
}

#[cfg(not(unix))]
fn run(_spec: LaunchSpec) -> ! {
    refuse("this OS has no sandbox yet")
}
