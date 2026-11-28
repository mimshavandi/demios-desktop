//! Linux (ADR-0021 §2): Landlock for files, seccomp for sockets, processes and the rest.
//! Applied by the launcher to itself, right before it `exec`s the runtime.

use crate::spec::LaunchSpec;
use landlock::{
    ABI, Access, AccessFs, CompatLevel, Compatible, LandlockStatus, PathBeneath, PathFd, Ruleset,
    RulesetAttr, RulesetCreatedAttr, RulesetStatus, Scope,
};
#[cfg(target_arch = "x86_64")]
use seccompiler::sock_filter;
use seccompiler::{
    BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter,
    SeccompRule, TargetArch,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// What a runtime reads on every distribution we support (missing entries are skipped):
/// libraries, the loader cache, the time zone, certificates, fonts, and this process's own
/// `/proc` entry. No execute right: only [`loaders`] and the runtime get that.
pub fn system_reads() -> Vec<PathBuf> {
    [
        "/usr/lib",
        "/usr/lib64",
        "/usr/share/zoneinfo",
        "/usr/share/fonts",
        "/usr/share/fontconfig",
        "/lib",
        "/lib64",
        "/etc/ld.so.cache",
        "/etc/localtime",
        "/etc/ssl/certs",
        "/etc/fonts",
        "/dev/urandom",
        "/proc/self",
    ]
    .iter()
    .map(PathBuf::from)
    .filter(|p| p.exists())
    .collect()
}

/// The dynamic loaders a runtime is started through (needs the execute right).
pub fn loaders() -> Vec<PathBuf> {
    ["/lib64/ld-linux-x86-64.so.2", "/lib/ld-linux-aarch64.so.1"]
        .iter()
        .filter_map(|p| std::fs::canonicalize(p).ok())
        .collect()
}

pub fn devices() -> Vec<PathBuf> {
    vec![PathBuf::from("/dev/null")]
}

/// The kernel's Landlock ABI when it's 3 or later, or why the sandbox can't be set up on this
/// computer, in the person's words. Run by the launcher in a throwaway process: it restricts
/// itself to find out, then exits.
pub fn check() -> Result<u32, String> {
    let kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|s| s.trim().to_owned())
        .unwrap_or_else(|_| "unknown".into());
    let status = Ruleset::default()
        .set_compatibility(CompatLevel::BestEffort)
        .handle_access(AccessFs::from_all(ABI::V1))
        .and_then(|r| r.create())
        .and_then(|r| r.restrict_self())
        .map_err(|e| format!("Landlock couldn't be checked: {e}"))?;
    match status.landlock {
        LandlockStatus::NotEnabled => Err(format!(
            "Linux's Landlock is off: it isn't in the kernel's lsm= list (kernel {kernel})"
        )),
        LandlockStatus::NotImplemented => Err(format!(
            "this Linux kernel ({kernel}) is built without Landlock"
        )),
        LandlockStatus::Available {
            effective_abi,
            kernel_abi,
        } => {
            let abi = kernel_abi.unwrap_or(effective_abi as i32);
            if abi < 3 {
                Err(format!(
                    "Landlock ABI 3 is needed; this kernel ({kernel}) has ABI {abi}"
                ))
            } else {
                Ok(abi as u32)
            }
        }
    }
}

fn err(what: &str) -> impl Fn(landlock::RulesetError) -> String + '_ {
    move |e| format!("Landlock: {what}: {e}")
}

