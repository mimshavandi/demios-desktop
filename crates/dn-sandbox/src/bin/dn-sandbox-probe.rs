//! The self-test's probe (ADR-0021 §5): run inside the sandbox in place of a runtime, it tries
//! what a run must not do and the work a run may do, and prints one JSON line per probe. It
//! needs no Python or Node, so the self-test and the e2e tests run the same on every computer.
//!
//! `dn-sandbox-probe probes <home> <data> <skill> <port> [<abstract name>]`: every probe.
//! `dn-sandbox-probe loop | mem <MB> | flood | disk-one <MB> | disk-many <MB> | noop`: limits.
//! `dn-sandbox-probe swap-out <target>`: tries to replace `out/` and `tmp/` with links.

use std::io::Write;
use std::net::{SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::path::{Path, PathBuf};
use std::time::Duration;

fn env_path(k: &str) -> PathBuf {
    PathBuf::from(std::env::var_os(k).unwrap_or_default())
}

fn report(name: &str, r: std::io::Result<String>) {
    let line = match r {
        Ok(detail) => serde_json::json!({"probe": name, "ok": true, "detail": detail}),
        Err(e) => serde_json::json!({
            "probe": name,
            "ok": false,
            "kind": format!("{:?}", e.kind()),
            "detail": e.to_string(),
        }),
    };
    println!("{line}");
}

fn read_file(p: &Path) -> std::io::Result<String> {
    std::fs::read(p).map(|b| format!("{} bytes", b.len()))
}

fn list(p: &Path) -> std::io::Result<String> {
    std::fs::read_dir(p).map(|d| format!("{} entries", d.count()))
}

fn write_file(p: &Path) -> std::io::Result<String> {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(p)?
        .write_all(b"x")
        .map(|()| "written".into())
}

fn tcp(addr: &str) -> std::io::Result<String> {
    let a: SocketAddr = addr.parse().map_err(std::io::Error::other)?;
    TcpStream::connect_timeout(&a, Duration::from_secs(2)).map(|_| format!("connected to {a}"))
}

fn probes(args: &[String]) {
    let [home, data, skill, port, ..] = args else {
        eprintln!("usage: probes <home> <data> <skill> <port> [<abstract name>]");
        std::process::exit(2);
    };
    let (home, data, skill) = (Path::new(home), Path::new(data), Path::new(skill));
    let (input, out, tmp) = (
        env_path("DN_RUN_IN"),
        env_path("DN_RUN_OUT"),
        env_path("DN_RUN_TMP"),
    );

    // The work a run may do.
    report(
        "positive: read in/, write out/ and tmp/",
        std::fs::read(input.join("data.csv")).and_then(|d| {
            std::fs::write(out.join("result.txt"), d.to_ascii_uppercase())?;
            std::fs::write(tmp.join("scratch"), b"x")?;
            Ok("read in/, wrote out/ and tmp/".into())
        }),
    );

    // Files.
    let users = if cfg!(target_os = "macos") {
        "/Users"
    } else {
        "/home"
    };
    report(
        "read: list the people's home folders",
        list(Path::new(users)),
    );
    report("read: list the home folder", list(home));
    report(
        "read: a secret in the home folder",
        read_file(&home.join("secret.txt")),
    );
    report(
        "read: the app's data folder (a secret beside runs/)",
        read_file(&data.join("secret.txt")),
    );
    report("read: /etc/passwd", read_file(Path::new("/etc/passwd")));
    report(
        "read: list the runs folder (other runs)",
        list(&data.join("runs")),
    );
    if cfg!(target_os = "macos") {
        report(
            "read: the keychains folder",
            list(&home.join("Library/Keychains")),
        );
    }
    report(
        "write: the home folder",
        write_file(&home.join("pwned.txt")),
    );
    report(
        "write: /tmp",
        write_file(Path::new("/tmp/dn-sandbox-pwned")),
    );
    report(
        "write: in/ (inputs are read-only)",
        write_file(&input.join("data.csv")),
    );
    report(
        "write: the skill's folder",
        write_file(&skill.join("pwned")),
    );
    report("read: through a link made in out/", {
        let link = out.join("link");
        symlink(&home.join("secret.txt"), &link).and_then(|()| read_file(&link))
    });

    // Network.
    report("net: TCP to 1.1.1.1:443", tcp("1.1.1.1:443"));
    report("net: TCP to localhost", tcp(&format!("127.0.0.1:{port}")));
    report(
        "net: DNS lookup",
        ("example.com", 443)
            .to_socket_addrs()
            .map(|a| format!("{} addresses", a.count())),
    );
    report(
        "net: UDP to 8.8.8.8:53",
        UdpSocket::bind("0.0.0.0:0")
            .and_then(|s| s.send_to(b"x", "8.8.8.8:53").map(|n| format!("sent {n}"))),
    );
    let unix_sock = if cfg!(target_os = "macos") {
        "/var/run/mDNSResponder"
    } else {
        "/run/dbus/system_bus_socket"
    };
    report(
        "net: a system Unix socket",
        unix_connect(Path::new(unix_sock)),
    );
    report(
        "net: X11 socket",
        unix_connect(Path::new("/tmp/.X11-unix/X0")),
    );
    #[cfg(target_os = "linux")]
    if let Some(name) = args.get(4) {
        report("net: abstract Unix socket", abstract_connect(name));
    }

    // Processes.
    report(
        "exec: /bin/sh",
        std::process::Command::new("/bin/sh")
            .args(["-c", "echo hi"])
            .output()
            .map(|o| format!("ran: {:?}", o.status)),
    );
    report(
        "fork: start a copy of this probe",
        std::env::current_exe().and_then(|me| {
            std::process::Command::new(me)
                .arg("noop")
                .output()
                .map(|o| format!("ran: {:?}", o.status))
        }),
    );
    #[cfg(unix)]
    {
        use rustix::process::{Pid, Signal, getppid, kill_process};
        // SIGCONT is harmless if it ever got through.
        report(
            "signal: the app (the parent process)",
            getppid()
                .ok_or_else(|| std::io::Error::other("no parent"))
                .and_then(|p| kill_process(p, Signal::CONT).map_err(Into::into))
                .map(|()| "signalled".into()),
        );
        report(
            "signal: process 1",
            Pid::from_raw(1)
                .ok_or_else(|| std::io::Error::other("no pid 1"))
                .and_then(|p| kill_process(p, Signal::CONT).map_err(Into::into))
                .map(|()| "signalled".into()),
        );
        report(
            "positive: signal itself",
            kill_process(rustix::process::getpid(), Signal::CONT)
                .map_err(Into::into)
                .map(|()| "ok".into()),
        );
    }
    #[cfg(target_os = "linux")]
    report(
        "signal: a pidfd for the app",
        rustix::process::getppid()
            .ok_or_else(|| std::io::Error::other("no parent"))
            .and_then(|p| {
                rustix::process::pidfd_open(p, rustix::process::PidfdFlags::empty())
                    .map_err(Into::into)
            })
            .map(|_| "opened".into()),
    );
    report(
        "tty: open /dev/tty",
        std::fs::File::open("/dev/tty").map(|_| "opened".into()),
    );
    #[cfg(target_os = "linux")]
    {
        report(
            "read: /proc/1/environ (another process)",
            read_file(Path::new("/proc/1/environ")),
        );
        report(
            "positive: socketpair (the runtime may use it)",
            std::os::unix::net::UnixStream::pair().map(|_| "ok".into()),
        );
    }
}

#[cfg(unix)]
fn symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}
#[cfg(not(unix))]
fn symlink(_target: &Path, _link: &Path) -> std::io::Result<()> {
    Err(std::io::Error::other("no symlinks here"))
}

