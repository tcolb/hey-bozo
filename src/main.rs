mod listener;
mod discord;

use dotenv::dotenv;
use songbird::events::context_data::SpeakingUpdateData;
use tokio::time::Instant;
use std::time::Duration;
use tokio::sync::mpsc;

#[tokio::main(flavor = "multi_thread", worker_threads = 10)]
async fn main() {
    println!("Hello, world!");
    dotenv().ok();
    
    let mut client = discord::init_serenity().await;

    let (tx_speaking_state, mut rx_speaking_state) = mpsc::channel::<SpeakingUpdateData>(32);
    let (tx_48khz, mut rx_48khz) = mpsc::channel::<(u32, Vec<i16>)>(32);

    {
        // Initialize shared state.
        let mut guard: tokio::sync::RwLockWriteGuard<'_, serenity::prelude::TypeMap> = client.data.write().await;
        guard.insert::<discord::SharedState>(discord::SharedState {
            tx_packet: tx_48khz.clone(),
            tx_state: tx_speaking_state.clone()
        });
    };

    let discord_handle = tokio::spawn(async move {
        client.start().await.expect("Error in serenity client!");
    });

    let consumer_handle = tokio::spawn(async move {
        listener::listener_loop(&mut rx_48khz).await;
    });

    let producer_handle = tokio::spawn(async move {
        producer_loop(&mut rx_speaking_state, &mut tx_48khz.clone()).await;
    });

    discord_handle.await.unwrap();
    consumer_handle.await.unwrap();
    producer_handle.await.unwrap();
}

async fn producer_loop(rx_state: &mut mpsc::Receiver<SpeakingUpdateData>, tx_48khz: &mut mpsc::Sender<(u32, Vec<i16>)>) {
    // TODO handle this via start/stop speaking
    let mut stopped = Instant::now();
    loop {
        let state = rx_state.recv().await.unwrap();
        if state.speaking {
            let mut silence_duration = stopped.elapsed();
            if silence_duration > Duration::from_millis(3000) {
                silence_duration = Duration::from_millis(3000);
            }
            let sample_count = (silence_duration.as_millis() * 48) as usize;
            tx_48khz.send((0, vec![0; sample_count])).await.unwrap();
        } else {
            stopped = Instant::now();
        }
    }
}