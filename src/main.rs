use anyhow::{Context, Result};
use clap::Parser;
use reqwest::Client;
use serde::Deserialize;
use serde_yaml::from_reader;
use std::env;
use std::fs::File;
#[cfg(target_os = "linux")]
use std::fs;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use std::process::{Stdio, ExitStatus};
#[cfg(target_os = "linux")]
use tokio::time::{sleep, Duration};

#[derive(Parser)]
#[command(version, about = "notify when command exited or a running process finishes")]
struct Args {
    /// Monitor an existing process by PID (Linux only)
    #[arg(long, value_name = "PID")]
    pid: Option<i32>,

    /// Polling interval in milliseconds for --pid (Linux only)
    #[arg(long, default_value_t = 1000, requires = "pid")]
    poll_interval_ms: u64,

    /// Command to execute (conflicts with --pid)
    #[arg(required_unless_present = "pid", conflicts_with = "pid")]
    command: Vec<String>,
}

#[derive(Deserialize)]
struct Config {
    webhook_url: Option<String>,
    #[serde(default)]
    notify_command: Option<String>,
    #[serde(default = "default_success_message")]
    success_message: String,
    #[serde(default = "default_failure_message")]
    failure_message: String,
    #[serde(default = "default_watch_message")]
    watch_message: String,
}

fn default_success_message() -> String {
    String::from("command successfully finished\n$CMD")
}

fn default_failure_message() -> String {
    String::from("command execution failure\n$CMD\n\n$STDERR")
}

fn default_watch_message() -> String {
    String::from("process finished: pid=$PID\n$CMD")
}

fn load_config() -> Result<Config> {
    let config_path =
        env::var("XDG_CONFIG_HOME").unwrap_or(format!("{}/.config", env::var("HOME").unwrap()));
    let config_file = File::open(&format!("{}/everlong.yaml", config_path))
        .with_context(|| "Failed to open config file")?;

    from_reader(config_file).with_context(|| "Can't parse config")
}

async fn send_webhook(webhook_url: &str, message: &str) -> Result<()> {
    let client = Client::new();
    let payload = if webhook_url.contains("slack.com") {
        serde_json::json!({
            "text": message,
        })
    } else {
        serde_json::json!({
            "content": message
        })
    };
    client.post(webhook_url).json(&payload).send().await?;
    Ok(())
}

async fn run_notify_command(command: &str, message: &str) -> Result<()> {
    let shell = env::var("SHELL").unwrap_or(String::from("/bin/sh"));
    let mut child = Command::new(shell)
        .arg("-c")
        .arg(command)
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| "Failed to spawn notify_command")?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(message.as_bytes())
            .await
            .with_context(|| "Failed to write message to notify_command stdin")?;
    }

    let _status = child
        .wait()
        .await
        .with_context(|| "Failed to wait notify_command")?;

    Ok(())
}

async fn exec_command(cmd: &[String]) -> Result<(String, String, ExitStatus, u32)> {
    let shell = env::var("SHELL").unwrap_or(String::from("/bin/sh"));
    let command_str = cmd.join(" ");

    let mut child = Command::new(shell)
        .arg("-c")
        .arg(&command_str)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::piped())
        .spawn()
        .with_context(|| "Failed to spawn shell")?;

    let child_pid = child.id().unwrap_or_default();

    let stdout = child.stdout.take().with_context(|| "Failed to take stdout")?;
    let stderr = child.stderr.take().with_context(|| "Failed to take stderr")?;

    let mut stdout_reader = BufReader::new(stdout).lines();
    let mut stderr_reader = BufReader::new(stderr).lines();

    let mut stdout_output = String::new();
    let mut stderr_output = String::new();

    let stdout_task = tokio::spawn(async move {
        while let Some(line) = stdout_reader.next_line().await.unwrap() {
            println!("{}", line);
            stdout_output.push_str(&line);
            stdout_output.push('\n');
        }
        stdout_output
    });
    let stderr_task = tokio::spawn(async move {
        while let Some(line) = stderr_reader.next_line().await.unwrap() {
            eprintln!("{}", line);
            stderr_output.push_str(&line);
            stderr_output.push('\n');
        }
        stderr_output
    });

    let status = child.wait().await.with_context(|| "Failed to wait child shell")?;
    let stdout_output = stdout_task.await?;
    let stderr_output = stderr_task.await?;
    
    Ok((stdout_output, stderr_output, status, child_pid))

}

