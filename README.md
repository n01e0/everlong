# everlong
The execution results of the command will be notified via Slack.

## Config
`$XDG_CONFIG_HOME/everlong.yaml`

```yaml
webhook_url: xxxx

# message templates
# $STDOUT replace with stdout
# $STDERR replace with stderr
# $CMD replace with executed commands
success_message: "blah blah"
failure_message: ":("
```
