use anyhow::{Context, Result};
use clap::Parser;
use reqwest::Client;
use serde::Deserialize;
use serde_yaml::from_reader;
use std::env;
use std::fs::File;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use std::process::{Stdio, ExitStatus};

#[derive(Parser)]
struct Args {
    command: Vec<String>,
}

#[derive(Deserialize)]
struct Config {
    webhook_url: String,
    #[serde(default = "default_success_message")]
    success_message: String,
    #[serde(default = "default_failure_message")]
    failure_message: String,
}

fn default_success_message() -> String {
    String::from("command successfully finished\n$CMD")
}

fn default_failure_message() -> String {
    String::from("command execution failure\n$CMD\n\n$STDERR")
}

fn load_config() -> Result<Config> {
    let config_path =
        env::var("XDG_CONFIG_HOME").unwrap_or(format!("{}/.config", env::var("HOME").unwrap()));
    let config_file = File::open(&format!("{}/everlong.yaml", config_path))
        .with_context(|| "Failed to open config file")?;

    from_reader(config_file).with_context(|| "Can't parse config")
}

async fn send_notification(webhook_url: &str, message: &str) -> Result<()> {
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

async fn exec_command(cmd: &[String]) -> Result<(String, String, ExitStatus)> {
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
    
    Ok((stdout_output, stderr_output, status))

}

fn substitute_variables(message: &str, command: &str, stdout: &str, stderr: &str) -> String {
    message
        .replace("$CMD", command)
        .replace("$STDOUT", stdout)
        .replace("$STDERR", stderr)
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let config = load_config()?;

    let (stdout, stderr, status)= exec_command(&args.command).await?;


    let message = if status.success() {
        substitute_variables(
            &config.success_message,
            &args.command.join(" "),
            &stdout,
            &stderr,
        )
    } else {
        substitute_variables(
            &config.failure_message,
            &args.command.join(" "),
            &stdout,
            &stderr,
        )
    };

    send_notification(&config.webhook_url, &message).await
}
