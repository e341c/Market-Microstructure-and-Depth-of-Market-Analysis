use dotenv::dotenv;
use envconfig::Envconfig;
use std::sync::OnceLock;

#[derive(Envconfig, Clone)]
pub struct Config {
    #[envconfig(from = "DEPTH")]
    pub depth: u32,

    #[envconfig(from = "HEARTBEAT_SECS")]
    pub heartbeat_secs: u32,

    #[envconfig(from = "BINANCE_URL")]
    pub binance_url: String,

    #[envconfig(from = "BINANCE_WS_URL")]
    pub binance_ws_url: String,

    #[envconfig(from = "ADDR")]
    pub addr: String,

    #[envconfig(from = "API_KEY")]
    pub api_key: String,

    #[envconfig(from = "PEM_PATH")]
    pub pem_path: String,

    #[envconfig(from = "SYMBOL")]
    pub symbol: String,

    #[envconfig(from = "METRICS_FILE_PATH")]
    pub metrics_file_path: String,
}

pub static CONFIG: OnceLock<Config> = OnceLock::new();

pub fn get_config() -> &'static Config {
    CONFIG.get_or_init(|| {
        let _ = dotenv();

        Config::init_from_env().expect("error config initializtion from enviroment")
    })
}
