use std::fs;
use std::io::{self, Read, Write};
use std::os::fd::FromRawFd;
use std::process::{Command, Stdio};
use std::time::Duration;

fn main() -> io::Result<()> {
    let args = std::env::args().collect::<Vec<_>>();
    if args.len() != 2 || args[1] != "--scribe-inference-worker" {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unexpected argv",
        ));
    }
    let mode = std::env::var("SCRIBE_PRIVATE_TEST_MODE").unwrap_or_else(|_| "inspect".into());
    match mode.as_str() {
        "inspect" => {
            let mut names = std::env::vars().map(|(name, _)| name).collect::<Vec<_>>();
            names.sort();
            let mut fds = fs::read_dir("/proc/self/fd")?
                .filter_map(Result::ok)
                .filter_map(|entry| entry.file_name().to_string_lossy().parse::<i32>().ok())
                .collect::<Vec<_>>();
            fds.sort_unstable();
            let mut input = String::new();
            io::stdin().read_to_string(&mut input)?;
            println!("ARGS={}", args[1]);
            println!("ENV={}", names.join(","));
            println!(
                "FDS={}",
                fds.iter().map(i32::to_string).collect::<Vec<_>>().join(",")
            );
            println!("CWD={}", std::env::current_dir()?.display());
            println!("INPUT={input}");
        }
        "hang" => loop {
            std::thread::sleep(Duration::from_secs(60));
        },
        "cooperative" => {
            let fd = std::env::var("SCRIBE_PRIVATE_PARENT_LIVENESS")
                .map_err(|_| io::Error::other("missing liveness fd"))?;
            let mut control = unsafe { fs::File::from_raw_fd(fd.parse().unwrap()) };
            let mut byte = [0_u8; 1];
            control.read_exact(&mut byte)?;
            if byte != [b'C'] {
                return Err(io::Error::other("unexpected control byte"));
            }
        }
        "process-group" => {
            let child = Command::new("/bin/sleep")
                .arg("60")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()?;
            println!("PIDS={},{}", std::process::id(), child.id());
            io::stdout().flush()?;
            loop {
                std::thread::sleep(Duration::from_secs(60));
            }
        }
        "leader-exit" => {
            let child = Command::new("/bin/sleep")
                .arg("60")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()?;
            println!("PIDS={},{}", std::process::id(), child.id());
            io::stdout().flush()?;
            let mut release = [0_u8; 1];
            let _ = io::stdin().read(&mut release)?;
        }
        _ => return Err(io::Error::new(io::ErrorKind::InvalidInput, "unknown mode")),
    }
    Ok(())
}
