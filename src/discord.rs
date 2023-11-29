use std::collections::HashMap;
use std::{env, sync::Arc};
use async_openai::Client as OpenAIClient;
use async_openai::types::{CreateSpeechRequestArgs, Voice, SpeechModel};
use serenity::model::id::GuildId;
use serenity::prelude::TypeMap;
use serenity::{client::{Client, Context, EventHandler}, 
               framework::{StandardFramework, standard::{macros::{group, command}, Args, CommandResult}},
               prelude::{GatewayIntents, Mentionable, TypeMapKey},
               model::{gateway::Ready, prelude::{Message, ChannelId}}};
use songbird::ffmpeg;
use songbird::model::id::UserId;
use songbird::{EventHandler as VoiceEventHandler, EventContext, model::payload::{Speaking, ClientDisconnect}, Event, Config, driver::DecodeMode, SerenityInit, CoreEvent};
use async_trait::async_trait;
use tokio::sync::{RwLock, mpsc, Mutex};
use uuid::Uuid;

use crate::assistant::DiscordAssistant;
use crate::{listener, sound_store};

pub struct SharedState {
    pub users: HashMap<u32, mpsc::Sender<listener::ListenerEvent>>,
    pub id_to_ssrc: HashMap<UserId, u32>,
    pub tx_audio: Option<mpsc::Sender<(GuildId, AgentVoiceEvent)>>,
    pub assistant: Arc<Mutex<DiscordAssistant>>
}

impl TypeMapKey for SharedState {
    type Value = SharedState;
}

pub enum AgentVoiceEvent {
    Acknowledge,
    Text(String)
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

    async fn cache_ready(&self, ctx: Context, _guilds: Vec<GuildId>) {
        let manager = songbird::get(&ctx)
                .await
                .expect("Songbird Voice client placed in at initialization.")
                .clone();

        let (tx_audio, mut rx_audio) = mpsc::channel::<(GuildId, AgentVoiceEvent)>(32);
        //let oai_client: OpenAIClient<async_openai::config::OpenAIConfig> = OpenAIClient::new();

        let mut write_guard = ctx.data.write().await;
        if let Some(state) = write_guard.get_mut::<SharedState>() {
            state.tx_audio = Some(tx_audio);
        }

        let ctx = ctx.clone();
        tokio::spawn(async move {

            let oai_client = OpenAIClient::new();

            loop {
                let (guild_id, event) = rx_audio.recv().await.unwrap();

                match event {
                    AgentVoiceEvent::Acknowledge => {
                        if let Some(handler_lock) = manager.get(guild_id) {
                            let mut handler = handler_lock.lock().await;
                            let sound_store_lock = ctx.data.read().await.get::<sound_store::SoundStore>().cloned().expect("Sound cache was installed at startup.");
                            let sound_store = sound_store_lock.lock().unwrap();
                            let input = sound_store.get("acknowledge").expect("Handle placed into cache at startup.");

                            handler.play_source(input.new_handle().try_into().unwrap());
                        }
                    },
                    AgentVoiceEvent::Text(text) => {
                        let voice_request: async_openai::types::CreateSpeechRequest = CreateSpeechRequestArgs::default()
                            .input(text)
                            .voice(Voice::Onyx)
                            .model(SpeechModel::Tts1)
                            .build().unwrap();

                        match oai_client.audio().speech(voice_request).await {
                            Ok(speech) => {
                                let path = format!("../../tmp/{}.mp3", Uuid::new_v4());
                                speech.save(path.clone()).await.unwrap();

                                if let Some(handler_lock) = manager.get(guild_id) {
                                    let mut handler = handler_lock.lock().await;
                                    handler.play_source(ffmpeg(path).await.unwrap());
                                }
                            },
                            Err(e) => {
                                println!("{}", e.to_string());
                            } 
                        }
                    }
                }
            }
        });
    }
}

struct Receiver {
    data: Arc<RwLock<TypeMap>>,
    guild_id: GuildId
}

impl Receiver {
    pub fn new(data: Arc<RwLock<TypeMap>>, guild_id: GuildId) -> Self{
        Self{
            data: data,
            guild_id: guild_id
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
                        let (tx_listener_event, mut rx_listener_event) = mpsc::channel::<listener::ListenerEvent>(32);
                        state.users.insert(ssrc.clone(), tx_listener_event);

                        let guild_id = self.guild_id.clone();
                        let mut tx_audio = state.tx_audio.clone().unwrap();
                        let assistant = state.assistant.clone();
                        tokio::spawn(async move {
                            listener::listener_loop(&mut rx_listener_event, guild_id, &mut tx_audio, assistant).await;
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
                            tx.send(listener::ListenerEvent::AudioPacket(audio.clone())).await.unwrap();
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
                            tx.send(listener::ListenerEvent::Disconnect).await.unwrap();
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
            Receiver::new(ctx.data.clone(), guild_id),
        );

        handler.add_global_event(
            CoreEvent::SpeakingUpdate.into(),
            Receiver::new(ctx.data.clone(), guild_id),
        );

        handler.add_global_event(
            CoreEvent::VoicePacket.into(),
            Receiver::new(ctx.data.clone(), guild_id),
        );

        handler.add_global_event(
            CoreEvent::RtcpPacket.into(),
            Receiver::new(ctx.data.clone(), guild_id),
        );

        handler.add_global_event(
            CoreEvent::ClientDisconnect.into(),
            Receiver::new(ctx.data.clone(), guild_id),
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