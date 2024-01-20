use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use songbird::{
    input::{cached::Memory, File},
    typemap::TypeMapKey,
};

pub type SoundStore = HashMap<String, Memory>;

pub struct SoundStoreKey {}

impl TypeMapKey for SoundStoreKey {
    type Value = Arc<Mutex<SoundStore>>;
}

pub async fn init_sound_store() -> HashMap<String, Memory> {
    let mut audio_map = HashMap::new();

    let acknowledge_src = Memory::new(File::new("../../resources/openai_onyx_huh.mp3").into())
        .await
        .unwrap();
    let _ = acknowledge_src.raw.spawn_loader();
    audio_map.insert("acknowledge".into(), acknowledge_src);

    let ping_src = Memory::new(File::new("../../resources/ping.mp3").into())
        .await
        .unwrap();
    let _ = ping_src.raw.spawn_loader();
    audio_map.insert("ping".into(), ping_src);

    audio_map
}
