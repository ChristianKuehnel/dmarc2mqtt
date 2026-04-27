use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use crate::AggregatedStatus;
use crate::DomainStatus;
use crate::config::AppConfig;
use crate::history::KnownTuple;

#[derive(Serialize)]
struct HomeAssistantSensorConfig {
    name: String,
    unique_id: String,
    state_topic: String,
    icon: String,
    object_id: String,
}

pub(crate) fn publish_reports_to_mqtt(
    config: &AppConfig,
    aggregated_statuses: &[AggregatedStatus],
    domain_statuses: &[DomainStatus],
    known_tuples: &[KnownTuple],
) -> Result<(), String> {
    let mqtt_options = mqtt_options_from_config(config);

    let (client, mut connection) = rumqttc::Client::new(mqtt_options, 20);
    let running = Arc::new(AtomicBool::new(true));
    let connection_error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let running_bg = Arc::clone(&running);
    let connection_error_bg = Arc::clone(&connection_error);

    let network_thread = thread::spawn(move || {
        for notification in connection.iter() {
            if !running_bg.load(Ordering::Relaxed) {
                break;
            }
            if let Err(err) = notification {
                if let Ok(mut slot) = connection_error_bg.lock() {
                    *slot = Some(err.to_string());
                }
                break;
            }
        }
    });

    let mut known_sensor_keys: BTreeMap<(String, String), (String, String)> = BTreeMap::new();
    for item in known_tuples {
        known_sensor_keys.insert(
            (item.org_name.clone(), item.domain.clone()),
            (item.org_name.clone(), item.domain.clone()),
        );
    }
    for item in aggregated_statuses {
        known_sensor_keys.insert(
            (item.org_name.clone(), item.domain.clone()),
            (item.org_name.clone(), item.domain.clone()),
        );
    }

    for (_, (org_name, domain)) in known_sensor_keys {
        let object_suffix = slugify(&format!("{org_name}_{domain}"));
        let object_id = format!("dmarc2mqtt_{object_suffix}");
        let discovery_topic = format!("homeassistant/sensor/{object_id}/config");
        let state_topic = format!("{}/sensors/{object_suffix}/state", config.mqtt.base_topic);

        let config_payload = serde_json::to_vec(&HomeAssistantSensorConfig {
            name: format!("DMARC {org_name} {domain}"),
            unique_id: object_id.clone(),
            state_topic,
            icon: "mdi:email-check-outline".to_owned(),
            object_id,
        })
        .map_err(|err| format!("Failed to serialize discovery payload: {err}"))?;
        client
            .publish(discovery_topic, rumqttc::QoS::AtLeastOnce, true, config_payload)
            .map_err(|err| format!("Failed to publish Home Assistant discovery: {err}"))?;
    }

    for item in aggregated_statuses {
        let object_suffix = slugify(&format!("{}_{}", item.org_name, item.domain));
        let state_topic = format!("{}/sensors/{}/state", config.mqtt.base_topic, object_suffix);
        client
            .publish(
                state_topic,
                rumqttc::QoS::AtLeastOnce,
                true,
                item.status.clone(),
            )
            .map_err(|err| format!("Failed to publish sensor state: {err}"))?;
    }

    let mut known_domains: BTreeSet<String> = BTreeSet::new();
    for item in known_tuples {
        known_domains.insert(item.domain.clone());
    }
    for item in domain_statuses {
        known_domains.insert(item.domain.clone());
    }

    for domain in known_domains {
        let object_suffix = slugify(&domain);
        let object_id = format!("dmarc2mqtt_domain_{object_suffix}");
        let discovery_topic = format!("homeassistant/sensor/{object_id}/config");
        let state_topic = format!("{}/domains/{object_suffix}/state", config.mqtt.base_topic);

        let config_payload = serde_json::to_vec(&HomeAssistantSensorConfig {
            name: format!("DMARC Domain {domain}"),
            unique_id: object_id.clone(),
            state_topic,
            icon: "mdi:shield-check-outline".to_owned(),
            object_id,
        })
        .map_err(|err| format!("Failed to serialize domain discovery payload: {err}"))?;
        client
            .publish(discovery_topic, rumqttc::QoS::AtLeastOnce, true, config_payload)
            .map_err(|err| format!("Failed to publish domain discovery: {err}"))?;
    }

    for item in domain_statuses {
        let object_suffix = slugify(&item.domain);
        let state_topic = format!("{}/domains/{object_suffix}/state", config.mqtt.base_topic);
        client
            .publish(
                state_topic,
                rumqttc::QoS::AtLeastOnce,
                true,
                item.status.clone(),
            )
            .map_err(|err| format!("Failed to publish domain state: {err}"))?;
    }

    thread::sleep(Duration::from_millis(300));
    let _ = client.disconnect();
    running.store(false, Ordering::Relaxed);
    let _ = network_thread.join();

    if let Ok(slot) = connection_error.lock() {
        if let Some(err) = &*slot {
            return Err(format!("MQTT connection error: {err}"));
        }
    }

    Ok(())
}

fn mqtt_options_from_config(config: &AppConfig) -> rumqttc::MqttOptions {
    let mut mqtt_options = rumqttc::MqttOptions::new(
        "dmarc2mqtt",
        config.mqtt.server_name.clone(),
        config.mqtt.server_port,
    );
    if config.mqtt.tls {
        mqtt_options.set_transport(rumqttc::Transport::tls_with_default_config());
    }
    mqtt_options.set_credentials(config.mqtt.login.clone(), config.mqtt.password.clone());
    mqtt_options.set_keep_alive(Duration::from_secs(10));
    mqtt_options
}

fn slugify(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut prev_underscore = false;
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() {
            output.push(ch.to_ascii_lowercase());
            prev_underscore = false;
        } else if !prev_underscore {
            output.push('_');
            prev_underscore = true;
        }
    }

    let trimmed = output.trim_matches('_').to_owned();
    if trimmed.is_empty() {
        "unknown".to_owned()
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ImapConfig, ImapMailboxConfig, MqttConfig};

    fn config(mqtt: MqttConfig) -> AppConfig {
        AppConfig {
            mqtt,
            imap: ImapConfig {
                mailboxes: vec![ImapMailboxConfig {
                    name: "primary".to_owned(),
                    server_name: "imap.example.com".to_owned(),
                    server_port: 993,
                    login: "user@example.com".to_owned(),
                    password: "change-me".to_owned(),
                    report_folder: "INBOX/DMARC".to_owned(),
                    trash_folder: "Trash".to_owned(),
                    move_emails: false,
                    poll_cron: "0 0 */6 * * *".to_owned(),
                    max_xml_size: 10,
                    max_message_size: 25,
                    max_attachment_size: 10,
                }],
            },
        }
    }

    fn mqtt_config(tls: bool) -> MqttConfig {
        MqttConfig {
            server_name: "mqtt.example.local".to_owned(),
            server_port: if tls { 8883 } else { 1883 },
            login: "dmarc2mqtt".to_owned(),
            password: "change-me".to_owned(),
            base_topic: "mail/dmarc".to_owned(),
            tls,
            allow_insecure: !tls,
            remove_stale_sensors: None,
        }
    }

    #[test]
    fn tls_config_selects_tls_transport() {
        let options = mqtt_options_from_config(&config(mqtt_config(true)));

        assert!(matches!(options.transport(), rumqttc::Transport::Tls(_)));
    }

    #[test]
    fn explicit_insecure_config_selects_tcp_transport() {
        let options = mqtt_options_from_config(&config(mqtt_config(false)));

        assert!(matches!(options.transport(), rumqttc::Transport::Tcp));
    }
}