fn substitute_variables(
    message: &str,
    command: &str,
    stdout: &str,
    stderr: &str,
    pid: Option<u32>,
) -> String {
    message
        .replace("$CMD", command)
        .replace("$STDOUT", stdout)
        .replace("$STDERR", stderr)
        .replace("$PID", &pid.unwrap_or_default().to_string())
}
#[cfg(target_os = "macos")]
mod mac_kqueue {
    use super::*;
    use std::ffi::c_void;
    use std::ptr;
    use std::process::Command as StdCommand;

    #[repr(C)]
    struct KEvent {
        ident: usize,   // uintptr_t
        filter: i16,    // short
        flags: u16,     // unsigned short
        fflags: u32,    // unsigned int
        data: isize,    // intptr_t
        udata: *mut c_void,
    }

    const EVFILT_PROC: i16 = -5;
    const EV_ADD: u16 = 0x0001;
    const EV_ONESHOT: u16 = 0x0010;
    const NOTE_EXIT: u32 = 0x8000_0000;

    #[repr(C)]
    struct Timespec {
        tv_sec: i64,
        tv_nsec: i64,
    }

    extern "C" {
        fn kqueue() -> i32;
        fn kevent(
            kq: i32,
            changelist: *const KEvent,
            nchanges: i32,
            eventlist: *mut KEvent,
            nevents: i32,
            timeout: *const Timespec,
        ) -> i32;
        fn close(fd: i32) -> i32;
    }

    fn macos_cmdline(pid: i32) -> String {
        let out = StdCommand::new("ps")
            .args(["-o", "command=", "-p", &pid.to_string()])
            .output();
        match out {
            Ok(o) if o.status.success() => {
                let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
                if s.is_empty() { format!("(pid {})", pid) } else { s }
            }
            _ => format!("(pid {})", pid),
        }
    }

    fn watch_pid_macos_blocking(pid: i32) -> Result<String> {
        // capture cmdline early
        let cmdline = macos_cmdline(pid);

        // create kqueue and register NOTE_EXIT for pid
        let kq = unsafe { kqueue() };
        if kq < 0 {
            return Err(std::io::Error::last_os_error()).with_context(|| "kqueue() failed");
        }

        let change = KEvent {
            ident: pid as usize,
            filter: EVFILT_PROC,
            flags: EV_ADD | EV_ONESHOT,
            fflags: NOTE_EXIT,
            data: 0,
            udata: ptr::null_mut(),
        };

        let r = unsafe { kevent(kq, &change as *const KEvent, 1, ptr::null_mut(), 0, ptr::null()) };
        if r < 0 {
            unsafe { close(kq) };
            return Err(std::io::Error::last_os_error()).with_context(|| "kevent(register) failed");
        }

        // wait for exit
        let mut event = KEvent { ident: 0, filter: 0, flags: 0, fflags: 0, data: 0, udata: ptr::null_mut() };
        let r = unsafe { kevent(kq, ptr::null(), 0, &mut event as *mut KEvent, 1, ptr::null()) };
        unsafe { close(kq) };
        if r < 0 {
            return Err(std::io::Error::last_os_error()).with_context(|| "kevent(wait) failed");
        }

        Ok(cmdline)
    }

    pub async fn watch_pid_macos(pid: i32) -> Result<String> {
        tokio::task::spawn_blocking(move || watch_pid_macos_blocking(pid)).await?
    }
}

#[cfg(target_os = "macos")]
use mac_kqueue::watch_pid_macos;

