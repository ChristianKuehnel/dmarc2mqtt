# dmarc2mqtt

Reads DMARC reports from ann IMAP mailbox and forwards a processed reports via MQTT to be integrated in home automation systems like home assistant.

    Note: This program was mostly coded by AI and wasn't reviewed thoroughly. So it mit do weird things.

## Feature Roadmap
1. [x] read xml
1. [x] extract a few key results
1. [ ] validate xml
2. [x] Push to mqtt
3. [x] publish HA sensor config
4. [x] Read zipped files
1. [ ] validate zip security
5. [x] read from imap and move to trash
6. [ ] add persistent storage to avoid flapping sensors, remove sensor after x days
1. [ ] add UUIDs for sensors?
7. [ ] add more values/sensors?
1. [x] wrap in docker container
1. [x] re-read config before every execution (not only at startup)

## Design considerations

- Security: Build outside of home assistant to isolate alle the mail processing.
- Security: Use Rust as memory safe language.
- Interoperability: Export data using MQTT to allow integration in other services.
- Deployment: Deploy as container
- Automation: Read dmarc reports via IMAP, as they arrive via email


## Configuration

Create or edit `config.yaml`:

```yaml
mqtt:
  server_name: "mqtt.example.local"
  server_port: 1883
  login: "dmarc2mqtt"
  password: "change-me"
  base_topic: "mail/dmarc"
imap:
  server_name: "imap.example.com"
  server_port: 993
  login: "user@example.com"
  password: "change-me"
  report_folder: "INBOX/DMARC Reports"
  trash_folder: "Trash"
  move_emails: true
  poll_cron: "0 0 */6 * * *"
```

## Docker

Build the image:

```bash
docker build -t dmarc2mqtt:latest .
```

Use a mounted config directory. A starter config is included at `docker/config/config.template.yaml`.

Create your runtime config file:

```bash
cp docker/config/config.template.yaml docker/config/config.yaml
```

```bash
docker run --rm \
  -v "$(pwd)/docker/config:/config:ro" \
  dmarc2mqtt:latest
```

The container entrypoint reads `/config/config.yaml`, so edit the host file at `docker/config/config.yaml` (or mount your own directory with a `config.yaml` file).
