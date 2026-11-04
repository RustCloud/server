//! The layers themselves, on Linux.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::os::unix::process::CommandExt;
use std::path::Path;

use landlock::{Access, AccessFs, PathBeneath, PathFd, Ruleset, RulesetAttr, RulesetCreatedAttr, RulesetStatus, ABI};
use seccompiler::{
    BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter, SeccompRule, TargetArch,
};

use crate::Layers;

/// Confines this process, which is then to become `tool`, or, without one,
/// stays itself with no file system at all. `cpu_seconds: None`: no limit on
/// processor time (a long-lived worker the server times itself). Fails if a
/// layer that must be there can't be set; Landlock is used where the kernel
/// has it.
pub(crate) fn confine(memory: u64, cpu_seconds: Option<u64>, tool: Option<&Path>) -> Result<Layers, String> {
    // A process group of its own, so the server can stop it with all it started.
    // SAFETY: plain system calls without pointers to Rust memory, or with
    // pointers to locals that outlive them.
    unsafe { libc::setsid() };

    let mut limits = vec![
        (libc::RLIMIT_AS, memory, memory),
        (libc::RLIMIT_FSIZE, 0, 0),
        (libc::RLIMIT_CORE, 0, 0),
        (libc::RLIMIT_NOFILE, 32, 32),
    ];
    if let Some(seconds) = cpu_seconds {
        limits.push((libc::RLIMIT_CPU, seconds, seconds + 1));
    }
    for (resource, soft, hard) in limits {
        let limit = libc::rlimit { rlim_cur: soft as libc::rlim_t, rlim_max: hard as libc::rlim_t };
        if unsafe { libc::setrlimit(resource, &limit) } != 0 {
            return Err(format!("setrlimit: {}", std::io::Error::last_os_error()));
        }
    }
    if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
        return Err(format!("no_new_privs: {}", std::io::Error::last_os_error()));
    }

    let landlock = restrict_files(tool)?;
    for filter in filters()? {
        seccompiler::apply_filter(&filter).map_err(|err| format!("seccomp: {err}"))?;
    }
    Ok(Layers { rlimits: true, no_new_privs: true, seccomp: true, landlock })
}

/// Becomes `tool`, with nothing of the server's environment. Returns only if
/// that fails.
pub(crate) fn exec(tool: &Path, args: &[OsString]) -> String {
    let err = std::process::Command::new(tool).args(args).env_clear().exec();
    format!("can't start {}: {err}", tool.display())
}

/// Landlock: no file system but reading and running the tool, if there is
/// one. Whether the kernel enforces it.
fn restrict_files(tool: Option<&Path>) -> Result<bool, String> {
    let fail = |err: &dyn std::fmt::Display| format!("landlock: {err}");
    let abi = ABI::V5;
    let tool = tool.map(PathFd::new).transpose().map_err(|err| fail(&err))?;
    let status = Ruleset::default()
        .handle_access(AccessFs::from_all(abi))
        .and_then(|ruleset| ruleset.create())
        .and_then(|ruleset| match tool {
            Some(tool) => ruleset.add_rule(PathBeneath::new(tool, AccessFs::Execute | AccessFs::ReadFile)),
            None => Ok(ruleset),
        })
        .and_then(|ruleset| ruleset.restrict_self())
        .map_err(|err| fail(&err))?;
    Ok(status.ruleset != RulesetStatus::NotEnforced)
}

