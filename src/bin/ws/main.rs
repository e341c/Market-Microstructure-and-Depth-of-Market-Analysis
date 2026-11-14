mod client;

fn main() {
    let ws_url: &str = "wss://stream.binance.com:9443";
    client::run_client(ws_url);
}