#[cfg(unix)]
fn unix_connect(p: &Path) -> std::io::Result<String> {
    std::os::unix::net::UnixStream::connect(p).map(|_| "connected".into())
}
#[cfg(not(unix))]
fn unix_connect(_p: &Path) -> std::io::Result<String> {
    Err(std::io::Error::other("no Unix sockets here"))
}

#[cfg(target_os = "linux")]
fn abstract_connect(name: &str) -> std::io::Result<String> {
    use std::os::linux::net::SocketAddrExt;
    let addr = std::os::unix::net::SocketAddr::from_abstract_name(name.as_bytes())?;
    std::os::unix::net::UnixStream::connect_addr(&addr).map(|_| "connected".into())
}

fn mb(args: &[String]) -> usize {
    args.first().and_then(|a| a.parse().ok()).unwrap_or(10)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rest = args.get(1..).unwrap_or_default();
    match args.first().map(String::as_str) {
        Some("probes") => probes(rest),
        Some("noop") => {}
        // Files whose length is just under the cap but take no space, then a quick exit:
        // the final disk check must still see them.
        Some("sparse") => {
            let cap: u64 = rest.first().and_then(|a| a.parse().ok()).unwrap_or(1 << 20);
            let out = env_path("DN_RUN_OUT");
            for i in 0..50 {
                let f = std::fs::File::create(out.join(format!("s{i}")));
                if let Err(e) = f.and_then(|f| f.set_len(cap - 1)) {
                    println!("stopped after {i}: {e}");
                    return;
                }
            }
            println!("made 50 sparse files");
        }
        // Replace out/ (or tmp/) with a link to `target`: the profile must refuse it, and if
        // it didn't, collecting outputs must refuse to read through it.
        Some("swap-out") => {
            let target = rest.first().cloned().unwrap_or_default();
            for (k, var) in [("out", "DN_RUN_OUT"), ("tmp", "DN_RUN_TMP")] {
                let dir = env_path(var);
                let r = std::fs::remove_dir(&dir).and_then(|()| symlink(Path::new(&target), &dir));
                report(
                    &format!("write: replace {k}/ with a link"),
                    r.map(|()| "replaced".into()),
                );
            }
        }
        Some("loop") => loop {
            std::hint::spin_loop();
        },
        Some("mem") => {
            let v = vec![1u8; mb(rest) * 1024 * 1024];
            std::thread::sleep(Duration::from_secs(30));
            println!("{}", v.len());
        }
        Some("flood") => {
            let line = [b'x'; 1024];
            let mut o = std::io::stdout().lock();
            while o.write_all(&line).is_ok() {}
        }
        Some("disk-one") => {
            let p = env_path("DN_RUN_OUT").join("big");
            let mut f = std::fs::File::create(p).expect("create");
            let chunk = vec![0u8; 1024 * 1024];
            for i in 0..mb(rest) {
                if let Err(e) = f.write_all(&chunk) {
                    println!("stopped after {i} MB: {e}");
                    return;
                }
            }
            println!("wrote {} MB", mb(rest));
        }
        Some("disk-many") => {
            let dir = env_path("DN_RUN_TMP");
            let chunk = vec![0u8; 1024 * 1024];
            for i in 0..mb(rest) {
                if let Err(e) = std::fs::write(dir.join(format!("f{i}")), &chunk) {
                    println!("stopped after {i} MB: {e}");
                    return;
                }
                // Paced, so the test sees the watchdog where there's no hard cap.
                std::thread::sleep(Duration::from_millis(2));
            }
            println!("wrote {} MB", mb(rest));
        }
        _ => {
            eprintln!("usage: dn-sandbox-probe probes|loop|mem|flood|disk-one|disk-many|noop");
            std::process::exit(2);
        }
    }
}
