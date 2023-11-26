mod listener;

use dotenv::dotenv;
use songbird::events::context_data::SpeakingUpdateData;
use tokio::time::Instant;
use std::time::Duration;
use std::{env, sync::Arc};
use serenity::prelude::TypeMap;
use serenity::{client::{Client, Context, EventHandler}, 
               framework::{StandardFramework, standard::{macros::{group, command}, Args, CommandResult}},
               prelude::{GatewayIntents, Mentionable, TypeMapKey},
               model::{gateway::Ready, prelude::{Message, ChannelId}}};
use songbird::{EventHandler as VoiceEventHandler, EventContext, model::payload::{Speaking, ClientDisconnect}, Event, Config, driver::DecodeMode, SerenityInit, CoreEvent};
use async_trait::async_trait;
use tokio::sync::RwLock;
use tokio::sync::mpsc;

#[tokio::main(flavor = "multi_thread", worker_threads = 10)]
async fn main() {
    println!("Hello, world!");
    dotenv().ok();
    
    let mut client = init_serenity().await;

    let (tx_speaking_state, mut rx_speaking_state) = mpsc::channel::<SpeakingUpdateData>(32);
    let (tx_48khz, mut rx_48khz) = mpsc::channel::<(u32, Vec<i16>)>(32);

    {
        let mut guard: tokio::sync::RwLockWriteGuard<'_, serenity::prelude::TypeMap> = client.data.write().await;
        guard.insert::<SharedState>(SharedState {
            tx_packet: tx_48khz.clone(),
            tx_state: tx_speaking_state.clone()
        });
    };

    let discord_handle = tokio::spawn(async move {
        client.start().await.expect("Error in serenity client!");
    });

    let consumer_handle: tokio::task::JoinHandle<()> = tokio::spawn(async move {
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

// Discord produces 48khz pcm 16
struct SharedState {
    tx_packet: mpsc::Sender<(u32, Vec<i16>)>,
    tx_state: mpsc::Sender<SpeakingUpdateData>
}

impl TypeMapKey for SharedState {
    type Value = SharedState;
}

#[group]
#[commands(join, leave)]
struct General;

struct Handler;

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, _: Context, ready: Ready) {
        println!("{} is connected!", ready.user.name);
    }
}

struct Receiver {
    data: Arc<RwLock<TypeMap>>
}

impl Receiver {
    pub fn new(data: Arc<RwLock<TypeMap>>) -> Self{
        Self{
            data: data
        }
    }
}

#[async_trait]
impl VoiceEventHandler for Receiver {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        match ctx {
            EventContext::SpeakingStateUpdate(Speaking{speaking, ssrc, user_id, ..}) => {
                println!(
                    "Speaking state update: user {:?} has SSRC {:?}, using {:?}",
                    user_id,
                    ssrc,
                    speaking,
                );
            },
            EventContext::SpeakingUpdate(data) => {
                println!(
                    "Source {} has {} speaking.",
                    data.ssrc,
                    if data.speaking {"started"} else {"stopped"},
                );

                let mut write_guard = self.data.write().await;
                if let Some(state) = write_guard.get_mut::<SharedState>() {
                    state.tx_state.send(data.clone()).await.unwrap();
                }
            },
            EventContext::VoicePacket(data) => {
                if let Some(audio) = data.audio {
                    println!(
                        "Audio packet sequence {:05} has {:04} bytes (decompressed from {}), SSRC {}",
                        data.packet.sequence.0,
                        audio.len() * std::mem::size_of::<i16>(),
                        data.packet.payload.len(),
                        data.packet.ssrc,
                    );

                    let mut write_guard = self.data.write().await;
                    if let Some(state) = write_guard.get_mut::<SharedState>() {

                        match state.tx_packet.send((data.packet.ssrc, audio.clone())).await {
                            Ok(()) => {

                            },
                            Err(e) => {
                                println!("Error {}", e);
                            }
                        }
                    }
                } else {
                    println!("RTP packet, but no audio. Driver may not be configured to decode.");
                }
            },
            EventContext::ClientDisconnect(ClientDisconnect{user_id, ..}) => {
                println!("Client disconnected: user {:?}", user_id);
            }
            _ => ()
        }

        None
    }
}

#[command]
#[only_in(guilds)]
async fn join(ctx: &Context, msg: &Message, mut args: Args) -> CommandResult {
    let connect_to = match args.single::<u64>() {
        Ok(id) => ChannelId(id),
        Err(_) => {
            msg.reply(ctx, "Requires a valid voice channel ID be given").await?;
            return Ok(());
        },
    };

    let guild = msg.guild(&ctx.cache).unwrap();
    let guild_id = guild.id;

    let manager = songbird::get(ctx).await
        .expect("Songbird Voice client placed in at initialisation.").clone();

    let (handler_lock, conn_result) = manager.join(guild_id, connect_to).await;

    if let Ok(_) = conn_result {
        // NOTE: this skips listening for the actual connection result.
        let mut handler = handler_lock.lock().await;

        handler.add_global_event(
            CoreEvent::SpeakingStateUpdate.into(),
            Receiver::new(ctx.data.clone()),
        );

        handler.add_global_event(
            CoreEvent::SpeakingUpdate.into(),
            Receiver::new(ctx.data.clone()),
        );

        handler.add_global_event(
            CoreEvent::VoicePacket.into(),
            Receiver::new(ctx.data.clone()),
        );

        handler.add_global_event(
            CoreEvent::RtcpPacket.into(),
            Receiver::new(ctx.data.clone()),
        );

        handler.add_global_event(
            CoreEvent::ClientDisconnect.into(),
            Receiver::new(ctx.data.clone()),
        );

        msg.channel_id.say(&ctx.http, &format!("Joined {}", connect_to.mention())).await.unwrap();
    } else {
        msg.channel_id.say(&ctx.http, "Error joining the channel").await.unwrap();
    }

    Ok(())
}

#[command]
#[only_in(guilds)]
async fn leave(ctx: &Context, msg: &Message) -> CommandResult {
    let guild = msg.guild(&ctx.cache).unwrap();
    let guild_id = guild.id;

    let manager = songbird::get(ctx).await
        .expect("Songbird Voice client placed in at initialisation.").clone();
    let has_handler = manager.get(guild_id).is_some();

    if has_handler {
        if let Err(e) = manager.remove(guild_id).await {
            msg.channel_id.say(&ctx.http, format!("Failed: {:?}", e)).await.unwrap();
        }

        msg.channel_id.say(&ctx.http,"Left voice channel").await.unwrap();
    } else {
        msg.reply(ctx, "Not in a voice channel").await.unwrap();
    }

    Ok(())
}

async fn init_serenity() -> Client {
    let framework = StandardFramework::new()
        .configure(|c| c.prefix("~"))
        .group(&GENERAL_GROUP);

    let intents = GatewayIntents::non_privileged()
        | GatewayIntents::MESSAGE_CONTENT;

    let songbird_config = Config::default()
        .decode_mode(DecodeMode::Decode);

    Client::builder(&env::var("DISCORD_TOKEN").expect("Couldn't find env DISCORD_TOKEN!"), intents)
        .event_handler(Handler)
        .framework(framework)
        .register_songbird_from_config(songbird_config)
        .await
        .expect("Couldn't create serenity client!")
}