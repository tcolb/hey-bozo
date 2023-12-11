use std::sync::{Arc, Mutex as SyncMutex};

use async_openai::Client as OpenAIClient;
use async_openai::config::OpenAIConfig;
use async_openai::types::{CreateSpeechRequestArgs, Voice, SpeechModel};
use songbird::{ffmpeg, create_player};
use serenity::model::id::GuildId;
use songbird::Songbird;
use songbird::tracks::{TrackHandle, PlayMode, LoopState};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::sound_store::SoundStore;

pub struct AgentSpeaker {
    songbird: Arc<Songbird>,
    guild_id: GuildId,
    oai_client: Arc<OpenAIClient<OpenAIConfig>>,
    task: Option<JoinHandle<()>>,
    track_handle: Arc<Mutex<Option<TrackHandle>>>,
    sound_store: Arc<SyncMutex<SoundStore>>
}

impl AgentSpeaker {
    pub fn new(songbird: Arc<Songbird>, guild_id: GuildId, sound_store: Arc<SyncMutex<SoundStore>>) -> Self {
        AgentSpeaker {
            songbird: songbird,
            guild_id: guild_id,
            oai_client: Arc::new(OpenAIClient::new()),
            task: None,
            track_handle: Arc::new(Mutex::new(None)),
            sound_store: sound_store
        }
    }

    pub fn speak(&mut self, text: &str) {
        let voice_request: async_openai::types::CreateSpeechRequest = CreateSpeechRequestArgs::default()
            .input(text)
            .voice(Voice::Onyx)
            .model(SpeechModel::Tts1)
            .build().unwrap();

        let speaker_handle_lock = self.track_handle.clone();
        let songbird_lock = self.songbird.get(self.guild_id.clone()).unwrap();
        let oai_client = self.oai_client.clone();
        self.task = Some(tokio::spawn(async move {
            match oai_client.audio().speech(voice_request).await {
                Ok(speech) => {
                    let path = format!("../../tmp/{}.mp3", Uuid::new_v4());
                    speech.save(path.clone()).await.unwrap();

                    let mut songbird_guard = songbird_lock.lock().await;
                    let new_track_handle = songbird_guard.play_only_source(ffmpeg(path).await.unwrap());
                    let mut handle_guard = speaker_handle_lock.lock().await;
                    *handle_guard = Some(new_track_handle);
                },
                Err(e) => {
                    println!("{}", e.to_string());
                }
            }
        }));
    }

    pub async fn acknowledge(&self) {
        let songbird_lock = self.songbird.get(self.guild_id.clone()).unwrap();
        let mut songbird_guard = songbird_lock.lock().await;

        if let Ok(song_store) = self.sound_store.lock() {
            if let Some(memory) = song_store.get("acknowledge") {
                songbird_guard.play_only_source(memory.new_handle().try_into().unwrap());
            }
        }
    }

    pub async fn start_ping(&self) {
        let songbird_lock = self.songbird.get(self.guild_id.clone()).unwrap();
        let mut songbird_guard = songbird_lock.lock().await;

        if let Ok(song_store) = self.sound_store.lock() {
            if let Some(memory) = song_store.get("ping") {
                let (mut track, _handle) = create_player(memory.new_handle().try_into().unwrap());
                track.set_loops(LoopState::Infinite).unwrap();
                songbird_guard.play_only(track);
            }
        }
    }

    pub async fn is_speaking(&self) -> bool {
        let track_guard = self.track_handle.lock().await;
        if let Some(handle) = track_guard.as_ref() {
            if let Ok(info) = handle.get_info().await {
                return info.playing == PlayMode::End;
            }
        }
        return false;
    }
}