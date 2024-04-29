use std::{
    collections::VecDeque,
    io::{self, Cursor, ErrorKind, Read},
    sync::{mpsc::SyncSender, Arc, Mutex},
};

use crate::config::BotConfig;
use byteorder::{ByteOrder, LittleEndian};
use librespot::{
    connect::spirc::Spirc,
    core::{
        authentication::Credentials,
        cache::Cache,
        config::{ConnectConfig, SessionConfig},
        session::Session,
    },
    discovery::DeviceType,
    playback::{
        audio_backend::{self, SinkResult},
        config::{Bitrate, PlayerConfig, VolumeCtrl},
        convert::Converter,
        decoder::AudioPacket,
        mixer::{softmixer::SoftMixer, Mixer, MixerConfig},
        player::{Player, PlayerEvent},
    },
};
use songbird::input::core::io::MediaSource;
use tokio::{sync::mpsc::UnboundedReceiver, task::JoinHandle};

struct EmittedSink {
    sender: SyncSender<Vec<f32>>,
}

impl EmittedSink {
    pub fn new(sender: SyncSender<Vec<f32>>) -> Self {
        EmittedSink { sender }
    }
}

impl audio_backend::Sink for EmittedSink {
    fn start(&mut self) -> SinkResult<()> {
        Ok(())
    }
    fn stop(&mut self) -> SinkResult<()> {
        println!("STOPPPPPPPPP");
        Ok(())
    }
    fn write(&mut self, packet: AudioPacket, _: &mut Converter) -> SinkResult<()> {
        let samples = packet.samples().unwrap();

        let _ = self
            .sender
            .send(samples.into_iter().map(|&v| v as f32).collect());

        Ok(())
    }
}

pub struct MyMediaSource {
    receiver: Arc<Mutex<std::sync::mpsc::Receiver<Vec<f32>>>>,
    spotify_buff: Cursor<Vec<u8>>,
}

impl MyMediaSource {
    pub fn new(receiver: Arc<Mutex<std::sync::mpsc::Receiver<Vec<f32>>>>) -> Self {
        MyMediaSource {
            receiver,
            spotify_buff: Cursor::new(Vec::with_capacity(1024)),
        }
    }
}

impl MediaSource for MyMediaSource {
    fn is_seekable(&self) -> bool {
        false
    }

    fn byte_len(&self) -> Option<u64> {
        None
    }
}

impl std::io::Seek for MyMediaSource {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        unreachable!()
    }
}

impl std::io::Read for MyMediaSource {
    fn read(&mut self, buff: &mut [u8]) -> std::io::Result<usize> {
        println!("{:?}", buff.len());

        let sample_size = std::mem::size_of::<f32>();

        if buff.len() < sample_size * 2 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "EmittedSink does not support read buffer too small to guarantee \
                holding one audio sample (8 bytes)",
            ));
        }

        let mut out_cursor = Cursor::new(buff);

        fn is_cursor_drained(cursor: &Cursor<Vec<u8>>) -> bool {
            cursor.position() >= cursor.get_ref().len() as u64
        }

        if is_cursor_drained(&self.spotify_buff) {
            let received = self.receiver.lock().unwrap().recv().unwrap();
            // Make sure that spot buff is big enough to hold received bytes
            let received_bytes = received.len() * sample_size;
            let spot_buff_size = self.spotify_buff.get_ref().len();
            self.spotify_buff
                .get_mut()
                .resize(usize::max(received_bytes, spot_buff_size), 0);
            let spot_buff_size = self.spotify_buff.get_ref().len();
            debug_assert!(spot_buff_size >= received_bytes);

            // We create a slice that is the same size as the received length on the right most section of spotify buff
            let spot_buff_start_idx = spot_buff_size - received_bytes;
            let spot_buff_sliced = &mut self.spotify_buff.get_mut()[spot_buff_start_idx..];
            debug_assert_eq!(received_bytes, spot_buff_sliced.len());

            LittleEndian::write_f32_into(received.as_slice(), spot_buff_sliced);

            self.spotify_buff.set_position(spot_buff_start_idx as u64);
        }

        let mut total_bytes_written = 0;

        while !is_cursor_drained(&self.spotify_buff) {
            let out_pos = out_cursor.position();
            let sliced_buff = &mut out_cursor.get_mut()[out_pos as usize..];
            let bytes_written = self.spotify_buff.read(sliced_buff).expect("IMPOSSIBLE");
            if bytes_written == 0 {
                debug_assert_eq!(total_bytes_written, out_cursor.get_ref().len());
                return Ok(total_bytes_written);
            }
            out_cursor.set_position(out_pos + bytes_written as u64);
            debug_assert!(bytes_written > 0);
            total_bytes_written += bytes_written;
        }

        Ok(total_bytes_written)
    }
}

pub struct Spotify {
    pub spirc_task_handle: JoinHandle<()>,
    pub spirc: Spirc,
    pub player_events: UnboundedReceiver<PlayerEvent>,
    pub session: Session,
    pub sink_receiver: Arc<Mutex<std::sync::mpsc::Receiver<Vec<f32>>>>,
}

impl Spotify {
    pub async fn spawn(config: BotConfig) -> Spotify {
        let session_config = SessionConfig::default();
        let player_config = PlayerConfig {
            bitrate: Bitrate::Bitrate320,
            ..Default::default()
        };

        let credentials = Credentials::with_password(
            config.spotify_username.clone(),
            config.spotify_password.clone(),
        );

        let mut cache_limit: u64 = 10;
        cache_limit = cache_limit.pow(9);
        cache_limit *= 4;

        let cache_dir = Some(config.cache_dir);

        let cache = Cache::new(
            cache_dir.clone(),
            cache_dir.clone(),
            cache_dir.clone(),
            Some(cache_limit),
        )
        .ok();

        let (session, _creds) = Session::connect(session_config, credentials, cache, false)
            .await
            .unwrap();

        let mixer = Box::new(SoftMixer::open(MixerConfig {
            volume_ctrl: VolumeCtrl::Linear,
            ..MixerConfig::default()
        }));

        let (sender, receiver) = std::sync::mpsc::sync_channel::<Vec<f32>>(0);
        let sink = EmittedSink::new(sender);

        let (player, player_events) = Player::new(
            player_config,
            session.clone(),
            mixer.get_soft_volume(),
            move || Box::new(sink),
        );

        let (spirc, spirc_task) = Spirc::new(
            ConnectConfig {
                name: "Biggus Dickus".to_string(),
                device_type: DeviceType::Speaker,
                initial_volume: None,
                has_volume_ctrl: true,
                autoplay: true,
            },
            session.clone(),
            player,
            mixer,
        );

        let spirc_task_handle = tokio::spawn(spirc_task);

        Spotify {
            session: session.clone(),
            player_events,
            spirc,
            spirc_task_handle,
            sink_receiver: Arc::new(Mutex::new(receiver)),
        }
    }
}
