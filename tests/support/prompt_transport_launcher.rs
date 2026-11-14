//! Standalone Rust wrapper fixture, compiled by tests/prompt_transport.rs.
//! Control disconnection releases the fixture; no sleep, polling or timer.
use std::{
    env, fs,
    io::{self, BufRead, BufReader, Read, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
};

unsafe extern "C" {
    fn fcntl(fd: i32, command: i32, ...) -> i32;
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
    let mut wire = BufReader::new(UnixStream::connect(root.join("agent.sock"))?);
    let args: Vec<_> = env::args().skip(1).collect();
    fs::write(
        root.join("wrapper.cwd"),
        env::current_dir()?.as_os_str().as_encoded_bytes(),
    )?;
    let final_message = args
        .windows(2)
        .find_map(|pair| (pair[0] == "--output-last-message").then(|| PathBuf::from(&pair[1])));
    message(&mut wire, &format!("STARTED {}", std::process::id()))?;
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
    // Consume the same input the real backend would consume, rather than
    // treating the controller's filesystem snapshot as proof of receipt.
    let mut system = Vec::new();
    for pair in args.windows(2) {
        match pair[0].as_str() {
            "--system-prompt-file" | "--append-system-prompt-file" => {
                system = fs::read(&pair[1])?;
            }
            "--system-prompt" => system = pair[1].as_bytes().to_vec(),
            "--add-dir" if args.first().is_some_and(|arg| arg == "copilot") => {
                let pointer = args.windows(2).find(|pair| pair[0] == "-p");
                for entry in fs::read_dir(&pair[1])? {
                    let path = entry?.path();
                    if path.is_file()
                        && pointer.is_some_and(|pair| pair[1].contains(path.to_str().unwrap()))
                    {
                        system = fs::read(path)?;
                    }
                }
            }
            _ => {}
        }
    }
    fs::write(root.join("system.received"), system)?;
    let scenario = fs::read_to_string(root.join("scenario"))?;
    if scenario != "early_zero" {
        // Reading to EOF proves delivery closed stdin, including empty input.
        io::stdin().read_to_end(&mut input)?;
        fs::write(root.join("stdin.received"), input)?;
    }
    if let Some(path) = final_message {
        fs::write(path, b"STUB_FINAL")?;
    }
    message(&mut wire, "INPUT_RECEIVED")?;
    if command(&mut wire)? != "RELEASE" {
        return Ok(());
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
