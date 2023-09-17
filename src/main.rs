use std::collections::VecDeque;
use std::{env, sync::Arc};
use cheetah::{CheetahBuilder, Cheetah};
use porcupine::{PorcupineBuilder, Porcupine};
use dotenv::dotenv;

use serenity::prelude::TypeMap;
use serenity::{client::{Client, Context, EventHandler}, 
               framework::{StandardFramework, standard::{macros::{group, command}, Args, CommandResult}},
               prelude::{GatewayIntents, Mentionable, TypeMapKey},
               model::{gateway::Ready, prelude::{Message, ChannelId}}};
use songbird::events::context_data::SpeakingUpdateData;
use songbird::{EventHandler as VoiceEventHandler, EventContext, model::payload::{Speaking, ClientDisconnect}, Event, Config, driver::DecodeMode, SerenityInit, CoreEvent};
use async_trait::async_trait;
use tokio::sync::RwLock;
use wav::WAV_FORMAT_PCM;
use tokio::sync::mpsc;

use rubato::{Resampler, SincFixedOut, SincInterpolationType, SincInterpolationParameters, WindowFunction};

async fn read_frames(rx: &mut mpsc::Receiver<(u32, Vec<i16>)>, buf: &mut VecDeque<i16>, instant: &mut std::time::Instant, n: usize, channels: usize) -> Vec<Vec<f64>> {

    let mut out = Vec::with_capacity(channels);
    for _ in 0..channels {
        out.push(Vec::with_capacity(n));
    }

    while buf.len() < n * 2 {
        let (_id, packet_audio) = rx.recv().await.unwrap();
        buf.extend(packet_audio);
    }
    
    for _ in 0..n {
        let l = buf.pop_front().unwrap() as f64 / 32768.0;
        let r = buf.pop_front().unwrap() as f64 / 32768.0;
        out[0].push(l.clamp(-1.0, 1.0));
        out[1].push(r.clamp(-1.0, 1.0));
    }

    out
}

#[tokio::main(flavor = "multi_thread", worker_threads = 10)]
async fn main() {
    println!("Hello, world!");
    dotenv().ok();
    
    let mut client = init_serenity().await;

    //let audio_rb = ringbuf::HeapRb::<i32>::new(64);

    let (tx_48khz, mut rx_48khz) = mpsc::channel::<(u32, Vec<i16>)>(32);
    {
        let mut guard: tokio::sync::RwLockWriteGuard<'_, serenity::prelude::TypeMap> = client.data.write().await;
        guard.insert::<SharedState>(SharedState {
            tx: tx_48khz
        });
    };

    let discord_handle = tokio::spawn(async move {
        client.start().await.expect("Error in serenity client!");
    });

    let consumer_handle = tokio::spawn(async move {
        consumer_loop(&mut rx_48khz).await;
    });

    discord_handle.await.unwrap();
    consumer_handle.await.unwrap();
}

async fn producer_loop() {



    // Produce silence every 20ms
    tokio::time::sleep(std::time::Duration::from_millis(20));
}

async fn consumer_loop(rx_48khz: &mut mpsc::Receiver<(u32, Vec<i16>)>) {
    // Porcupine model per voice, should probably mix audio beforehand
    let porcupine = init_porcupine();
    let porcupine_sample_rate = porcupine.sample_rate();
    let porcupine_frame_length = porcupine.frame_length();

    // Cheetah
    let cheetah = init_cheetah();
    let cheetah_sample_rate = cheetah.sample_rate();
    let cheetah_frame_length = cheetah.frame_length();

    assert!(porcupine_sample_rate == cheetah_sample_rate);
    assert!(cheetah_frame_length == porcupine_frame_length);

    // Resampler
    let resampler_params = SincInterpolationParameters {
        sinc_len: 256,
        f_cutoff: 0.95,
        interpolation: SincInterpolationType::Linear,
        oversampling_factor: 256,
        window: WindowFunction::BlackmanHarris2,
    };

    let mut resampler = SincFixedOut::<f64>::new(
        porcupine_sample_rate as f64 / 48_000 as f64,
        2.0,
        resampler_params,
        porcupine_frame_length as usize,
        2,
    ).unwrap();

    let mut buf = VecDeque::with_capacity(porcupine_frame_length as usize * 2);
    let mut input_frame = Vec::<i16>::with_capacity(porcupine_frame_length as  usize);

    let mut speech_to_text = false;

    // Consume packets
    loop {
        let frames_needed = resampler.input_frames_next();
        let frames = read_frames(rx_48khz, &mut buf, frames_needed, 2).await;

        let result = resampler.process(&frames, None).unwrap();

        input_frame.clear();
        for i in 0..result[0].len() {
            let l = result[0][i];
            let r = result[1][i];
            let v = (l + r) / 2.0;
            input_frame.push((v * 32768.0).clamp(-32768.0, 32768.0) as i16);
        }

        // assert!(frame_length as usize == input_frame.len());

        if speech_to_text {
            if let Ok(cheetah_transcript) = cheetah.process(&input_frame) {
                println!("Cheetah listening: {}", cheetah_transcript.transcript);
                if cheetah_transcript.is_endpoint {
                    if let Ok(cheetah_transcript) = cheetah.flush() {
                        speech_to_text = false;
                        println!("Cheetah finished: {}", cheetah_transcript.transcript)
                    }
                }
            }
        }
        else {
            // Listening in for trigger word
            match porcupine.process(&input_frame) {
                Ok(keyword_index) => {
                    if keyword_index >= 0 {
                        // Hit the trigger word, start speech to text
                        speech_to_text = true;
                        println!("bozo detected!");
                    }
                },
                Err(e) => {
                    println!("Porcupine error: {}", e);
                }
            }
        }

        // Write that shit
        // if cringe.len() >= 16_000 * 5 {
        //     let mut path1 = std::env::current_dir().expect("Couldn't get CWD!");
        //     path1.push("resources");
        //     path1.push("output");
        //     path1.push(format!("output_16.wav"));

        //     let mut file1 = OpenOptions::new().write(true).create(true).open(path1).unwrap();

        //     let bit_depth = wav::bit_depth::BitDepth::Sixteen(cringe.iter().cloned().collect());
        //     let header1 = wav::Header::new(WAV_FORMAT_PCM, 2, sample_rate, 16);
        //     wav::write(header1, &bit_depth, &mut file1).unwrap();
        //     cringe.clear();
        // }
    }
}

fn init_porcupine() -> Porcupine {
    let mut ppn_path = std::env::current_dir().expect("Couldn't get CWD!");
    ppn_path.push("resources");
    ppn_path.push("hey-bozo_en_windows_v2_2_0.ppn");

    PorcupineBuilder::new_with_keyword_paths(env::var("PV_KEY").expect("Couldn't get env PV_KEY!"), &[ppn_path])
        .init()
        .expect("Couldn't init porcupine!")
}

fn init_cheetah() -> Cheetah {
    CheetahBuilder::new()
        .access_key(env::var("PV_KEY").expect("Couldn't get env PV_KEY!"))
        .init()
        .expect("Unable to create Cheetah")
}

// Discord produces 48khz pcm 16
struct SharedState {
    tx: mpsc::Sender<(u32, Vec<i16>)>,
    speaking: SpeakingUpdateData,
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
                    if let Some(buffers) = write_guard.get_mut::<SharedState>() {

                        match buffers.tx.send((data.packet.ssrc, audio.clone())).await {
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