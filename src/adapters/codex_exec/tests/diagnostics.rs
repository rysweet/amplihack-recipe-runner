use super::super::diagnostics::{classify_failure, drain_diagnostics};

#[test]
fn readers_cap_stored_bytes_and_report_failure() {
    use std::io::Write;
    use std::os::fd::FromRawFd;
    fn pair() -> (std::fs::File, std::fs::File) {
        let mut fds = [0; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        unsafe {
            (
                std::fs::File::from_raw_fd(fds[0]),
                std::fs::File::from_raw_fd(fds[1]),
            )
        }
    }
    let (reader, mut writer) = pair();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let handle = drain_diagnostics(reader, stop.clone());
    writer.write_all(&vec![b'x'; 1024 * 1024]).unwrap();
    writer
        .write_all(b"authentication failed: SECRET_CANARY")
        .unwrap();
    drop(writer);
    let tail = handle.join().unwrap().unwrap();
    assert_eq!(tail.len(), 64 * 1024);
    assert!(tail.ends_with(b"authentication failed: SECRET_CANARY"));
    assert!(classify_failure(&String::from_utf8_lossy(&tail), false).contains("auth"));

    struct Broken(std::fs::File);
    impl std::os::fd::AsRawFd for Broken {
        fn as_raw_fd(&self) -> i32 {
            self.0.as_raw_fd()
        }
    }
    impl std::io::Read for Broken {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("injected reader failure"))
        }
    }
    let (reader, _) = pair();
    assert!(
        drain_diagnostics(Broken(reader), stop)
            .join()
            .unwrap()
            .is_err()
    );
}

#[test]
fn continuously_readable_diagnostics_observe_stop() {
    struct Continuous {
        file: std::fs::File,
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
        reads: usize,
    }
    impl std::os::fd::AsRawFd for Continuous {
        fn as_raw_fd(&self) -> i32 {
            self.file.as_raw_fd()
        }
    }
    impl std::io::Read for Continuous {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            self.reads += 1;
            assert!(self.reads <= 228, "reader ignored bounded completion");
            buffer.fill(b'x');
            if self.reads == 100 {
                self.stop.store(true, std::sync::atomic::Ordering::Release);
            }
            Ok(buffer.len())
        }
    }
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let pipe = Continuous {
        file: tempfile::tempfile().unwrap(),
        stop: stop.clone(),
        reads: 0,
    };
    assert_eq!(
        drain_diagnostics(pipe, stop).join().unwrap().unwrap().len(),
        64 * 1024
    );
}

#[test]
fn failure_categories_never_echo_provider_secrets() {
    for (message, limited) in [
        ("authentication failed SECRET_CANARY", false),
        ("rate limit SECRET_CANARY", true),
        ("unknown provider error SECRET_CANARY", false),
    ] {
        let category = classify_failure(message, limited);
        assert!(!category.contains("SECRET_CANARY"));
        assert!(!category.is_empty());
    }
}
