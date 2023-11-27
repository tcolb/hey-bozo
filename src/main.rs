mod discord;
mod listener;

use std::collections::HashMap;
use dotenv::dotenv;

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
            id_to_ssrc: HashMap::default()
        });
    };

    let discord_handle = tokio::spawn(async move {
        client.start().await.expect("Error in serenity client!");
    });

    discord_handle.await.unwrap();
}