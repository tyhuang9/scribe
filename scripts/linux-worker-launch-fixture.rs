use std::fs;
use std::io::{self, Read, Seek, SeekFrom, Write};
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
    if std::env::var("SCRIBE_PRIVATE_EXECUTABLE_FD").as_deref() != Ok("3") {
        return Err(io::Error::other("unexpected executable image fd"));
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
            let image_sha256 = inherited_image_sha256(3)?;
            println!("ARGS={}", args[1]);
            println!("ENV={}", names.join(","));
            println!(
                "FDS={}",
                fds.iter().map(i32::to_string).collect::<Vec<_>>().join(",")
            );
            println!("CWD={}", std::env::current_dir()?.display());
            println!("INPUT={input}");
            println!("IMAGE_SHA256={image_sha256}");
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

fn inherited_image_sha256(fd: i32) -> io::Result<String> {
    // SAFETY: the launcher gives this fixture sole ownership of fixed FD 3.
    let mut image = unsafe { fs::File::from_raw_fd(fd) };
    image.seek(SeekFrom::Start(0))?;
    let mut child = Command::new("/usr/bin/sha256sum")
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    io::copy(
        &mut image,
        child
            .stdin
            .as_mut()
            .ok_or_else(|| io::Error::other("sha256sum stdin unavailable"))?,
    )?;
    drop(child.stdin.take());
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(io::Error::other("sha256sum rejected inherited image"));
    }
    let text = String::from_utf8(output.stdout)
        .map_err(|_| io::Error::other("sha256sum output was not UTF-8"))?;
    text.split_whitespace()
        .next()
        .filter(|digest| digest.len() == 64)
        .map(str::to_owned)
        .ok_or_else(|| io::Error::other("sha256sum output was malformed"))
}
