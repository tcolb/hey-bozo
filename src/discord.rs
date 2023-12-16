use std::collections::HashMap;
use std::{env, sync::{Arc, Mutex as SyncMutex}};
use async_openai::Client as OpenAIClient;
use async_openai::config::OpenAIConfig;
use serenity::prelude::TypeMap;
use serenity::{client::{Client, Context, EventHandler}, 
               framework::{StandardFramework, standard::{macros::{group, command}, Args, CommandResult}},
               prelude::{GatewayIntents, Mentionable, TypeMapKey},
               model::{gateway::Ready, prelude::{Message, ChannelId}}};
use simple_error::bail;
use songbird::model::id::UserId;
use songbird::{EventHandler as VoiceEventHandler, EventContext, model::payload::{Speaking, ClientDisconnect}, Event, Config, driver::DecodeMode, SerenityInit, CoreEvent};
use async_trait::async_trait;
use tokio::sync::{RwLock, mpsc, Mutex, broadcast};

use crate::action_handler::action_handler_loop;
use crate::actions::AssistantAction;
use crate::agent_speaker::AgentSpeaker;
use crate::assistant::DiscordAssistant;
use crate::{listener, resampler};
use crate::sound_store::SoundStore;

pub struct SharedState {
    pub users: HashMap<u32, mpsc::Sender<resampler::ListenerEvent>>,
    pub id_to_ssrc: HashMap<UserId, u32>,
    pub oai_client: Arc<OpenAIClient<OpenAIConfig>>,
    pub sound_store: Arc<SyncMutex<SoundStore>>,
    pub action_channel_tx: broadcast::Sender<AssistantAction>
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
    async fn ready(&self, ctx: Context, ready: Ready) {
        println!("{} is connected!", ready.user.name);

        let music_bot_channel_id: u64 = env::var("MUSIC_CMD_CHANNEL").expect("Couldn't find env MUSIC_CMD_CHANNEL!").parse().unwrap();
        //let channel = ctx.cache.guild_channel(music_bot_channel_id).unwrap();
        let read_guard = ctx.data.read().await;
        let state = read_guard.get::<SharedState>();
        let action_rx = state.unwrap().action_channel_tx.subscribe();
        tokio::spawn(async move {
            action_handler_loop(ctx.http.clone(), ChannelId(music_bot_channel_id), action_rx).await;
        });
    }
}

struct Receiver {
    data: Arc<RwLock<TypeMap>>,
    assistant: Arc<Mutex<DiscordAssistant>>
}

impl Receiver {
    pub fn new(data: Arc<RwLock<TypeMap>>, assistant: Arc<Mutex<DiscordAssistant>>) -> Self{
        Self{
            data: data,
            assistant: assistant
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
                
                let mut write_guard = self.data.write().await;
                if let Some(state) = write_guard.get_mut::<SharedState>() {
                    if !state.users.contains_key(ssrc) {
                        state.id_to_ssrc.insert(user_id.unwrap(), ssrc.clone());
                        let (tx_listener_event, rx_listener_event) = mpsc::channel::<resampler::ListenerEvent>(32);
                        state.users.insert(ssrc.clone(), tx_listener_event);

                        let ssrc = ssrc.clone();
                        let assistant = self.assistant.clone();
                        tokio::spawn(async move {
                            listener::listener_loop(rx_listener_event, assistant, ssrc).await;
                        });
                    }
                }
            },
            EventContext::SpeakingUpdate(data) => {
                println!(
                    "Source {} has {} speaking.",
                    data.ssrc,
                    if data.speaking {"started"} else {"stopped"},
                );
            },
            EventContext::VoicePacket(data) => {
                if let Some(audio) = data.audio {
                    // println!(
                    //     "Audio packet sequence {:05} has {:04} bytes (decompressed from {}), SSRC {}",
                    //     data.packet.sequence.0,
                    //     audio.len() * std::mem::size_of::<i16>(),
                    //     data.packet.payload.len(),
                    //     data.packet.ssrc,
                    // );

                    let mut write_guard: tokio::sync::RwLockWriteGuard<'_, TypeMap> = self.data.write().await;
                    if let Some(state) = write_guard.get_mut::<SharedState>() {
                        if let Some(tx) = state.users.get(&data.packet.ssrc) {
                            tx.send(resampler::ListenerEvent::AudioPacket(audio.clone())).await.unwrap();
                        }
                    }
                } else {
                    println!("RTP packet, but no audio. Driver may not be configured to decode.");
                }
            },
            EventContext::ClientDisconnect(ClientDisconnect{user_id, ..}) => {
                println!("Client disconnected: user {:?}", user_id);
                let mut write_guard: tokio::sync::RwLockWriteGuard<'_, TypeMap> = self.data.write().await;
                if let Some(state) = write_guard.get_mut::<SharedState>() {
                    if let Some(ssrc) = state.id_to_ssrc.get(user_id) {
                        if let Some(tx) = state.users.get(ssrc) {
                            tx.send(resampler::ListenerEvent::Disconnect).await.unwrap();
                        }
                    }
                }
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

    let manager = songbird::get(ctx).await
        .expect("Songbird Voice client placed in at initialization.").clone();

    let guild = msg.guild(&ctx.cache).unwrap();

    let assistant: Arc<Mutex<DiscordAssistant>> = {
        let mut data_guard = ctx.data.write().await;
        if let Some(state) = data_guard.get_mut::<SharedState>() {
            let speaker = AgentSpeaker::new(manager.clone(), guild.id, state.sound_store.clone());
            Arc::new(Mutex::new(DiscordAssistant::new(state.oai_client.clone(), speaker, state.action_channel_tx.clone()).await))
        }
        else {
            bail!("couldn't create discord assistant for channel!")
        }
    };

    let (handler_lock, conn_result) = manager.join(guild.id, connect_to).await;

    if let Ok(_) = conn_result {
        // NOTE: this skips listening for the actual connection result.
        let mut handler = handler_lock.lock().await;

        handler.add_global_event(
            CoreEvent::SpeakingStateUpdate.into(),
            Receiver::new(ctx.data.clone(), assistant.clone()),
        );

        handler.add_global_event(
            CoreEvent::SpeakingUpdate.into(),
            Receiver::new(ctx.data.clone(), assistant.clone()),
        );

        handler.add_global_event(
            CoreEvent::VoicePacket.into(),
            Receiver::new(ctx.data.clone(), assistant.clone()),
        );

        handler.add_global_event(
            CoreEvent::RtcpPacket.into(),
            Receiver::new(ctx.data.clone(), assistant.clone()),
        );

        handler.add_global_event(
            CoreEvent::ClientDisconnect.into(),
            Receiver::new(ctx.data.clone(), assistant.clone()),
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
        .expect("Songbird Voice client placed in at initialization.").clone();
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

pub async fn init_serenity() -> Client {
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