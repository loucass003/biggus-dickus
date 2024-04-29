use std::sync::{Arc, Mutex};

use crate::{
    config::BotConfig,
    spotify::{self},
};
use color_eyre::eyre::Context as _;
use librespot::playback::SAMPLE_RATE;
use serenity::{
    all::{Context, Framework, FullEvent, GatewayIntents},
    async_trait,
    model::id,
    Client,
};
use songbird::{
    events::{Event, EventContext, EventHandler as VoiceEventHandler, TrackEvent},
    input::{codecs::RawReader, core::codecs::CodecRegistry, Input},
    Songbird,
};
use songbird::{input::RawAdapter, SerenityInit};
use symphonia::core::probe::Probe;

struct TrackErrorNotifier;

#[async_trait]
impl VoiceEventHandler for TrackErrorNotifier {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        if let EventContext::Track(track_list) = ctx {
            for (state, handle) in *track_list {
                println!(
                    "Track {:?} encountered an error: {:?}",
                    handle.uuid(),
                    state.playing
                );
            }
        }

        None
    }
}

struct Fr {
    evt_tx: tokio::sync::mpsc::Sender<(Context, FullEvent)>,
}
impl Fr {
    fn new() -> (Self, tokio::sync::mpsc::Receiver<(Context, FullEvent)>) {
        let (evt_tx, rx) = tokio::sync::mpsc::channel(16);
        (Fr { evt_tx: evt_tx }, rx)
    }
}

#[async_trait]
impl Framework for Fr {
    async fn dispatch(&self, ctx: Context, event: FullEvent) {
        self.evt_tx
            .send((ctx, event))
            .await
            .expect("Receiver should never be closed");
    }
}

pub struct Discord {
    pub ctx: Context,
    pub event_rx: tokio::sync::mpsc::Receiver<(Context, FullEvent)>,
    manager: Arc<Songbird>,
    spotify_sink_receiver: Arc<Mutex<std::sync::mpsc::Receiver<Vec<f32>>>>,
}

impl Discord {
    pub async fn spawn(
        config: BotConfig,
        spotify_sink_receiver: Arc<Mutex<std::sync::mpsc::Receiver<Vec<f32>>>>,
    ) -> color_eyre::Result<Self> {
        let token = config.discord_token;
        let intents = GatewayIntents::non_privileged();

        let (framework, mut event_rx) = Fr::new();

        let mut client: Client = Client::builder(&token, intents)
            .framework(framework)
            .register_songbird()
            .await
            .wrap_err("Err creating client")?;

        tokio::spawn(async move {
            let _ = client
                .start()
                .await
                .wrap_err("Unable to start discord client");
        });

        let ctx = {
            loop {
                if let Some((ctx, event)) = event_rx.recv().await {
                    match event {
                        FullEvent::Ready { .. } => {
                            break ctx;
                        }
                        _ => {}
                    }
                }
            }
        };

        let manager = songbird::get(&ctx)
            .await
            .expect("Songbird Voice client placed in at initialisation.")
            .clone();

        println!("Ready!");
        println!("Invite me with https://discord.com/api/oauth2/authorize?client_id={}&permissions=36700160&scope=bot", ctx.cache.current_user().id);

        Ok(Discord {
            ctx,
            event_rx,
            manager,
            spotify_sink_receiver,
        })
    }

    pub async fn leave_vc(&self, guild_id: id::GuildId) {
        let _ = self.manager.remove(guild_id).await;
    }

    pub async fn join_vc(&self, guild_id: id::GuildId, channel_id: id::ChannelId) {
        match self.manager.join(guild_id, channel_id).await {
            Ok(lock) => {
                let mut handler = lock.lock().await;
                handler.add_global_event(TrackEvent::Error.into(), TrackErrorNotifier);

                let audio_source = spotify::MyMediaSource::new(self.spotify_sink_receiver.clone());

                let mut codecs = CodecRegistry::new();
                codecs.register_all::<symphonia::default::codecs::PcmDecoder>();

                let mut probe = Probe::default();
                probe.register_all::<RawReader>();

                let raw_adapter = RawAdapter::new(audio_source, SAMPLE_RATE as _, 2);
                let input: Input = songbird::input::Input::from(raw_adapter)
                    .make_playable(&codecs, &probe, &tokio::runtime::Handle::current())
                    .expect("FAILED TO PROMOTE");
                handler.set_bitrate(songbird::driver::Bitrate::Auto);
                handler.play_only_input(input);
            }
            Err(e) => {
                panic!("unable to send")
            }
        }
    }

    pub async fn user_channel(&self, user_id: id::UserId) -> Option<(id::GuildId, id::ChannelId)> {
        for guild_id in self.ctx.cache.guilds() {
            let channel_id = self
                .ctx
                .cache
                .guild(guild_id)
                .expect("Guild Should exist")
                .voice_states
                .get(&user_id.into())
                .and_then(|voice_state| voice_state.channel_id);

            if channel_id.is_some() {
                return Some((guild_id, channel_id.unwrap()));
            }
        }
        None
    }
}
