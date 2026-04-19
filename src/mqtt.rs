use rumqttd::{Broker, Config};
use std::thread;

pub fn start_broker() {
    thread::spawn(|| {
        let config_str = r#"
            id = 0
            [router]
                id = 0
                dir = "/tmp/rumqttd"
                max_connections = 10000
                max_outgoing_packet_count = 200
                max_segment_size = 104857600
                max_segment_count = 10
            [v4.1]
                name = "mqtt-tcp"
                listen = "127.0.0.1:1883"
                next_connection_delay_ms = 1
                [v4.1.connections]
                    connection_timeout_ms = 100
                    max_client_id_len = 256
                    throttle_delay_ms = 0
                    max_payload_size = 51200
                    max_inflight_count = 100
                    max_inflight_size = 1024
            [ws.1]
                name = "mqtt-ws"
                listen = "127.0.0.1:8081"
                next_connection_delay_ms = 1
                [ws.1.connections]
                    connection_timeout_ms = 100
                    max_client_id_len = 256
                    throttle_delay_ms = 0
                    max_payload_size = 51200
                    max_inflight_count = 100
                    max_inflight_size = 1024
        "#;

        let config: Config = toml::from_str(config_str).unwrap();
        let mut broker = Broker::new(config);
        broker.start().unwrap();
    });
}

#[test]
fn print_default_config() {
    let c = rumqttd::Config::default();
    println!("{}", toml::to_string(&c).unwrap());
}

#[test]
fn test_broker_start() {
    let config_str = r#"
            id = 0
            [router]
                id = 0
                dir = "/tmp/rumqttd"
                max_connections = 10000
                max_outgoing_packet_count = 200
                max_segment_size = 104857600
                max_segment_count = 10
            [v4.1]
                name = "mqtt-tcp"
                listen = "127.0.0.1:1883"
                next_connection_delay_ms = 1
                [v4.1.connections]
                    connection_timeout_ms = 100
                    max_client_id_len = 256
                    throttle_delay_ms = 0
                    max_payload_size = 51200
                    max_inflight_count = 100
                    max_inflight_size = 1024
            [ws.1]
                name = "mqtt-ws"
                listen = "127.0.0.1:8081"
                next_connection_delay_ms = 1
                [ws.1.connections]
                    connection_timeout_ms = 100
                    max_client_id_len = 256
                    throttle_delay_ms = 0
                    max_payload_size = 51200
                    max_inflight_count = 100
                    max_inflight_size = 1024
        "#;

    let config: rumqttd::Config = toml::from_str(config_str).unwrap();
    println!("Config parsed successfully.");
    let mut broker = rumqttd::Broker::new(config);
    println!("Broker created.");
    std::thread::spawn(move || {
        broker.start().unwrap();
    });
    std::thread::sleep(std::time::Duration::from_secs(2));
}
