//! A descriptor the app leaks without close-on-exec (a CI runner, cargo's jobserver, a leaky
//! parent) must not reach the script: the launcher closes it, then checks none is left
//! (ADR-0021 §2). Its own test binary, so the leak can't affect other tests.
#![cfg(any(target_os = "macos", target_os = "linux"))]

use dn_sandbox::{Limits, RunSpec, Runtime, Sandbox};
use rustix::io::{FdFlags, fcntl_getfd, fcntl_setfd};
use std::path::PathBuf;

#[test]
fn an_inherited_descriptor_is_closed_before_the_script_starts() {
    let base = std::env::temp_dir().join(format!("dn-sandbox-fd-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("skill")).unwrap();
    let base = std::fs::canonicalize(&base).unwrap();
    std::fs::write(base.join("secret.txt"), "api key").unwrap();
    // A file this process opens and then leaks to every child it starts.
    let leaked = std::fs::File::open(base.join("secret.txt")).unwrap();
    fcntl_setfd(&leaked, FdFlags::empty()).unwrap();
    assert!(!fcntl_getfd(&leaked).unwrap().contains(FdFlags::CLOEXEC));

    let sandbox = Sandbox::new(
        PathBuf::from(env!("CARGO_BIN_EXE_dn-sandbox-launch")),
        &base.join("data"),
    )
    .unwrap();
    let spec = RunSpec {
        runtime: Runtime {
            program: PathBuf::from(env!("CARGO_BIN_EXE_dn-sandbox-probe")),
            executables: vec![],
            read_dirs: vec![],
        },
        skill_dir: base.join("skill"),
        args: vec!["noop".into()],
        env: vec![],
        inputs: vec![],
        limits: Limits::default(),
    };
    // Before the launcher closed inherited descriptors, this run was refused with "file
    // descriptor N would be inherited"; now it runs, and the check after the close passed.
    let r = sandbox.run(&spec).unwrap();
    assert_eq!(
        r.exit_code,
        Some(0),
        "{}",
        String::from_utf8_lossy(&r.stderr)
    );
    drop(leaked);
    let _ = std::fs::remove_dir_all(&base);
}
