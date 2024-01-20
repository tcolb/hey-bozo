use async_openai::config::OpenAIConfig;
use async_openai::Client as OpenAIClient;
use async_trait::async_trait;
use serenity::all::GuildChannel;
use serenity::cache::GuildRef;
use serenity::client::Context;
use serenity::framework::standard::Configuration;
use serenity::http::CacheHttp;
use serenity::model::guild::Guild;
use serenity::model::id::GuildId;
use serenity::model::user::User;
use serenity::prelude::TypeMap;
use serenity::{
    client::{Client, EventHandler},
    framework::{
        standard::{
            macros::{command, group},
            Args, CommandResult,
        },
        StandardFramework,
    },
    model::{
        gateway::Ready,
        prelude::{ChannelId, Message},
    },
    prelude::{GatewayIntents, Mentionable, TypeMapKey},
};
use simple_error::bail;
use songbird::driver::DecodeMode;
use songbird::model::id::UserId;
use songbird::packet::Packet;
use songbird::{
    model::payload::{ClientDisconnect, Speaking},
    Config, CoreEvent, Event, EventContext, EventHandler as VoiceEventHandler, SerenityInit,
};
use std::collections::HashMap;
use std::ops::Deref;
use std::{
    env,
    sync::{Arc, Mutex as SyncMutex},
};
use tokio::sync::{broadcast, mpsc, Mutex, RwLock};

use crate::action_handler::action_handler_loop;
use crate::actions::AssistantAction;
use crate::agent_speaker::AgentSpeaker;
use crate::assistant::DiscordAssistant;
use crate::sound_store::SoundStore;
use crate::{listener, resampler};

pub struct SharedState {
    pub users: HashMap<u32, mpsc::Sender<resampler::ListenerEvent>>,
    pub id_to_ssrc: HashMap<UserId, u32>,
    pub oai_client: Arc<OpenAIClient<OpenAIConfig>>,
    pub sound_store: Arc<SyncMutex<SoundStore>>,
    pub action_channel_tx: broadcast::Sender<AssistantAction>,
}

impl TypeMapKey for SharedState {
    type Value = SharedState;
}

#[group]
#[commands(bozo, unbozo)]
struct General;

struct Handler;

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, ctx: Context, ready: Ready) {
        println!("{} is connected!", ready.user.name);

        let music_bot_channel_id: u64 = env::var("MUSIC_CMD_CHANNEL")
            .expect("Couldn't find env MUSIC_CMD_CHANNEL!")
            .parse()
            .unwrap();
        //let channel = ctx.cache.guild_channel(music_bot_channel_id).unwrap();
        let read_guard = ctx.data.read().await;
        let state = read_guard.get::<SharedState>();
        let action_rx = state.unwrap().action_channel_tx.subscribe();
        tokio::spawn(async move {
            action_handler_loop(
                ctx.http.clone(),
                ChannelId::new(music_bot_channel_id),
                action_rx,
            )
            .await;
        });
    }
}

#[derive(Clone)]
struct Receiver {
    data: Arc<RwLock<TypeMap>>,
    assistant: Arc<Mutex<DiscordAssistant>>,
}

impl Receiver {
    pub fn new(data: Arc<RwLock<TypeMap>>, assistant: Arc<Mutex<DiscordAssistant>>) -> Self {
        Self {
            data: data,
            assistant: assistant,
        }
    }
}

#[async_trait]
impl VoiceEventHandler for Receiver {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        match ctx {
            EventContext::SpeakingStateUpdate(Speaking {
                speaking,
                ssrc,
                user_id,
                ..
            }) => {
                println!(
                    "Speaking state update: user {:?} has SSRC {:?}, using {:?}",
                    user_id, ssrc, speaking,
                );

                let mut write_guard = self.data.write().await;
                if let Some(state) = write_guard.get_mut::<SharedState>() {
                    if !state.users.contains_key(ssrc) {
                        state.id_to_ssrc.insert(user_id.unwrap(), ssrc.clone());
                        let (tx_listener_event, rx_listener_event) =
                            mpsc::channel::<resampler::ListenerEvent>(32);
                        state.users.insert(ssrc.clone(), tx_listener_event);

                        let ssrc = ssrc.clone();
                        let assistant = self.assistant.clone();
                        tokio::spawn(async move {
                            listener::listener_loop(rx_listener_event, assistant, ssrc).await;
                        });
                    }
                }
            }
            // EventContext::SpeakingUpdate(data) => {
            //     println!(
            //         "Source {} has {} speaking.",
            //         data.ssrc,
            //         if data.speaking { "started" } else { "stopped" },
            //     );
            // }
            EventContext::VoiceTick(tick) => {
                for (ssrc, data) in &tick.speaking {
                    let decoded_voice = data.decoded_voice.as_ref().unwrap();
                    let mut write_guard: tokio::sync::RwLockWriteGuard<'_, TypeMap> =
                        self.data.write().await;
                    if let Some(state) = write_guard.get_mut::<SharedState>() {
                        if let Some(tx) = state.users.get(ssrc) {
                            tx.send(resampler::ListenerEvent::AudioPacket(decoded_voice.clone()))
                                .await
                                .unwrap();
                        }
                    }
                }
            }
            EventContext::ClientDisconnect(ClientDisconnect { user_id, .. }) => {
                println!("Client disconnected: user {:?}", user_id);
                let mut write_guard: tokio::sync::RwLockWriteGuard<'_, TypeMap> =
                    self.data.write().await;
                if let Some(state) = write_guard.get_mut::<SharedState>() {
                    if let Some(ssrc) = state.id_to_ssrc.get(user_id) {
                        if let Some(tx) = state.users.get(ssrc) {
                            tx.send(resampler::ListenerEvent::Disconnect).await.unwrap();
                        }
                    }
                }
            }
            _ => (),
        }

