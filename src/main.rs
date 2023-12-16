mod assistant;
mod discord;
mod listener;
mod sound_store;
mod agent_speaker;
mod resampler;
mod action_handler;

use std::{collections::HashMap, sync::Arc};
use async_openai::Client;
use dotenv::dotenv;
use tokio::sync::broadcast;

#[tokio::main(flavor = "multi_thread", worker_threads = 10)]
async fn main() {
    println!("Hello, world!");
    dotenv().ok();
    
    let mut client = discord::init_serenity().await;

    let (action_tx, _) = broadcast::channel(16);
    let sound_store = sound_store::init_sound_store().await;
    {
        // Initialize shared state.
        let mut guard: tokio::sync::RwLockWriteGuard<'_, serenity::prelude::TypeMap> = client.data.write().await;
        guard.insert::<discord::SharedState>(discord::SharedState {
            users: HashMap::default(),
            id_to_ssrc: HashMap::default(),
            oai_client: Arc::new(Client::new()),
            sound_store: std::sync::Arc::new(std::sync::Mutex::new(sound_store)),
            action_channel_tx: action_tx.clone()
        });
    };

    // {
    //     let mut data = client.data.write().await;
    //     data.insert::<sound_store::SoundStore>(std::sync::Arc::new(std::sync::Mutex::new(sound_store)));
    // }

    tokio::join!(async move {
        client.start().await.expect("Error in serenity client!");
    });
}