/// The seccomp filters, in the order they are applied.
fn filters() -> Result<Vec<BpfProgram>, String> {
    let arch = TargetArch::try_from(std::env::consts::ARCH).map_err(|err| format!("seccomp: {err}"))?;
    let program = |rules: BTreeMap<i64, Vec<SeccompRule>>, errno: i32| -> Result<BpfProgram, String> {
        let filter = SeccompFilter::new(rules, SeccompAction::Allow, SeccompAction::Errno(errno as u32), arch)
            .map_err(|err| format!("seccomp: {err}"))?;
        filter.try_into().map_err(|err| format!("seccomp: {err}"))
    };
    let condition = |arg: u8, op: SeccompCmpOp, value: u64| {
        SeccompCondition::new(arg, SeccompCmpArgLen::Qword, op, value).map_err(|err| format!("seccomp: {err}"))
    };
    let rule = |conditions: Vec<SeccompCondition>| SeccompRule::new(conditions).map_err(|err| err.to_string());

    // clone3 passes its flags in memory, out of a filter's sight: "not here",
    // and the C library falls back to clone, whose flags it can check.
    let clone3 = BTreeMap::from([(libc::SYS_clone3, Vec::new())]);

    let pid = std::process::id() as u64;
    let mut denied: BTreeMap<i64, Vec<SeccompRule>> = DENIED.iter().map(|&call| (call, Vec::new())).collect();
    // New threads, not new processes.
    let not_a_thread = condition(0, SeccompCmpOp::MaskedEq(libc::CLONE_THREAD as u64), 0)?;
    denied.insert(libc::SYS_clone, vec![rule(vec![not_a_thread])?]);
    // Signals to itself only (the server runs as the same user).
    for call in [libc::SYS_kill, libc::SYS_tgkill] {
        denied.insert(call, vec![rule(vec![condition(0, SeccompCmpOp::Ne, pid)?])?]);
    }
    let mut programs = vec![program(clone3, libc::ENOSYS)?, program(denied, libc::EPERM)?];
    #[cfg(target_arch = "x86_64")]
    programs.insert(0, no_x32_calls());
    Ok(programs)
}

/// A filter that ends the process at any x32 system call. They have the
/// x86-64 architecture but other numbers (with bit 30 set), so the deny
/// list above, written with the x86-64 numbers, wouldn't see them. Most
/// kernels have x32 turned off; this one doesn't rely on that.
#[cfg(target_arch = "x86_64")]
fn no_x32_calls() -> BpfProgram {
    use seccompiler::sock_filter;
    const LOAD_WORD: u16 = 0x20; // BPF_LD | BPF_W | BPF_ABS
    const JUMP_IF_AT_LEAST: u16 = 0x35; // BPF_JMP | BPF_JGE | BPF_K
    const RETURN: u16 = 0x06; // BPF_RET | BPF_K
    const X32_BIT: u32 = 0x4000_0000;
    const KILL_PROCESS: u32 = 0x8000_0000;
    const ALLOW: u32 = 0x7fff_0000;
    vec![
        // The call's number: the first field of seccomp_data.
        sock_filter { code: LOAD_WORD, jt: 0, jf: 0, k: 0 },
        sock_filter { code: JUMP_IF_AT_LEAST, jt: 0, jf: 1, k: X32_BIT },
        sock_filter { code: RETURN, jt: 0, jf: 0, k: KILL_PROCESS },
        sock_filter { code: RETURN, jt: 0, jf: 0, k: ALLOW },
    ]
}

