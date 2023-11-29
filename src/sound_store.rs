use std::{sync::{Arc, Mutex}, collections::HashMap};

use songbird::{input::{cached::Memory}, ffmpeg, typemap::TypeMapKey};

pub struct SoundStore;

impl TypeMapKey for SoundStore {
    type Value = Arc<Mutex<HashMap<String, Memory>>>;
}

pub async fn init_sound_store() -> HashMap<String, Memory> {
    let mut audio_map = HashMap::new();

    let ting_src = Memory::new(
        ffmpeg("D:/Dev/hey-bozo/resources/openai_onyx_what_loser.mp3").await.unwrap(),
    ).unwrap();
    let _ = ting_src.raw.spawn_loader();
    audio_map.insert("acknowledge".into(), ting_src);

    audio_map
}

