use color_eyre::eyre::Context;
use serde::{Deserialize, Serialize};
use tokio::fs;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct BotConfig {
    pub discord_token: String,
    pub discord_user: u64,
    pub spotify_username: String,
    pub spotify_password: String,
    pub cache_dir: String,
}

pub async fn load_config() -> color_eyre::Result<BotConfig> {
    let filename = "config.toml";
    let contents = fs::read_to_string(filename)
        .await
        .wrap_err("Could not read file")?;

    toml::from_str(&contents).wrap_err("unable to load toml file")
}