/// Files: deny by default; the reads, writes, executables and devices in `spec`. A hard
/// requirement: anything below ABI 3 fails, and so does a ruleset that isn't enforced.
pub fn landlock(spec: &LaunchSpec) -> Result<(), String> {
    let read = AccessFs::ReadFile | AccessFs::ReadDir;
    let exec = AccessFs::Execute | AccessFs::ReadFile;
    let write = read
        | AccessFs::WriteFile
        | AccessFs::RemoveDir
        | AccessFs::RemoveFile
        | AccessFs::MakeDir
        | AccessFs::MakeReg
        | AccessFs::MakeSym
        | AccessFs::Truncate;
    let device = AccessFs::ReadFile | AccessFs::WriteFile;
    let ruleset = Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
        .handle_access(AccessFs::from_all(ABI::V3))
        .map_err(err("ABI 3 is required"))?
        // Newer rights (device ioctls) and scopes (abstract Unix sockets, signals) where the
        // kernel has them; seccomp already denies every socket but `socketpair`.
        .set_compatibility(CompatLevel::BestEffort)
        .handle_access(AccessFs::from_all(ABI::V5))
        .map_err(err("ioctl rights"))?
        .scope(Scope::from_all(ABI::V6))
        .map_err(err("scopes"))?;
    let mut created = ruleset.create().map_err(err("create"))?;
    let rules = spec
        .reads
        .iter()
        .map(|p| (p, read))
        .chain(spec.writes.iter().map(|p| (p, write)))
        .chain(spec.execs.iter().map(|p| (p, exec)))
        .chain(spec.devices.iter().map(|p| (p, device)));
    for (path, access) in rules {
        let access = if Path::new(path).is_dir() {
            access
        } else {
            access & AccessFs::from_file(ABI::V3)
        };
        let fd = PathFd::new(path).map_err(|e| format!("Landlock: {}: {e}", path.display()))?;
        created = created
            .add_rule(PathBeneath::new(fd, access))
            .map_err(err("add a rule"))?;
    }
    let status = created.restrict_self().map_err(err("restrict"))?;
    if status.ruleset == RulesetStatus::NotEnforced || !status.no_new_privs {
        return Err(format!("Landlock wasn't enforced: {status:?}"));
    }
    Ok(())
}

// Syscall numbers (include/uapi/asm-generic/unistd.h for aarch64, arch/x86/entry/syscalls
// for x86_64).
#[cfg(target_arch = "x86_64")]
mod nr {
    pub const SOCKET: i64 = 41;
    pub const PTRACE: i64 = 101;
    pub const UNSHARE: i64 = 272;
    pub const SETNS: i64 = 308;
    pub const PROCESS_VM_READV: i64 = 310;
    pub const PROCESS_VM_WRITEV: i64 = 311;
    pub const ADD_KEY: i64 = 248;
    pub const REQUEST_KEY: i64 = 249;
    pub const KEYCTL: i64 = 250;
    pub const PERF_EVENT_OPEN: i64 = 298;
    pub const BPF: i64 = 321;
    pub const USERFAULTFD: i64 = 323;
    pub const IOCTL: i64 = 16;
    pub const CLONE: i64 = 56;
    pub const FORK: Option<i64> = Some(57);
    pub const VFORK: Option<i64> = Some(58);
}
#[cfg(target_arch = "aarch64")]
mod nr {
    pub const SOCKET: i64 = 198;
    pub const PTRACE: i64 = 117;
    pub const UNSHARE: i64 = 97;
    pub const SETNS: i64 = 268;
    pub const PROCESS_VM_READV: i64 = 270;
    pub const PROCESS_VM_WRITEV: i64 = 271;
    pub const ADD_KEY: i64 = 217;
    pub const REQUEST_KEY: i64 = 218;
    pub const KEYCTL: i64 = 219;
    pub const PERF_EVENT_OPEN: i64 = 241;
    pub const BPF: i64 = 280;
    pub const USERFAULTFD: i64 = 282;
    pub const IOCTL: i64 = 29;
    pub const CLONE: i64 = 220;
    pub const FORK: Option<i64> = None;
    pub const VFORK: Option<i64> = None;
}
const CLONE3: i64 = 435;
const IO_URING: [i64; 3] = [425, 426, 427];

const EPERM: u32 = 1;
const ENOSYS: u32 = 38;
const TIOCSTI: u64 = 0x5412;
const TIOCLINUX: u64 = 0x541C;
const CLONE_THREAD: u64 = 0x0001_0000;
/// CLONE_NEWNS, NEWCGROUP, NEWUTS, NEWIPC, NEWUSER, NEWPID, NEWNET, NEWTIME.
const CLONE_NEW: [u64; 8] = [
    0x0002_0000,
    0x0200_0000,
    0x0400_0000,
    0x0800_0000,
    0x1000_0000,
    0x2000_0000,
    0x4000_0000,
    0x0000_0080,
];

