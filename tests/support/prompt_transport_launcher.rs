//! Standalone Rust wrapper fixture, compiled by tests/prompt_transport.rs.
//! Control disconnection releases the fixture; no sleep, polling or timer.
use std::{
    env, fs,
    io::{self, BufRead, BufReader, Read, Write},
    os::fd::AsRawFd,
    os::unix::net::UnixStream,
    path::PathBuf,
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, AtomicI32, Ordering},
};

unsafe extern "C" {
    fn fcntl(fd: i32, command: i32, ...) -> i32;
    fn signal(signal: i32, handler: usize) -> usize;
    fn write(fd: i32, bytes: *const u8, count: usize) -> isize;
    fn kill(pid: i32, signal: i32) -> i32;
}

static SIGNAL_SOCKET: AtomicI32 = AtomicI32::new(-1);
static BACKEND_NOTIFIED: AtomicBool = AtomicBool::new(false);
static BACKEND_PID: AtomicI32 = AtomicI32::new(0);

extern "C" fn wrapper_shutdown(_: i32) {
    let bytes = b"WRAPPER_SHUTDOWN\n";
    unsafe {
        write(
            SIGNAL_SOCKET.load(Ordering::Relaxed),
            bytes.as_ptr(),
            bytes.len(),
        );
        let pid = BACKEND_PID.load(Ordering::Relaxed);
        if pid > 1 {
            kill(pid, 15);
        }
    }
}

extern "C" fn backend_shutdown(_: i32) {
    // Group TERM and the wrapper's forwarded TERM can both reach the backend.
    if BACKEND_NOTIFIED.swap(true, Ordering::Relaxed) {
        return;
    }
    let bytes = b"BACKEND_SHUTDOWN\n";
    unsafe {
        write(
            SIGNAL_SOCKET.load(Ordering::Relaxed),
            bytes.as_ptr(),
            bytes.len(),
        );
    }
}

fn message(wire: &mut BufReader<UnixStream>, text: &str) -> io::Result<()> {
    writeln!(wire.get_mut(), "{text}")
}

fn command(wire: &mut BufReader<UnixStream>) -> io::Result<String> {
    let mut line = String::new();
    wire.read_line(&mut line)?;
    Ok(line.trim_end().to_owned())
}

fn run() -> io::Result<()> {
    let root = PathBuf::from(env::var_os("PROMPT_TEST_ROOT").unwrap());
    let backend = env::var_os("PROMPT_TEST_BACKEND").is_some();
    let mut wire = BufReader::new(UnixStream::connect(root.join(if backend {
        "backend.sock"
    } else {
        "agent.sock"
    }))?);
    let args: Vec<_> = env::args().skip(1).collect();
    let scenario = fs::read_to_string(root.join("scenario"))?;
    if backend || scenario.starts_with("descendant_") {
        SIGNAL_SOCKET.store(wire.get_ref().as_raw_fd(), Ordering::Relaxed);
        unsafe {
            signal(
                15,
                if backend {
                    backend_shutdown as *const () as usize
                } else {
                    wrapper_shutdown as *const () as usize
                },
            );
        }
    }
    fs::write(
        root.join("wrapper.cwd"),
        env::current_dir()?.as_os_str().as_encoded_bytes(),
    )?;
    let final_message = args
        .windows(2)
        .find_map(|pair| (pair[0] == "--output-last-message").then(|| PathBuf::from(&pair[1])));
    message(
        &mut wire,
        &format!(
            "{} {}",
            if backend {
                "BACKEND_STARTED"
            } else {
                "STARTED"
            },
            std::process::id()
        ),
    )?;
    let mut input = Vec::new();
    let mut instruction = command(&mut wire)?;
    if instruction == "PREFIX" {
        // Linux F_GETPIPE_SZ: establish that the remaining test payload cannot
        // fit while this wrapper waits for its explicit next instruction.
        let capacity = unsafe { fcntl(0, 1032) };
        let mut byte = [0];
        let count = io::stdin().read(&mut byte)?;
        input.extend_from_slice(&byte[..count]);
        message(&mut wire, &format!("PREFIX_RECEIVED {count} {capacity}"))?;
        instruction = command(&mut wire)?;
    }
    if instruction != "READ" {
        return Ok(());
    }
    if !backend && scenario.starts_with("descendant_") {
        let mut child = Command::new(env::current_exe()?)
            .args(&args)
            .env("PROMPT_TEST_BACKEND", "1")
            .stdin(Stdio::inherit())
            .spawn()?;
        BACKEND_PID.store(child.id() as i32, Ordering::Relaxed);
        message(&mut wire, &format!("BACKEND_LAUNCHED {}", child.id()))?;
        if scenario == "descendant_orphan" {
            // The backend remains in the invocation group after this wrapper exits.
            let _ = command(&mut wire)?;
            return Ok(());
        }
        let status = child.wait()?;
        fs::write(root.join("backend.wait_status"), status.to_string())?;
        return Ok(());
    }
    // Consume the same input the real backend would consume, rather than
    // treating the controller's filesystem snapshot as proof of receipt.
    let mut system = Vec::new();
    let mut prompt_path = None;
    for pair in args.windows(2) {
        match pair[0].as_str() {
            "--system-prompt-file" | "--append-system-prompt-file" => {
                system = fs::read(&pair[1])?;
                prompt_path = Some(PathBuf::from(&pair[1]));
            }
            "--system-prompt" => system = pair[1].as_bytes().to_vec(),
            "--add-dir" if args.first().is_some_and(|arg| arg == "copilot") => {
                let pointer = args.windows(2).find(|pair| pair[0] == "-p");
                for entry in fs::read_dir(&pair[1])? {
                    let path = entry?.path();
                    if path.is_file()
                        && pointer.is_some_and(|pair| pair[1].contains(path.to_str().unwrap()))
                    {
                        system = fs::read(&path)?;
                        prompt_path = Some(path);
                    }
                }
            }
            _ => {}
        }
    }
    fs::write(root.join("system.received"), system)?;
    if scenario != "early_zero" {
        // Reading to EOF proves delivery closed stdin, including empty input.
        io::stdin().read_to_end(&mut input)?;
        fs::write(root.join("stdin.received"), input)?;
    }
    if let Some(path) = final_message {
        fs::write(path, b"STUB_FINAL")?;
    }
    message(&mut wire, "INPUT_RECEIVED")?;
    loop {
        match command(&mut wire)?.as_str() {
            "CHECK" if backend => {
                let bytes = fs::read(prompt_path.as_ref().unwrap())?;
                if bytes != fs::read(root.join("system.received"))? {
                    return Err(io::Error::other("prompt changed during shutdown"));
                }
                message(&mut wire, "PROMPT_AVAILABLE")?;
            }
            "RELEASE" => break,
            _ => return Ok(()),
        }
    }
    println!("STUB_STDOUT");
    eprintln!("STUB_STDERR");
    if scenario == "nonzero" {
        std::process::exit(7);
    }
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        // An owner closing the control socket is normal cancellation.
        if !matches!(
            error.kind(),
            io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset
        ) {
            eprintln!("fixture I/O failed: {error}");
            std::process::exit(90);
        }
    }
}
