use rumqttd::Config;
fn main() {
    let c = Config::default();
    println!("{}", toml::to_string(&c).unwrap());
}
