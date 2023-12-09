mod assistant;
mod discord;
mod listener;
mod sound_store;
mod agent_speaker;
mod resampler;

use std::{collections::HashMap, sync::Arc};
use async_openai::Client;
use dotenv::dotenv;

#[tokio::main(flavor = "multi_thread", worker_threads = 10)]
async fn main() {
    println!("Hello, world!");
    dotenv().ok();
    
    let mut client = discord::init_serenity().await;

    let sound_store = sound_store::init_sound_store().await;
    {
        // Initialize shared state.
        let mut guard: tokio::sync::RwLockWriteGuard<'_, serenity::prelude::TypeMap> = client.data.write().await;
        guard.insert::<discord::SharedState>(discord::SharedState {
            users: HashMap::default(),
            id_to_ssrc: HashMap::default(),
            oai_client: Arc::new(Client::new()),
            sound_store: std::sync::Arc::new(std::sync::Mutex::new(sound_store))
        });
    };

    // {
    //     let mut data = client.data.write().await;
    //     data.insert::<sound_store::SoundStore>(std::sync::Arc::new(std::sync::Mutex::new(sound_store)));
    // }

    let discord_handle = tokio::spawn(async move {
        client.start().await.expect("Error in serenity client!");
    });

    discord_handle.await.unwrap();
}