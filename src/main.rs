use color_eyre::{eyre::{eyre, WrapErr}};
use futures::FutureExt;
use librespot::{
    connect::spirc::Spirc,
    core::{
        authentication::Credentials,
        config::{ConnectConfig, SessionConfig},
        session::Session,
    },
    discovery::DeviceType,
    playback::{
        audio_backend,
        config::{AudioFormat, PlayerConfig, VolumeCtrl},
        mixer::{softmixer::SoftMixer, Mixer, MixerConfig},
        player::{Player, PlayerEvent},
    },
};
use serde::{Deserialize, Serialize};
use serenity::{all::{Context, EventHandler, GatewayIntents, Ready}, async_trait, Client};
use tokio::{select, try_join};
use songbird::events::{Event, EventContext, EventHandler as VoiceEventHandler, TrackEvent};
use songbird::SerenityInit;


#[derive(Debug, Serialize, Deserialize)]
struct BotConfig {
    discord_token: Option<String>,
    spotify_username: Option<String>,
    spotify_password: Option<String>,
}

impl ::std::default::Default for BotConfig {
    fn default() -> Self {
        Self {
            discord_token: None,
            spotify_username: None,
            spotify_password: None,
        }
    }
}


struct Handler;

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, _: Context, ready: Ready) {
        println!("{} is connected!", ready.user.name);
    }
}


#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    let config: BotConfig = confy::load("discord_bot", "config")?;
    let file = confy::get_configuration_file_path("discord_bot", "config")?;
    println!("The configuration file path is: {:#?}", file);

    if config.discord_token.is_none() || config.spotify_password.is_none() || config.spotify_username.is_none() {
        return Err(eyre!("Config not set"))
    }

    let token = config.discord_token.expect("Expected a token in the environment");

    let intents = GatewayIntents::non_privileged() | GatewayIntents::MESSAGE_CONTENT;

    let mut client = Client::builder(&token, intents)
        .event_handler(Handler)
        .register_songbird()
        .await
        .expect("Err creating client");

    tokio::spawn(async move {
        let _ = client
            .start()
            .await
            .map_err(|why| println!("Client ended: {:?}", why));
    });

    let session_config = SessionConfig::default();
    let player_config = PlayerConfig::default();
    let audio_format = AudioFormat::default();

    let credentials = Credentials::with_password(config.spotify_username.unwrap(), config.spotify_password.unwrap());
    let backend = audio_backend::find(None).unwrap();

    println!("Connecting...");
    let (session, _creds) = Session::connect(session_config, credentials, None, false)
        .await
        .unwrap();
    let cloned_session = session.clone();

    let mixer = Box::new(SoftMixer::open(MixerConfig {
        volume_ctrl: VolumeCtrl::Linear,
        ..MixerConfig::default()
    }));

    let (player, mut player_events) =
        Player::new(player_config, session, mixer.get_soft_volume(), move || {
            backend(None, audio_format)
        });

    let config = ConnectConfig {
        name: "Biggus Dickus".to_string(),
        device_type: DeviceType::Speaker,
        initial_volume: None,
        has_volume_ctrl: true,
        autoplay: true,
    };

    let (spirc, task) = Spirc::new(config, cloned_session, player, mixer);

    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let rx = rx.fuse();

    let spirc_task = tokio::spawn(task);

    let player_events_task = tokio::spawn(async move {
        let recv_fut = async {
            while let Some(message) = player_events.recv().await {
                match message {
                    PlayerEvent::Playing {
                        play_request_id,
                        track_id,
                        position_ms,
                        duration_ms,
                    } => {
                        println!("PLAYING")
                    }
                    PlayerEvent::Paused {
                        play_request_id,
                        track_id,
                        position_ms,
                        duration_ms,
                    } => {
                        println!("PAUSED")
                    }
                    _ => {}
                }
            }
        };

        select! {
            _ = rx => {}
            _ = recv_fut => {}
        }
        Ok(())
    });

    match try_join!(
        spirc_task.map(|r| r.wrap_err("spirc task exited unexpectedly")),
        player_events_task.map(|r| r.wrap_err("player events task exited unexpectedly")?)
    ) {
        Err(e) => {
            spirc.shutdown();
            Err(e)
        }
        Ok(((), ())) => Ok(()),
    }
}
