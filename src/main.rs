use librespot::core::mercury::MercuryError;
use librespot::playback::player::PlayerEvent;
use serenity::all::{ActivityData, FullEvent, OnlineStatus};
use serenity::model::id;
use tokio::select;

mod config;
mod discord;
mod spotify;

struct BotState {
    current_vc: Option<(id::GuildId, id::ChannelId)>,
}

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;

    let config = config::load_config().await?;

    let mut spotify = spotify::Spotify::spawn(config.clone()).await;
    let mut discord = discord::Discord::spawn(config.clone(), spotify.sink_receiver).await?;

    let mut state = BotState { current_vc: None };

    loop {
        select! {
            event = spotify.player_events.recv() => {
                if let Some(e) = event {
                    match e {
                        PlayerEvent::Started {
                            ..
                        } => {
                            let current_vc = discord.user_channel(config.discord_user.clone().into()).await;
                            if let Some((guild_id, channel_id)) = current_vc {
                                discord.join_vc(guild_id, channel_id).await;
                                state.current_vc = current_vc;
                            }
                        }

                        PlayerEvent::Stopped {
                            ..
                        } => {
                            if let Some((guild_id, _)) = state.current_vc {
                                discord.leave_vc(guild_id).await;
                                discord.ctx.set_presence(
                                    None,
                                    OnlineStatus::Online,
                                );
                                state.current_vc = None;
                            }
                        }

                        PlayerEvent::Paused { .. } => {
                            discord.ctx.set_presence(
                                None,
                                OnlineStatus::Online,
                            )
                        }

                        PlayerEvent::Playing { track_id, .. } => {
                            let track: Result<librespot::metadata::Track, MercuryError> =
                                librespot::metadata::Metadata::get(
                                    &spotify.session,
                                    track_id,
                                )
                                .await;

                            if let Ok(track) = track {
                                let artist: Result<librespot::metadata::Artist, MercuryError> =
                                    librespot::metadata::Metadata::get(
                                        &spotify.session,
                                        *track.artists.first().unwrap(),
                                    )
                                    .await;

                                if let Ok(artist) = artist {
                                    discord.ctx.set_presence(
                                        Some(ActivityData::listening(format!("{}: {}", artist.name, track.name))),
                                        OnlineStatus::Online,
                                    )
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            event = discord.event_rx.recv() => {
                if let Some((ctx, event)) = event {
                    async {
                        match event {
                            FullEvent::VoiceStateUpdate { old, new } => {
                                if state.current_vc.is_none() {
                                    return;
                                }

                                if new.user_id != config.discord_user.clone() {
                                    return;
                                }

                                if old.is_none() {
                                    discord.join_vc(new.guild_id.unwrap(), new.channel_id.unwrap()).await;
                                    return;
                                }

                                let old_state = old.unwrap();

                                if old_state.channel_id.is_some() && new.channel_id.is_none() {
                                    discord.leave_vc(new.guild_id.unwrap()).await;
                                    return;
                                }

                                if old_state.channel_id.unwrap() != new.channel_id.unwrap() {
                                    discord.join_vc(new.guild_id.unwrap(), new.channel_id.unwrap()).await;
                                    return;
                                }
                            }
                            _ => {}
                        };
                    }.await;
                }
            }
        }
    }
}
