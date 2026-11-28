//! End to end, per OS (ADR-0021 §5, #435): the probe binary runs in the real sandbox and
//! tries everything a run must not do; every attempt must fail, and the work a run may do
//! must succeed. Then each limit is hit once.
#![cfg(any(target_os = "macos", target_os = "linux"))]

use dn_sandbox::{Input, Killed, Limits, RunSpec, Runtime, Sandbox};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

const LAUNCHER: &str = env!("CARGO_BIN_EXE_dn-sandbox-launch");
const PROBE: &str = env!("CARGO_BIN_EXE_dn-sandbox-probe");

struct World {
    base: PathBuf,
    home: PathBuf,
    data: PathBuf,
    skill: PathBuf,
    input: PathBuf,
    sandbox: Sandbox,
}

impl World {
    fn new() -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let base = std::env::temp_dir().join(format!(
            "dn-sandbox-e2e-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let base = std::fs::canonicalize(&base).unwrap();
        let (home, data, skill) = (base.join("home"), base.join("data"), base.join("skill"));
        for d in [&home, &data, &skill] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(home.join("secret.txt"), "id_rsa").unwrap();
        std::fs::create_dir_all(home.join("Library/Keychains")).unwrap();
        std::fs::write(data.join("secret.txt"), "api key").unwrap();
        std::fs::write(skill.join("run.py"), "print('hi')").unwrap();
        let input = base.join("data.csv");
        std::fs::write(&input, "a,b\n1,2\n").unwrap();
        let sandbox = Sandbox::new(PathBuf::from(LAUNCHER), &data)
            .unwrap_or_else(|e| panic!("the sandbox must be available on this CI OS: {e}"));
        Self {
            base,
            home,
            data,
            skill,
            input,
            sandbox,
        }
    }

    fn spec(&self, args: &[&str], limits: Limits) -> RunSpec {
        RunSpec {
            runtime: Runtime {
                program: PathBuf::from(PROBE),
                executables: vec![],
                read_dirs: vec![],
            },
            skill_dir: self.skill.clone(),
            args: args.iter().map(|s| s.to_string()).collect(),
            env: vec![],
            inputs: vec![Input {
                name: "data.csv".into(),
                path: self.input.clone(),
            }],
            limits,
        }
    }

    fn no_run_left(&self) {
        let left: Vec<_> = std::fs::read_dir(self.sandbox.runs_dir())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("run-"))
            .collect();
        assert!(left.is_empty(), "run folders left: {left:?}");
    }
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

#[test]
fn every_probe_is_denied_and_the_allowed_work_succeeds() {
    let w = World::new();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port().to_string();
    #[cfg(target_os = "linux")]
    let abstract_name = format!("dn-sandbox-e2e-{}", std::process::id());
    #[cfg(target_os = "linux")]
    let _abstract = {
        use std::os::linux::net::SocketAddrExt;
        let a =
            std::os::unix::net::SocketAddr::from_abstract_name(abstract_name.as_bytes()).unwrap();
        std::os::unix::net::UnixListener::bind_addr(&a).unwrap()
    };
    #[allow(unused_mut)]
    let mut args = vec!["probes", s(&w.home), s(&w.data), s(&w.skill), &port];
    #[cfg(target_os = "linux")]
    args.push(&abstract_name);
    let r = w.sandbox.run(&w.spec(&args, Limits::default())).unwrap();
    let stdout = String::from_utf8_lossy(&r.stdout);
    assert_eq!(
        r.exit_code,
        Some(0),
        "{stdout}\n{}",
        String::from_utf8_lossy(&r.stderr)
    );
    let probes: Vec<Value> = stdout
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert!(probes.len() >= 20, "{stdout}");
    let mut failures = Vec::new();
    for p in &probes {
        let name = p["probe"].as_str().unwrap();
        let ok = p["ok"].as_bool().unwrap();
        let kind = p["kind"].as_str().unwrap_or_default();
        if name.starts_with("positive:") {
            if !ok {
                failures.push(format!("{name} failed: {p}"));
            }
            continue;
        }
        if ok {
            failures.push(format!("{name} was ALLOWED: {p}"));
            continue;
        }
        // Files and sockets must be refused by the sandbox, not missing; the rest (DNS,
        // processes, the terminal) may fail any way.
        let strict = name.starts_with("read:")
            || name.starts_with("write:")
            || (name.starts_with("net: ") && !name.contains("DNS") && !name.contains("X11"));
        if strict && kind != "PermissionDenied" {
            failures.push(format!("{name}: expected permission denied, got {p}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    let names: Vec<&str> = r.outputs.iter().map(|o| o.name.as_str()).collect();
    assert_eq!(
        names,
        ["result.txt"],
        "the link made in out/ isn't collected"
    );
    assert_eq!(r.outputs[0].bytes, b"A,B\n1,2\n");
    assert_eq!(r.skipped.len(), 1, "{:?}", r.skipped);
    assert_eq!(r.skipped[0].reason, "a link");
    assert!(!w.home.join("pwned.txt").exists());
    assert!(!Path::new("/tmp/dn-sandbox-pwned").exists());
    assert_eq!(std::fs::read_to_string(&w.input).unwrap(), "a,b\n1,2\n");
    w.no_run_left();
}

/// Room on the wall clock: macOS checks a freshly built binary on its first start, which can
/// take seconds.
fn small() -> Limits {
    Limits {
        wall: Duration::from_secs(30),
        memory: 64 * 1024 * 1024,
        disk: 20 * 1024 * 1024,
        output: 64 * 1024,
    }
}

#[test]
fn the_wall_clock_kills_a_loop() {
    let w = World::new();
    let limits = Limits {
        wall: Duration::from_secs(2),
        ..small()
    };
    let r = w.sandbox.run(&w.spec(&["loop"], limits)).unwrap();
    assert_eq!(r.killed, Some(Killed::WallClock));
    assert!(r.elapsed < Duration::from_secs(5), "{:?}", r.elapsed);
    w.no_run_left();
}

#[test]
fn memory_over_the_limit_is_killed() {
    let w = World::new();
    let r = w.sandbox.run(&w.spec(&["mem", "400"], small())).unwrap();
    assert_eq!(r.killed, Some(Killed::Memory), "{r:?}");
}

#[test]
fn an_output_flood_is_cut_and_killed() {
    let w = World::new();
    let r = w.sandbox.run(&w.spec(&["flood"], small())).unwrap();
    assert_eq!(r.killed, Some(Killed::Output), "{r:?}");
    assert!(r.stdout_truncated);
    assert_eq!(r.stdout.len(), 64 * 1024);
}

#[test]
fn one_big_file_stops_at_the_disk_cap() {
    let w = World::new();
    let r = w
        .sandbox
        .run(&w.spec(&["disk-one", "60"], small()))
        .unwrap();
    let total: usize = r.outputs.iter().map(|o| o.bytes.len()).sum();
    assert!(total <= 20 * 1024 * 1024, "{total} bytes got out");
    assert!(
        r.killed.is_some()
            || r.exit_code != Some(0)
            || String::from_utf8_lossy(&r.stdout).contains("stopped"),
        "{r:?}"
    );
}

#[test]
fn many_files_stop_at_the_disk_cap() {
    let w = World::new();
    let r = w
        .sandbox
        .run(&w.spec(&["disk-many", "200"], small()))
        .unwrap();
    let stdout = String::from_utf8_lossy(&r.stdout);
    if cfg!(target_os = "macos") {
        // A hard cap: the volume is full before the watchdog has to step in.
        assert!(r.peak_disk <= 20 * 1024 * 1024, "{}", r.peak_disk);
        assert!(
            stdout.contains("stopped") || r.killed == Some(Killed::Disk),
            "{r:?}"
        );
    } else {
        assert_eq!(r.killed, Some(Killed::Disk), "{stdout}");
    }
}

#[test]
fn inputs_must_be_plain_regular_files() {
    let w = World::new();
    let mut spec = w.spec(&["noop"], small());
    spec.inputs[0].name = "../escape.csv".into();
    assert!(matches!(
        w.sandbox.run(&spec),
        Err(dn_sandbox::Error::Input { .. })
    ));
    let link = w.base.join("link.csv");
    std::os::unix::fs::symlink(&w.input, &link).unwrap();
    let mut spec = w.spec(&["noop"], small());
    spec.inputs[0].path = link;
    assert!(matches!(
        w.sandbox.run(&spec),
        Err(dn_sandbox::Error::Input { .. })
    ));
    let hard = w.base.join("hard.csv");
    std::fs::hard_link(&w.input, &hard).unwrap();
    let mut spec = w.spec(&["noop"], small());
    spec.inputs[0].path = hard;
    assert!(matches!(
        w.sandbox.run(&spec),
        Err(dn_sandbox::Error::Input { .. })
    ));
    w.no_run_left();
}

#[test]
fn sweep_removes_what_a_crash_left() {
    let w = World::new();
    std::fs::create_dir_all(w.sandbox.runs_dir().join("run-crashed/in")).unwrap();
    std::fs::write(w.sandbox.runs_dir().join("run-crashed/in/copy.csv"), "x").unwrap();
    assert_eq!(w.sandbox.sweep().unwrap(), 1);
    w.no_run_left();
}

/// A real runtime: the computer's Python, run with `-I`. Not on CI (the managed runtimes
/// aren't there yet): `cargo test -p dn-sandbox -- --ignored`.
#[test]
#[ignore]
fn a_python_script_runs_and_is_confined() {
    let w = World::new();
    let info = std::process::Command::new("python3")
        .args([
            "-I",
            "-c",
            "import sys,os;print(os.path.realpath(sys.executable));print(sys.base_prefix)",
        ])
        .output()
        .expect("python3");
    let text = String::from_utf8(info.stdout).unwrap();
    let mut lines = text.lines();
    let exe = PathBuf::from(lines.next().unwrap());
    let prefix = PathBuf::from(lines.next().unwrap());
    let app = prefix.join("Resources/Python.app/Contents/MacOS/Python");
    let script = w.skill.join("job.py");
    std::fs::write(
        &script,
        "import os, sys, csv, socket\n\
         rows = list(csv.reader(open(os.path.join(os.environ['DN_RUN_IN'], 'data.csv'))))\n\
         open(os.path.join(os.environ['DN_RUN_OUT'], 'n.txt'), 'w').write(str(len(rows)))\n\
         try:\n    open(sys.argv[1]).read(); print('LEAK')\n\
         except OSError as e: print('denied', type(e).__name__)\n\
         try:\n    socket.create_connection(('1.1.1.1', 443), timeout=2); print('LEAK')\n\
         except OSError as e: print('denied', type(e).__name__)\n",
    )
    .unwrap();
    let secret = w.data.join("secret.txt");
    let mut spec = w.spec(&["-I", s(&script), s(&secret)], Limits::default());
    spec.runtime = Runtime {
        program: exe,
        executables: if app.exists() { vec![app] } else { vec![] },
        read_dirs: vec![prefix],
    };
    let r = w.sandbox.run(&spec).unwrap();
    let out = String::from_utf8_lossy(&r.stdout);
    assert_eq!(
        r.exit_code,
        Some(0),
        "{out}\n{}",
        String::from_utf8_lossy(&r.stderr)
    );
    assert!(!out.contains("LEAK"), "{out}");
    assert_eq!(out.matches("denied").count(), 2, "{out}");
    assert_eq!(r.outputs[0].bytes, b"2");
}