/// System calls a confined tool may not make at all.
const DENIED: &[i64] = &[
    // Files: opening, creating, changing, deleting.
    libc::SYS_openat,
    libc::SYS_openat2,
    libc::SYS_open_by_handle_at,
    libc::SYS_name_to_handle_at,
    libc::SYS_renameat,
    libc::SYS_renameat2,
    libc::SYS_unlinkat,
    libc::SYS_mkdirat,
    libc::SYS_mknodat,
    libc::SYS_linkat,
    libc::SYS_symlinkat,
    libc::SYS_fchmodat,
    libc::SYS_fchownat,
    libc::SYS_utimensat,
    libc::SYS_truncate,
    libc::SYS_setxattr,
    libc::SYS_lsetxattr,
    libc::SYS_removexattr,
    libc::SYS_lremovexattr,
    #[cfg(target_arch = "x86_64")]
    libc::SYS_open,
    #[cfg(target_arch = "x86_64")]
    libc::SYS_creat,
    #[cfg(target_arch = "x86_64")]
    libc::SYS_rename,
    #[cfg(target_arch = "x86_64")]
    libc::SYS_unlink,
    #[cfg(target_arch = "x86_64")]
    libc::SYS_mkdir,
    #[cfg(target_arch = "x86_64")]
    libc::SYS_rmdir,
    #[cfg(target_arch = "x86_64")]
    libc::SYS_mknod,
    #[cfg(target_arch = "x86_64")]
    libc::SYS_link,
    #[cfg(target_arch = "x86_64")]
    libc::SYS_symlink,
    #[cfg(target_arch = "x86_64")]
    libc::SYS_chmod,
    #[cfg(target_arch = "x86_64")]
    libc::SYS_chown,
    #[cfg(target_arch = "x86_64")]
    libc::SYS_lchown,
    #[cfg(target_arch = "x86_64")]
    libc::SYS_utime,
    #[cfg(target_arch = "x86_64")]
    libc::SYS_utimes,
    #[cfg(target_arch = "x86_64")]
    libc::SYS_futimesat,
    // Network.
    libc::SYS_socket,
    libc::SYS_socketpair,
    libc::SYS_connect,
    libc::SYS_bind,
    libc::SYS_listen,
    libc::SYS_accept,
    libc::SYS_accept4,
    // Other processes.
    #[cfg(target_arch = "x86_64")]
    libc::SYS_fork,
    #[cfg(target_arch = "x86_64")]
    libc::SYS_vfork,
    libc::SYS_tkill,
    libc::SYS_rt_sigqueueinfo,
    libc::SYS_rt_tgsigqueueinfo,
    libc::SYS_pidfd_open,
    libc::SYS_pidfd_getfd,
    libc::SYS_pidfd_send_signal,
    libc::SYS_ptrace,
    libc::SYS_process_vm_readv,
    libc::SYS_process_vm_writev,
    libc::SYS_kcmp,
    // The system.
    libc::SYS_mount,
    libc::SYS_umount2,
    libc::SYS_pivot_root,
    libc::SYS_chroot,
    libc::SYS_unshare,
    libc::SYS_setns,
    libc::SYS_bpf,
    libc::SYS_perf_event_open,
    libc::SYS_keyctl,
    libc::SYS_add_key,
    libc::SYS_request_key,
    libc::SYS_kexec_load,
    libc::SYS_kexec_file_load,
    libc::SYS_init_module,
    libc::SYS_finit_module,
    libc::SYS_delete_module,
    libc::SYS_reboot,
    libc::SYS_swapon,
    libc::SYS_swapoff,
    libc::SYS_acct,
    libc::SYS_quotactl,
    libc::SYS_userfaultfd,
    libc::SYS_personality,
    // io_uring does all of the above without these system calls.
    libc::SYS_io_uring_setup,
    libc::SYS_io_uring_enter,
    libc::SYS_io_uring_register,
];

/// Tries one thing in the confined process: `ok`, or `denied: <why>`.
#[cfg(feature = "testing")]
pub(crate) fn probe(what: &str) -> String {
    let answer = |result: std::io::Result<()>| match result {
        Ok(()) => "ok".to_owned(),
        Err(err) => format!("denied: {err}"),
    };
    let os = |code: libc::c_int| if code == -1 { Err(std::io::Error::last_os_error()) } else { Ok(()) };
    let (name, arg) = what.split_once(':').unwrap_or((what, ""));
    match name {
        "read" => answer(std::fs::read(arg).map(drop)),
        "write" => answer(std::fs::write(arg, b"x")),
        "socket" => answer(std::net::UdpSocket::bind("127.0.0.1:0").map(drop)),
        "thread" => answer(std::thread::spawn(|| 1 + 1).join().map(drop).map_err(|_| std::io::Error::other("panic"))),
        "spawn" => answer(std::process::Command::new(arg).status().map(drop)),
        // SAFETY: the child only exits; the parent waits for it.
        "fork" => answer(match unsafe { libc::fork() } {
            -1 => Err(std::io::Error::last_os_error()),
            0 => unsafe { libc::_exit(0) },
            child => os(unsafe { libc::waitpid(child, std::ptr::null_mut(), 0) }),
        }),
        "signal-self" => answer(os(unsafe { libc::kill(libc::getpid(), 0) })),
        "signal-parent" => answer(os(unsafe { libc::kill(libc::getppid(), 0) })),
        "alloc" => {
            let mib: usize = arg.parse().unwrap_or(0);
            let mut memory = Vec::<u8>::new();
            answer(memory.try_reserve_exact(mib * 1024 * 1024).map_err(std::io::Error::other))
        }
        other => format!("unknown probe {other}"),
    }
}
