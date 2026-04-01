# dmarc2mqtt

Reads DMARC reports from ann IMAP mailbox and forwards a processed reports via MQTT to be integrated in home automation systems like home assistant.

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
1. [ ] wrap in docker container
1. [ ] re-read config before every execution (not only at startup)

## Design considerations

- Security: Build outside of home assistant to isolate alle the mail processing.
- Security: Use Rust as memory safe language.
- Interoperability: Export data using MQTT to allow integration in other services.

## Configuration

Create or edit `config.yaml`:

```yaml
mqtt:
  server_name: "mqtt.example.local"
  server_port: 1883
  login: "dmarc2mqtt"
  password: "change-me"
  base_topic: "mail/dmarc"
```
