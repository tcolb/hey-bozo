use std::sync::{Arc, Mutex};

use async_openai::Client as OpenAIClient;
use async_openai::config::OpenAIConfig;
use async_openai::types::{CreateSpeechRequestArgs, Voice, SpeechModel};
use songbird::ffmpeg;
use serenity::model::id::GuildId;
use songbird::Songbird;
use songbird::tracks::{TrackHandle, PlayMode};
use tokio::task::JoinHandle;
use uuid::Uuid;

pub enum SpeechType {
    Acknowledge,
    Text(String)
}

pub struct AgentSpeaker {
    songbird: Arc<Songbird>,
    guild_id: GuildId,
    oai_client: Arc<OpenAIClient<OpenAIConfig>>,
    task: Option<JoinHandle<()>>,
    track_handle: Arc<Mutex<Option<TrackHandle>>>
}

impl AgentSpeaker {
    pub fn new(songbird: &mut Arc<Songbird>, guild_id: GuildId) -> Self {
        AgentSpeaker {
            songbird: songbird.clone(),
            guild_id: guild_id,
            oai_client: Arc::new(OpenAIClient::new()),
            task: None,
            track_handle: Arc::new(Mutex::new(None))
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
                    let new_track_handle = songbird_guard.play_source(ffmpeg(path).await.unwrap());
                    if let Ok(mut speaker_handle) = speaker_handle_lock.lock() {
                        *speaker_handle = Some(new_track_handle);
                    }
                },
                Err(e) => {
                    println!("{}", e.to_string());
                }
            }
        }));
    }

    pub async fn is_speaking(&self) -> bool {
        if let Ok(mut speaker_handle) = self.track_handle.lock() {
            if let Some(handle) = speaker_handle.as_mut() {
                if let Ok(info) = handle.get_info().await {
                    return info.playing == PlayMode::End;
                }
            }
        }
        return false;
    }

    pub async fn stop() {

    }
}