mod assistant;
mod discord;
mod listener;
mod sound_store;
mod agent_speaker;

use std::{collections::HashMap, sync::Arc};
use dotenv::dotenv;
use tokio::sync::Mutex;

#[tokio::main(flavor = "multi_thread", worker_threads = 10)]
async fn main() {
    println!("Hello, world!");
    dotenv().ok();
    
    let mut client = discord::init_serenity().await;

    {
        // Initialize shared state.
        let mut guard: tokio::sync::RwLockWriteGuard<'_, serenity::prelude::TypeMap> = client.data.write().await;
        guard.insert::<discord::SharedState>(discord::SharedState {
            users: HashMap::default(),
            id_to_ssrc: HashMap::default(),
            tx_audio: None,
            assistant: Arc::new(Mutex::new(assistant::DiscordAssistant::new().await))
        });
    };

    let sound_store = sound_store::init_sound_store().await;
    {
        let mut data = client.data.write().await;
        data.insert::<sound_store::SoundStore>(std::sync::Arc::new(std::sync::Mutex::new(sound_store)));
    }

    let discord_handle = tokio::spawn(async move {
        client.start().await.expect("Error in serenity client!");
    });

    discord_handle.await.unwrap();
}