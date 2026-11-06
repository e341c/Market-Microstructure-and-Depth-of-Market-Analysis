use dotenv::dotenv;
use envconfig::Envconfig;
use std::sync::OnceLock;

#[derive(Envconfig, Clone)]
pub struct Config {
    #[envconfig(from = "BINANCE_URL")]
    pub binance_url: String,

    #[envconfig(from = "BINANCE_WS_URL")]
    pub binance_ws_url: String,
}

pub static CONFIG: OnceLock<Config> = OnceLock::new();

pub fn get_config() -> &'static Config {
    CONFIG.get_or_init(|| {
        let _ = dotenv();

        Config::init_from_env().expect("error config initializtion from enviroment")
    })
}