#[cfg(target_os = "linux")]
fn read_linux_start_time(pid: i32) -> Result<u64> {
    let stat_path = format!("/proc/{}/stat", pid);
    let content = fs::read_to_string(&stat_path)
        .with_context(|| format!("Failed to read {}", stat_path))?;

    let rparen = content.rfind(')')
        .with_context(|| "Malformed /proc stat: missing ')' ")?;
    // Skip ") "
    let after = content.get(rparen + 1..).unwrap_or("");
    let after = after.strip_prefix(' ').unwrap_or(after);
    let parts: Vec<&str> = after.split_whitespace().collect();
    // parts[0] is state (3rd field), so starttime index is 22-3 = 19
    let st = parts
        .get(19)
        .with_context(|| "Malformed /proc stat: missing starttime")?
        .parse::<u64>()
        .with_context(|| "Failed to parse starttime")?;
    Ok(st)
}

#[cfg(target_os = "linux")]
fn read_linux_cmdline(pid: i32) -> Result<String> {
    let cmdline_path = format!("/proc/{}/cmdline", pid);
    let bytes = fs::read(&cmdline_path)
        .with_context(|| format!("Failed to read {}", cmdline_path))?;
    if bytes.is_empty() {
        return Ok(String::from("(unknown)"));
    }
    // cmdline is null-separated
    let mut s = String::from_utf8_lossy(&bytes).to_string();
    s = s.replace('\u{0}', " ").trim().to_string();
    Ok(s)
}

#[cfg(target_os = "linux")]
async fn watch_pid_linux(pid: i32, interval: Duration) -> Result<String> {
    let start_time = read_linux_start_time(pid)?;
    let cmdline = read_linux_cmdline(pid).unwrap_or_else(|_| format!("(pid {})", pid));

    loop {
        sleep(interval).await;
        match read_linux_start_time(pid) {
            Ok(cur) => {
                if cur != start_time {
                    break; // PID reused; original exited
                }
            }
            Err(_) => break, // /proc vanished => exited
        }
    }

    Ok(cmdline)
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let config = load_config()?;

    // PID watch mode (Linux/macOS)
    if let Some(pid) = args.pid {
        #[cfg(target_os = "linux")]
        {
            let cmdline = watch_pid_linux(pid, Duration::from_millis(args.poll_interval_ms)).await?;
            let message = substitute_variables(
                &config.watch_message,
                &cmdline,
                "",
                "",
                Some(pid as u32),
            );
            return dispatch_notifications(&config, &message).await;
        }
        #[cfg(target_os = "macos")]
        {
            let cmdline = watch_pid_macos(pid).await?;
            let message = substitute_variables(
                &config.watch_message,
                &cmdline,
                "",
                "",
                Some(pid as u32),
            );
            return dispatch_notifications(&config, &message).await;
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            anyhow::bail!("--pid is supported only on Linux/macOS for now");
        }
    }

    let (stdout, stderr, status, pid) = exec_command(&args.command).await?;

    let message = if status.success() {
        substitute_variables(
            &config.success_message,
            &args.command.join(" "),
            &stdout,
            &stderr,
            Some(pid),
        )
    } else {
        substitute_variables(
            &config.failure_message,
            &args.command.join(" "),
            &stdout,
            &stderr,
            Some(pid),
        )
    };

    dispatch_notifications(&config, &message).await
}

async fn dispatch_notifications(config: &Config, message: &str) -> Result<()> {
    let mut any_ok = false;
    let mut first_err: Option<anyhow::Error> = None;

    if let Some(url) = config.webhook_url.as_deref() {
        match send_webhook(url, message).await {
            Ok(_) => any_ok = true,
            Err(e) => if first_err.is_none() { first_err = Some(e); },
        }
    }

    if let Some(cmd) = config.notify_command.as_deref() {
        match run_notify_command(cmd, message).await {
            Ok(_) => any_ok = true,
            Err(e) => if first_err.is_none() { first_err = Some(e); },
        }
    }

    if any_ok {
        Ok(())
    } else {
        Err(first_err.unwrap_or_else(|| anyhow::anyhow!("No notification method configured")))
    }
}
