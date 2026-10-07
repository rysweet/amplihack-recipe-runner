//! Concrete private-helper protocol, reaping and sealed-authority contracts.
use super::{GroupAnchor, validate_reaper};
use crate::adapters::codex_exec::{CleanupFailure, process::OwnedProcess};
use std::os::unix::process::CommandExt;
use std::{
    process::Command,
    time::{Duration, Instant},
};

fn launcher() -> std::process::Child {
    let mut command = Command::new("/bin/sleep");
    command.arg("30");
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command.spawn().unwrap()
}
fn wait_dead(anchor: &GroupAnchor) {
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        if unsafe { anchor.state().unwrap().si_pid() } != 0 {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "helper did not exit within its lifetime/EOF bound"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}
fn deny_close_range() {
    // Kernel-origin ENOSYS on the real production syscall, isolated to this worker.
    let mut filter = [
        libc::sock_filter {
            code: (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16,
            jt: 0,
            jf: 0,
            k: 0,
        },
        libc::sock_filter {
            code: (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16,
            jt: 0,
            jf: 1,
            k: libc::SYS_close_range as u32,
        },
        libc::sock_filter {
            code: (libc::BPF_RET | libc::BPF_K) as u16,
            jt: 0,
            jf: 0,
            k: libc::SECCOMP_RET_ERRNO | libc::ENOSYS as u32,
        },
        libc::sock_filter {
            code: (libc::BPF_RET | libc::BPF_K) as u16,
            jt: 0,
            jf: 0,
            k: libc::SECCOMP_RET_ALLOW,
        },
    ];
    let program = libc::sock_fprog {
        len: filter.len() as u16,
        filter: filter.as_mut_ptr(),
    };
    assert_eq!(
        unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) },
        0
    );
    assert_eq!(
        unsafe { libc::prctl(libc::PR_SET_SECCOMP, libc::SECCOMP_MODE_FILTER, &program) },
        0
    );
}
pub(in crate::adapters::codex_exec) fn worker(case: &str) {
    if case.starts_with("protocol_") {
        let mode = match case {
            "protocol_bad" => 8,
            "protocol_timeout" => 9,
            "protocol_pipe" => 11,
            "protocol_enosys" => 0,
            _ => panic!("unknown protocol case"),
        };
        crate::adapters::codex_exec::tests::retirement_fixtures::arm(0, 0, 0, 0, mode);
        if case == "protocol_enosys" {
            deny_close_range();
        }
        let root = tempfile::tempdir().unwrap();
        let mut command = Command::new("/bin/sleep");
        command.arg("30");
        let started = Instant::now();
        let error = match OwnedProcess::spawn(command, root.path(), root.path(), &[]) {
            Err(error) => error,
            Ok(mut child) => {
                child.shutdown(&mut Vec::new());
                panic!("invalid protocol succeeded");
            }
        };
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(error.downcast_ref::<CleanupFailure>().is_some());
        let diagnostic = format!("{error:#}");
        assert!(diagnostic.contains("anchor"));
        if case == "protocol_bad" {
            assert!(diagnostic.contains("Invalid Codex anchor acknowledgment"));
        }
        if case == "protocol_timeout" {
            assert!(diagnostic.contains("Timed out establishing"));
        }
        return;
    }
    if case == "state_boundary" {
        crate::adapters::codex_exec::tests::retirement_fixtures::arm(0, 0, 0, 0, 13);
    }
    validate_reaper().unwrap();
    let mut child = launcher();
    let group = child.id() as i32;
    let lifetime = if case == "state_expiry" {
        Some(30)
    } else {
        None
    };
    let mut anchor = GroupAnchor::establish(
        &mut child,
        Instant::now() + Duration::from_millis(100),
        lifetime,
    )
    .unwrap();
    let helper = anchor.pid;
    child.kill().unwrap();
    child.wait().unwrap();
    if case == "state_eof" {
        anchor.control.take();
        wait_dead(&anchor);
    }
    if case == "state_expiry" {
        wait_dead(&anchor);
    }
    if case == "state_stolen" {
        assert_eq!(unsafe { libc::kill(helper, libc::SIGKILL) }, 0);
        wait_dead(&anchor);
        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(helper, &mut status, 0) }, helper);
    }
    if case == "state_policy_change" {
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = libc::SIG_IGN;
            assert_eq!(
                libc::sigaction(libc::SIGCHLD, &action, std::ptr::null_mut()),
                0
            );
        }
        assert!(anchor.signal(libc::SIGTERM, Instant::now()).is_err());
        // Restore before owned fallback; group operations were refused under the policy.
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = libc::SIG_DFL;
            assert_eq!(
                libc::sigaction(libc::SIGCHLD, &action, std::ptr::null_mut()),
                0
            );
        }
    }
    if case == "state_drop" {
        drop(anchor);
        assert_eq!(
            unsafe { libc::waitpid(helper, std::ptr::null_mut(), libc::WNOHANG) },
            -1
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
        return;
    }
    let result = anchor.signal(libc::SIGKILL, Instant::now() + Duration::from_secs(2));
    if matches!(
        case,
        "state_eof" | "state_expiry" | "state_stolen" | "state_boundary"
    ) {
        assert!(result.is_err());
    } else {
        result.unwrap();
    }
    assert!(anchor.sealed && anchor.retired);
    // A numeric PGID substituted with an unrelated live group after release supplies
    // no authority. The production signaling entry must refuse before any syscall.
    let mut unrelated = launcher();
    let replacement_group = unrelated.id() as i32;
    let mut member_command = Command::new("/bin/sleep");
    member_command.arg("30");
    unsafe {
        member_command.pre_exec(move || {
            if libc::setpgid(0, replacement_group) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut replacement_member = member_command.spawn().unwrap();
    // Model reuse of BOTH cached numeric identities with a live unrelated group
    // member. ECHILD alone must not accidentally make this authority test pass.
    anchor.group = replacement_group;
    anchor.pid = replacement_member.id() as i32;
    let refused = anchor.signal(libc::SIGKILL, Instant::now());
    assert!(refused.is_err());
    assert!(unrelated.try_wait().unwrap().is_none());
    let member_survived = replacement_member.try_wait().unwrap().is_none();
    replacement_member.kill().unwrap();
    replacement_member.wait().unwrap();
    unrelated.kill().unwrap();
    unrelated.wait().unwrap();
    assert!(
        member_survived,
        "sealed authority signaled a reused live group member"
    );
    assert!(!crate::adapters::codex_exec::process::group_live(group).unwrap());
}
macro_rules! isolated {
    ($name:ident, $case:literal) => {
        #[test]
        fn $name() {
            crate::adapters::codex_exec::tests::extended_fixtures::run($case);
        }
    };
}
isolated!(invalid_ack_refuses_and_unwinds, "protocol_bad");
isolated!(withheld_ack_times_out_and_unwinds, "protocol_timeout");
isolated!(failed_ack_refuses_and_unwinds, "protocol_pipe");
isolated!(
    unavailable_close_range_refuses_and_unwinds,
    "protocol_enosys"
);
isolated!(parent_control_eof_exits_and_retains_failure, "state_eof");
isolated!(
    configured_lifetime_exits_and_retains_failure,
    "state_expiry"
);
isolated!(stolen_child_refuses_group_access, "state_stolen");
isolated!(
    changed_reaper_policy_refuses_group_access,
    "state_policy_change"
);
isolated!(drop_reaps_helper_without_second_close, "state_drop");
isolated!(
    sealed_authority_refuses_reused_numeric_group,
    "state_sealed"
);

isolated!(
    unexpected_exit_at_final_signal_boundary_remains_terminal,
    "state_boundary"
);
