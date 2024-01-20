use std::sync::{Arc, Mutex as SyncMutex};

use async_openai::config::OpenAIConfig;
use async_openai::types::{CreateSpeechRequestArgs, SpeechModel, Voice};
use async_openai::Client as OpenAIClient;
use songbird::id::GuildId;
use songbird::input::File;
use songbird::tracks::{LoopState, PlayMode, Track, TrackHandle};
use songbird::Songbird;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::sound_store::SoundStore;

pub struct AgentSpeaker {
    songbird: Arc<Songbird>,
    guild_id: GuildId,
    oai_client: Arc<OpenAIClient<OpenAIConfig>>,
    track_handle: Arc<Mutex<Option<TrackHandle>>>,
    sound_store: Arc<SyncMutex<SoundStore>>,
}

impl AgentSpeaker {
    pub fn new(
        songbird: Arc<Songbird>,
        guild_id: GuildId,
        sound_store: Arc<SyncMutex<SoundStore>>,
    ) -> Self {
        AgentSpeaker {
            songbird: songbird,
            guild_id: guild_id,
            oai_client: Arc::new(OpenAIClient::new()),
            track_handle: Arc::new(Mutex::new(None)),
            sound_store: sound_store,
        }
    }

    pub async fn speak(&mut self, text: &str) {
        let voice_request: async_openai::types::CreateSpeechRequest =
            CreateSpeechRequestArgs::default()
                .input(text)
                .voice(Voice::Onyx)
                .model(SpeechModel::Tts1)
                .build()
                .unwrap();

        let speaker_handle_lock = self.track_handle.clone();
        let songbird_lock = self.songbird.get(self.guild_id.clone()).unwrap();
        let oai_client = self.oai_client.clone();
        match oai_client.audio().speech(voice_request).await {
            Ok(speech) => {
                let path = format!("../../tmp/{}.mp3", Uuid::new_v4());
                speech.save(path.clone()).await.unwrap();

                let mut songbird_guard = songbird_lock.lock().await;
                let source = File::new(path);
                let new_track_handle = songbird_guard.play_only_input(source.into());
                let mut handle_guard = speaker_handle_lock.lock().await;
                *handle_guard = Some(new_track_handle);
            }
            Err(e) => {
                println!("{}", e.to_string());
            }
        }
    }

    pub async fn acknowledge(&self) {
        let songbird_lock = self.songbird.get(self.guild_id.clone()).unwrap();
        let mut songbird_guard = songbird_lock.lock().await;

        if let Ok(song_store) = self.sound_store.lock() {
            if let Some(memory) = song_store.get("acknowledge") {
                songbird_guard.play_only(memory.new_handle().try_into().unwrap());
            }
        }
    }

    pub async fn start_ping(&self) {
        let songbird_lock = self.songbird.get(self.guild_id.clone()).unwrap();
        let mut songbird_guard = songbird_lock.lock().await;

        if let Ok(song_store) = self.sound_store.lock() {
            if let Some(memory) = song_store.get("ping") {
                let track = Track::new(memory.new_handle().into()).loops(LoopState::Infinite);
                songbird_guard.play_only(track);
            }
        }
    }

    pub async fn is_finished(&self) -> bool {
        let track_guard = self.track_handle.lock().await;
        if let Some(handle) = track_guard.as_ref() {
            if let Ok(info) = handle.get_info().await {
                return info.playing == PlayMode::End;
            }
        }
        return true;
    }

    pub async fn stop(&self) {
        let songbird_lock = self.songbird.get(self.guild_id.clone()).unwrap();
        let mut songbird_guard = songbird_lock.lock().await;
        songbird_guard.stop();
    }
}
