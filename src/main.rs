mod listener;
mod discord;

use dotenv::dotenv;
use tokio::sync::mpsc;

#[tokio::main(flavor = "multi_thread", worker_threads = 10)]
async fn main() {
    println!("Hello, world!");
    dotenv().ok();
    
    let mut client = discord::init_serenity().await;

    let (tx_48khz, mut rx_48khz) = mpsc::channel::<(u32, Vec<i16>)>(32);

    {
        // Initialize shared state.
        let mut guard: tokio::sync::RwLockWriteGuard<'_, serenity::prelude::TypeMap> = client.data.write().await;
        guard.insert::<discord::SharedState>(discord::SharedState {
            tx_packet: tx_48khz.clone()
        });
    };

    let discord_handle = tokio::spawn(async move {
        client.start().await.expect("Error in serenity client!");
    });

    let consumer_handle = tokio::spawn(async move {
        listener::listener_loop(&mut rx_48khz).await;
    });

    discord_handle.await.unwrap();
    consumer_handle.await.unwrap();
}