fn cond(arg: u8, len: SeccompCmpArgLen, op: SeccompCmpOp, v: u64) -> Result<SeccompRule, String> {
    SeccompCondition::new(arg, len, op, v)
        .and_then(|c| SeccompRule::new(vec![c]))
        .map_err(|e| format!("seccomp: {e}"))
}

fn install(rules: BTreeMap<i64, Vec<SeccompRule>>, action: SeccompAction) -> Result<(), String> {
    let arch: TargetArch = std::env::consts::ARCH
        .try_into()
        .map_err(|e| format!("seccomp: {e:?}"))?;
    let filter = SeccompFilter::new(rules, SeccompAction::Allow, action, arch)
        .map_err(|e| format!("seccomp: {e}"))?;
    let prog: BpfProgram = filter.try_into().map_err(|e| format!("seccomp: {e:?}"))?;
    seccompiler::apply_filter(&prog).map_err(|e| format!("seccomp: {e}"))
}

/// x86_64's x32 syscalls carry the same audit arch with bit 30 set in the number, so rules
/// keyed by number would miss them: kill any such call.
#[cfg(target_arch = "x86_64")]
fn deny_x32() -> Result<(), String> {
    const LD_W_ABS: u16 = 0x20;
    const JGE_K: u16 = 0x35;
    const RET_K: u16 = 0x06;
    let prog: BpfProgram = vec![
        sock_filter {
            code: LD_W_ABS,
            jt: 0,
            jf: 0,
            k: 0,
        },
        sock_filter {
            code: JGE_K,
            jt: 0,
            jf: 1,
            k: 0x4000_0000,
        },
        sock_filter {
            code: RET_K,
            jt: 0,
            jf: 0,
            k: 0x8000_0000, // SECCOMP_RET_KILL_PROCESS
        },
        sock_filter {
            code: RET_K,
            jt: 0,
            jf: 0,
            k: 0x7fff_0000, // SECCOMP_RET_ALLOW
        },
    ];
    seccompiler::apply_filter(&prog).map_err(|e| format!("seccomp (x32): {e}"))
}
#[cfg(not(target_arch = "x86_64"))]
fn deny_x32() -> Result<(), String> {
    Ok(())
}

/// Sockets (all but `socketpair`), new processes (threads only), ptrace, namespaces, keys,
/// BPF, io_uring (it could open sockets past these rules) and terminal injection.
pub fn seccomp() -> Result<(), String> {
    use SeccompCmpArgLen::{Dword, Qword};
    use SeccompCmpOp::{Eq, MaskedEq};
    deny_x32()?;
    // clone3 passes its flags behind a pointer, where a filter can't read them: ENOSYS makes
    // libc fall back to clone, which the rules below check.
    install(
        BTreeMap::from([(CLONE3, vec![])]),
        SeccompAction::Errno(ENOSYS),
    )?;
    let mut rules: BTreeMap<i64, Vec<SeccompRule>> = [
        nr::SOCKET,
        nr::PTRACE,
        nr::UNSHARE,
        nr::SETNS,
        nr::PROCESS_VM_READV,
        nr::PROCESS_VM_WRITEV,
        nr::ADD_KEY,
        nr::REQUEST_KEY,
        nr::KEYCTL,
        nr::PERF_EVENT_OPEN,
        nr::BPF,
        nr::USERFAULTFD,
    ]
    .into_iter()
    .chain(IO_URING)
    .chain(nr::FORK)
    .chain(nr::VFORK)
    .map(|n| (n, vec![]))
    .collect();
    rules.insert(
        nr::IOCTL,
        vec![cond(1, Dword, Eq, TIOCSTI)?, cond(1, Dword, Eq, TIOCLINUX)?],
    );
    // No new processes: a clone without CLONE_THREAD, or with any CLONE_NEW* flag.
    let mut clone = vec![cond(0, Qword, MaskedEq(CLONE_THREAD), 0)?];
    for f in CLONE_NEW {
        clone.push(cond(0, Qword, MaskedEq(f), f)?);
    }
    rules.insert(nr::CLONE, clone);
    install(rules, SeccompAction::Errno(EPERM))
}
