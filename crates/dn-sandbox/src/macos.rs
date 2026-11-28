//! macOS (ADR-0021 §2, #431): a deny-by-default Seatbelt profile loaded by
//! `/usr/bin/sandbox-exec`, and `out/` and `tmp/` on a size-capped disk image per run, so the
//! disk cap is hard (the kernel says "disk full"), not only polled.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

pub const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";
const HDIUTIL: &str = "/usr/bin/hdiutil";

/// What a runtime reads on macOS: system libraries and frameworks, the dyld shared cache,
/// time zones, ICU data and fonts. Nothing from /Users, /private/etc or /private/var
/// beyond these.
pub const SYSTEM_READS: &[&str] = &[
    "/System/Library",
    "/System/Volumes/Preboot/Cryptexes/OS",
    "/usr/lib",
    "/usr/share/icu",
    "/usr/share/zoneinfo",
    "/private/var/db/dyld",
    "/private/var/db/timezone",
    "/private/etc/localtime",
    "/Library/Fonts",
];

/// Why the sandbox can't run here, or `Ok`.
pub fn check() -> Result<(), String> {
    if !Path::new(SANDBOX_EXEC).exists() {
        return Err(
            "macOS's sandbox-exec isn't on this version of macOS, so scripts can't run \
             in a sandbox here"
                .into(),
        );
    }
    if !Path::new(HDIUTIL).exists() {
        return Err("macOS's hdiutil is missing, so a run's disk space can't be capped".into());
    }
    Ok(())
}

fn quote(p: &Path) -> String {
    let s = p.to_string_lossy();
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn subpaths<'a>(ps: impl IntoIterator<Item = &'a Path>) -> String {
    ps.into_iter()
        .map(|p| format!("(subpath {})", quote(p)))
        .collect::<Vec<_>>()
        .join(" ")
}

