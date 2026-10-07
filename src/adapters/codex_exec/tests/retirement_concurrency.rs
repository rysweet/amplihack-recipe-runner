//! Independent first-owner admission races the last owner's locked retirement.
use super::super::{Interruption, cancellation::Cancellation, check_cancellation};
use std::{
    sync::{Arc, Barrier},
    time::{Duration, Instant},
};

unsafe fn resolve(name: &std::ffi::CStr) -> unsafe extern "C" fn() -> i32 {
    let ptr = unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) };
    assert!(!ptr.is_null());
    unsafe { std::mem::transmute(ptr) }
}
pub(super) fn worker(case: &str) {
    let signal = if case == "race_int" {
        libc::SIGINT
    } else {
        libc::SIGTERM
    };
    super::retirement_fixtures::arm(signal, 3, 0, 0, 0);
    let owner = Cancellation::install().unwrap();
    let stage = unsafe { resolve(c"lifecycle_retiring") };
    let attempt = unsafe { resolve(c"lifecycle_attempting_install") };
    let barrier = Arc::new(Barrier::new(2));
    let ready = barrier.clone();
    let next = std::thread::spawn(move || {
        ready.wait();
        let deadline = Instant::now() + Duration::from_secs(3);
        while unsafe { stage() } == 0 {
            assert!(
                Instant::now() < deadline,
                "restoration boundary was not reached"
            );
            std::thread::yield_now();
        }
        assert_eq!(unsafe { attempt() }, 1);
        let independent = Cancellation::install().unwrap();
        let clean = check_cancellation().is_ok();
        let result = independent.close();
        (clean, result)
    });
    barrier.wait();
    let retired = owner.close();
    let (clean, result) = next.join().unwrap();
    assert!(clean, "independent epoch inherited the retired observation");
    result.unwrap();
    let error = retired.expect_err("next owner reset the retiring owner's cancellation");
    assert_eq!(error.downcast_ref::<Interruption>().unwrap().signal, signal);
}
#[test]
fn sigint_independent_install_races_owned_retirement() {
    super::retirement_fixtures::run("race_int");
}
#[test]
fn sigterm_independent_install_races_owned_retirement() {
    super::retirement_fixtures::run("race_term");
}