        None
    }
}

fn find_channel_from_user(
    ctx: &Context,
    user: &User,
    channels: &HashMap<ChannelId, GuildChannel>,
) -> Option<ChannelId> {
    for (channel_id, channel) in channels {
        if let Some(guild_channel) = channel.guild(&ctx.cache) {
            for (user_id, _member) in &guild_channel.members {
                if user_id.eq(&user.id) {
                    return Some(channel_id.clone());
                }
            }
        }
    }
    return None;
}

#[command]
#[only_in(guilds)]
async fn bozo(ctx: &Context, msg: &Message, mut _args: Args) -> CommandResult {
    let channels = {
        match msg.guild(&ctx.cache) {
            Some(guild) => Some(guild.channels.clone()),
            None => None,
        }
    };

    match find_channel_from_user(&ctx, &msg.author, &channels.unwrap()) {
        Some(channel_id) => {
            let manager = songbird::get(ctx)
                .await
                .expect("Songbird Voice client placed in at initialization.")
                .clone();

            let assistant: Arc<Mutex<DiscordAssistant>> = {
                let mut data_guard = ctx.data.write().await;
                if let Some(state) = data_guard.get_mut::<SharedState>() {
                    Arc::new(Mutex::new(
                        DiscordAssistant::new(
                            state.oai_client.clone(),
                            AgentSpeaker::new(
                                manager.clone(),
                                msg.guild_id.unwrap().into(),
                                state.sound_store.clone(),
                            ),
                            state.action_channel_tx.clone(),
                        )
                        .await,
                    ))
                } else {
                    bail!("couldn't create discord assistant for channel!")
                }
            };

            if let Ok(join_lock) = manager.join(msg.guild_id.unwrap(), channel_id).await {
                // NOTE: this skips listening for the actual connection result.
                let mut handler = join_lock.lock().await;

                let receiver = Receiver::new(ctx.data.clone(), assistant.clone());

                handler.add_global_event(CoreEvent::SpeakingStateUpdate.into(), receiver.clone());
                handler.add_global_event(CoreEvent::VoiceTick.into(), receiver.clone());
                handler.add_global_event(CoreEvent::ClientDisconnect.into(), receiver.clone());

                msg.channel_id
                    .say(&ctx.http, &format!("Joined {}", channel_id.mention()))
                    .await
                    .unwrap();
            } else {
                msg.channel_id
                    .say(&ctx.http, "Error joining the channel")
                    .await
                    .unwrap();
            }

            Ok(())
        }
        None => {
            msg.reply(ctx, "Error finding channel ID!").await?;
            return Ok(());
        }
    }
}

#[command]
#[only_in(guilds)]
async fn unbozo(ctx: &Context, msg: &Message) -> CommandResult {
    let guild_id = msg.guild_id.unwrap();

    let manager = songbird::get(ctx)
        .await
        .expect("Songbird Voice client placed in at initialization.");
    let has_handler = manager.get(guild_id).is_some();

    if has_handler {
        if let Err(e) = manager.remove(guild_id).await {
            msg.channel_id
                .say(&ctx.http, format!("Failed: {:?}", e))
                .await
                .unwrap();
        }

        msg.channel_id
            .say(&ctx.http, "Left voice channel")
            .await
            .unwrap();
    } else {
        msg.reply(ctx, "Not in a voice channel").await.unwrap();
    }

    Ok(())
}

pub async fn init_serenity() -> Client {
    let framework = StandardFramework::new().group(&GENERAL_GROUP);
    framework.configure(Configuration::new().prefix("~"));
    let intents = GatewayIntents::non_privileged() | GatewayIntents::MESSAGE_CONTENT;
    let songbird_config = Config::default().decode_mode(DecodeMode::Decode);
    let token = env::var("DISCORD_TOKEN").expect("Couldn't find env DISCORD_TOKEN!");
    Client::builder(token, intents)
        .event_handler(Handler)
        .framework(framework)
        .register_songbird_from_config(songbird_config)
        .await
        .expect("Couldn't create serenity client!")
}
