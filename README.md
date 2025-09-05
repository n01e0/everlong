# everlong
The execution results of the command will be notified via Slack.
Alternatively or additionally, you can pipe the message to an external command.
You can also monitor an existing process by PID (Linux/macOS).

![xkcd.com/303](https://imgs.xkcd.com/comics/compiling.png)

## installation
```sh
cargo install everlong
```

## usage
- Execute and notify when finished
  - `everlong <command ...>`

- Monitor an existing process (Linux/macOS)
  - Linux: `everlong --pid <PID> [--poll-interval-ms 1000]`
  - macOS: `everlong --pid <PID>` (kqueue/EVFILT_PROC)

## Config
`$XDG_CONFIG_HOME/everlong.yaml`

```yaml
webhook_url: xxxx # optional
notify_command: "notify-send 'everlong'" # optional, message is piped to stdin

# message templates
# Available variables:
#  - $CMD: executed command (or monitored process cmdline)
#  - $STDOUT: captured stdout (execute mode only)
#  - $STDERR: captured stderr (execute mode only)
#  - $PID: process id
success_message: "blah blah"
failure_message: ":("
# Message used for --pid mode (Linux)
watch_message: "process finished: pid=$PID\n$CMD"
```
