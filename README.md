# dmarc2mqtt

Reads DMARC reports from one or more IMAP mailboxes and forwards processed reports via MQTT to be integrated in home automation systems like Home Assistant.

    Note: This program was mostly coded by AI and wasn't reviewed thoroughly. So it might do weird things.

## Design considerations

- Security: Build outside of home assistant to isolate alle the mail processing.
- Security: Use Rust as memory safe language.
- Interoperability: Export data using MQTT to allow integration in other services.
- Deployment: Deploy as container
- Automation: Read DMARC reports via IMAP, as they arrive via email


## Configuration

Create or edit `config.yaml`:

```yaml
mqtt:
  server_name: "mqtt.example.local"
  server_port: 8883
  login: "dmarc2mqtt"
  password: "change-me"
  base_topic: "mail/dmarc"
  tls: true
  allow_insecure: false
  remove_stale_sensors: 30
imap:
  mailboxes:
    - name: "primary"
      server_name: "imap.example.com"
      server_port: 993
      login: "user@example.com"
      password: "change-me"
      report_folder: "INBOX/DMARC Reports"
      trash_folder: "Trash"
      move_emails: true
      poll_cron: "0 0 */6 * * *"
      max_xml_size: 10
      max_message_size: 25
      max_attachment_size: 10
      max_zip_entries: 1000
      max_zip_xml_files: 10
      max_zip_uncompressed_size: 10
    - name: "secondary"
      server_name: "imap.other.example"
      server_port: 993
      login: "other-user@example.com"
      password: "change-me-too"
      report_folder: "INBOX/Reports/DMARC"
      trash_folder: "Trash"
      move_emails: false
      poll_cron: "0 30 */12 * * *"
      max_xml_size: 20
      max_message_size: 25
      max_attachment_size: 20
      max_zip_entries: 1000
      max_zip_xml_files: 10
      max_zip_uncompressed_size: 20
```

After each mailbox poll, the app updates `history.json` in the same directory as the config file.
It stores the latest `last_seen_epoch` (Unix epoch seconds) for every `org_name + domain` tuple.
Home Assistant discovery is announced for all tuples from `history.json` on every poll, so sensors stay available even when no new reports arrive.
If `mqtt.remove_stale_sensors` is set, tuples not seen for more than that many days are removed from history.
Each mailbox has its own connection settings, folders, schedule and `max_xml_size`.
`imap.mailboxes[].max_xml_size` defines the maximum allowed uncompressed XML payload size in MB (applies to XML, GZIP and ZIP attachments).
`imap.mailboxes[].max_message_size` caps IMAP messages before fetching their full RFC822 body, and `max_attachment_size` caps encoded and decoded attachment payloads before parsing.
ZIP attachments are additionally bounded by `max_zip_entries`, `max_zip_xml_files`, and `max_zip_uncompressed_size` to limit archive-wide work.
IMAP connections always use TLS; configure an IMAPS endpoint, typically port `993`.
MQTT uses TLS by default (`mqtt.tls: true`) and the examples use port `8883`.
If you must connect to a plaintext broker, set `mqtt.tls: false` and explicitly opt in with `mqtt.allow_insecure: true`.


## Container Image

Published images are available from GitHub Container Registry:

```text
ghcr.io/christiankuehnel/dmarc2mqtt:latest
```

Use a mounted config directory. A starter config is included at `docker/config/config.template.yaml`.

Create your runtime config file:

```bash
cp docker/config/config.template.yaml docker/config/config.yaml
```

Run with Docker:

```bash
docker run --rm \
  --name dmarc2mqtt \
  -v "$(pwd)/docker/config:/config" \
  ghcr.io/christiankuehnel/dmarc2mqtt:latest
```

Run with Podman:

```bash
podman run --rm \
  --name dmarc2mqtt \
  -v "$(pwd)/docker/config:/config:Z" \
  ghcr.io/christiankuehnel/dmarc2mqtt:latest
```

The container entrypoint reads `/config/config.yaml`, so edit the host file at `docker/config/config.yaml` (or mount your own directory with a `config.yaml` file).
The container writes `history.json` next to the mounted config file, so the mounted directory must be writable if you want history to persist.

To build the image locally instead:

```bash
docker build -t dmarc2mqtt:latest .
```

## MQTT Output

The app publishes retained MQTT messages using the configured `mqtt.base_topic`.
With the example `base_topic: "mail/dmarc"`, it publishes two kinds of state topics:

```text
mail/dmarc/sensors/<reporter>_<domain>/state
mail/dmarc/domains/<domain>/state
```

The `<reporter>` value comes from the DMARC report metadata `report_metadata/org_name`.
The `<domain>` value comes from `policy_published/domain`.
Both values are normalized for topic names: ASCII letters and numbers are lowercased and kept, any other run of characters becomes `_`.
For example, `Google Inc.` reporting on `example.com` becomes:

```text
mail/dmarc/sensors/google_inc_example_com/state
mail/dmarc/domains/example_com/state
```

State payloads are plain text:

```text
pass
```

or, if any DMARC result in the group failed:

```text
12.5% failed
```

The per-reporter sensor topic aggregates all pass/fail results for one `org_name + domain` pair in the current poll.
The domain topic aggregates all reporters for one domain in the current poll.
A `pass` state means the app saw no `fail` results in that group.
Percentage states are calculated as `failed results / all pass-or-fail results * 100`, rounded to one decimal place.

The app also publishes retained Home Assistant MQTT discovery messages:

```text
homeassistant/sensor/dmarc2mqtt_<reporter>_<domain>/config
homeassistant/sensor/dmarc2mqtt_domain_<domain>/config
```

Example discovery payload:

```json
{
  "name": "DMARC Google Inc. example.com",
  "unique_id": "dmarc2mqtt_google_inc_example_com",
  "state_topic": "mail/dmarc/sensors/google_inc_example_com/state",
  "icon": "mdi:email-check-outline",
  "object_id": "dmarc2mqtt_google_inc_example_com"
}
```

Discovery is also published for tuples stored in `history.json`, so Home Assistant sensors remain defined even when a later poll has no new report for that tuple.
