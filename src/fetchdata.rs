use crate::config::Config;

pub fn fetchdata(cfg: &Config) -> Result<String, reqwest::Error> {
    let url = format!("{}api/v3/depth?symbol=BTCUSDT&limit=5", cfg.binance_url);

    let raw_text = reqwest::blocking::get(url)?.text()?;

    println!("Первые 100 символов ответа:\n{}", &raw_text[..]);
    Ok(raw_text)
}