fn literals<'a>(ps: impl IntoIterator<Item = &'a Path>) -> String {
    ps.into_iter()
        .map(|p| format!("(literal {})", quote(p)))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The profile for one run. `exes` are the runtime's real executables (a framework Python
/// re-execs its app bundle's binary); `reads` are the runtime's folders, the skill and `in/`;
/// `writes` are `out/` and `tmp/`. Fork stays denied: a child that calls `setsid` would leave
/// the run's process group and outlive the run (#431).
pub fn profile(exes: &[PathBuf], reads: &[PathBuf], writes: &[PathBuf]) -> String {
    let system: Vec<&Path> = SYSTEM_READS.iter().map(Path::new).collect();
    let reads_all = system.into_iter().chain(reads.iter().map(PathBuf::as_path));
    let exes_l = literals(exes.iter().map(PathBuf::as_path));
    [
        "(version 1)".to_owned(),
        "(deny default)".to_owned(),
        format!("(allow file-read* {} {exes_l})", subpaths(reads_all)),
        // Path resolution needs metadata of the folders on the way (no listing, no content).
        "(allow file-read-metadata)".to_owned(),
        // The root folder itself: runtimes read it at start and abort without it.
        "(allow file-read* (literal \"/\"))".to_owned(),
        "(allow file-read-data (literal \"/dev/null\") (literal \"/dev/urandom\") (literal \"/dev/random\"))"
            .to_owned(),
        format!(
            "(allow file-write* {})",
            subpaths(writes.iter().map(PathBuf::as_path))
        ),
        // `subpath` matches the folder itself too: without this, a script could remove out/
        // and put a link to the home folder in its place (the later rule wins).
        format!(
            "(deny file-write* {})",
            literals(writes.iter().map(PathBuf::as_path))
        ),
        "(allow file-write-data (literal \"/dev/null\"))".to_owned(),
        format!("(allow process-exec {exes_l})"),
        "(allow sysctl-read)".to_owned(),
        "(allow process-info-pidinfo (target self))".to_owned(),
        "(allow signal (target self))".to_owned(),
        // Not allowed, so denied: process-fork, network*, mach-lookup (no Mach service at all:
        // pasteboard, mDNSResponder, LaunchServices), iokit, ipc.
    ]
    .join("\n")
        + "\n"
}

fn hdiutil(args: &[&str]) -> Result<(), String> {
    let out = Command::new(HDIUTIL)
        .args(args)
        .output()
        .map_err(|e| format!("hdiutil: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "hdiutil {}: {}",
            args.first().copied().unwrap_or_default(),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// An empty APFS sparse image of `bytes`, made once per size and cloned per run (a clone is
/// instant; making one takes about a second).
fn template(runs: &Path, bytes: u64) -> Result<PathBuf, String> {
    let t = runs.join(format!(".volume-{bytes}.sparseimage"));
    if !t.exists() {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let tmp = runs.join(format!(".volume-{bytes}-{}-{n}", std::process::id()));
        let size = format!("{}k", bytes.div_ceil(1024));
        hdiutil(&[
            "create",
            "-quiet",
            "-size",
            &size,
            "-fs",
            "APFS",
            "-type",
            "SPARSE",
            "-volname",
            "run",
            "-o",
            &tmp.to_string_lossy(),
        ])?;
        let made = tmp.with_extension("sparseimage");
        std::fs::rename(&made, &t).map_err(|e| format!("volume template: {e}"))?;
    }
    Ok(t)
}

/// Attaches a fresh capped volume at `mountpoint` (an empty folder), from an image kept in
/// `image`.
pub fn attach(runs: &Path, bytes: u64, image: &Path, mountpoint: &Path) -> Result<(), String> {
    let t = template(runs, bytes)?;
    // std::fs::copy clones on APFS.
    std::fs::copy(&t, image).map_err(|e| format!("volume image: {e}"))?;
    hdiutil(&[
        "attach",
        "-quiet",
        "-nobrowse",
        "-noautoopen",
        "-owners",
        "on",
        "-mountpoint",
        &mountpoint.to_string_lossy(),
        &image.to_string_lossy(),
    ])
}

pub fn detach(mountpoint: &Path) -> Result<(), String> {
    hdiutil(&["detach", "-quiet", &mountpoint.to_string_lossy()])
        .or_else(|_| hdiutil(&["detach", "-quiet", "-force", &mountpoint.to_string_lossy()]))
}

/// Images still attached from under `runs` (after a crash): their mount points.
pub fn attached_under(runs: &Path) -> Vec<PathBuf> {
    let Ok(out) = Command::new(HDIUTIL).arg("info").output() else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut found = Vec::new();
    let mut ours = false;
    for line in text.lines() {
        if let Some(path) = line
            .strip_prefix("image-path")
            .and_then(|l| l.split_once(':'))
        {
            ours = Path::new(path.1.trim()).starts_with(runs);
        } else if ours {
            // "/dev/disk12s1\t<GUID>\t/path/to/mount"
            if let Some(mp) = line
                .split('\t')
                .nth(2)
                .map(str::trim)
                .filter(|m| !m.is_empty())
            {
                found.push(PathBuf::from(mp));
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_profile_denies_by_default_and_quotes_paths() {
        let p = profile(
            &[PathBuf::from("/opt/rt/bin/python3")],
            &[
                PathBuf::from("/data/runs/run-1/in"),
                PathBuf::from("/a \"b\""),
            ],
            &[PathBuf::from("/data/runs/run-1/work/out")],
        );
        assert!(p.starts_with("(version 1)\n(deny default)\n"));
        assert!(p.contains("(subpath \"/a \\\"b\\\"\")"));
        assert!(p.contains("(allow process-exec (literal \"/opt/rt/bin/python3\"))"));
        assert!(p.contains("(allow file-write* (subpath \"/data/runs/run-1/work/out\"))"));
        assert!(p.contains("(deny file-write* (literal \"/data/runs/run-1/work/out\"))"));
        for never in ["process-fork", "network", "mach-lookup"] {
            assert!(!p.contains(never), "{never} must stay denied");
        }
    }
}
