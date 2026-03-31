# dmarc2mqtt

Reads DMARC reports from ann IMAP mailbox and forwards a processed reports via MQTT to be integrated in home automation systems like home assistant.

## Feature Roadmap
1. read and validate xml 2.extract a few key results
2. Push to mqtt
3. publish HA sensor config
4. Read zipped files and validate zip security
5. read from imap and move to trash
6. add database, add statistics
7. add more values/sensors


## Design considerations

- Security: Build outside of home assistant to isolate alle the mail processing.
- Security: Use Rust as memory safe language.
- Interoperability: Export data using MQTT to allow integration in other services.
