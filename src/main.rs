use std::fs::OpenOptions;
use std::mem::MaybeUninit;
use std::time::{Instant, Duration};
use std::{env, collections::HashMap, sync::Arc};
use dasp_signal::{Signal, interpolate};
use dasp_frame::Frame;
use porcupine::{PorcupineBuilder, Porcupine};
use dotenv::dotenv;
use ringbuf::{Consumer, Producer, SharedRb};
use serenity::prelude::TypeMap;
use serenity::{client::{Client, Context, EventHandler}, 
               framework::{StandardFramework, standard::{macros::{group, command}, Args, CommandResult}},
               prelude::{GatewayIntents, Mentionable, TypeMapKey},
               model::{gateway::Ready, prelude::{Message, ChannelId}}};
use songbird::{EventHandler as VoiceEventHandler, EventContext, model::payload::{Speaking, ClientDisconnect}, Event, Config, driver::DecodeMode, SerenityInit, CoreEvent};
use async_trait::async_trait;
use tokio::sync::RwLock;
use wav::WAV_FORMAT_PCM;
use tokio::sync::mpsc;

#[tokio::main]
async fn main() {
    println!("Hello, world!");
    dotenv().ok();
    
    let mut client = init_serenity().await;

    let (tx_48khz, mut rx_48khz) = mpsc::channel::<Consumer<i16, Arc<SharedRb<i16, Vec<MaybeUninit<i16>>>>>>(32);

    {
        let mut guard: tokio::sync::RwLockWriteGuard<'_, serenity::prelude::TypeMap> = client.data.write().await;
        guard.insert::<DiscordHandles>(DiscordHandles {
            map: HashMap::default(),
            tx: tx_48khz
        });
    };

    let discord_handle = tokio::spawn(async move {
        client.start().await.expect("Error in serenity client!");
    });

    let sample_rate = 16_000;
    let frame_length = 512 as usize;

    let (tx_16khz, mut rx_16khz) = mpsc::channel::<Consumer<i16, Arc<SharedRb<i16, Vec<MaybeUninit<i16>>>>>>(32);

    let downsampler_handle = tokio::spawn(async move {
        loop {
            let mut cons_48khz = rx_48khz.recv().await.unwrap();
            // Create ring buffer for 16khz audio
            let (mut prod_16khz, cons_16khz) = ringbuf::HeapRb::<i16>::new(4 * 16_000).split();
            // Send consumer off to porcupine thread
            tx_16khz.send(cons_16khz).await.unwrap();

            tokio::spawn(async move {
                loop {
                    if cons_48khz.len() >= frame_length {
                        let mut from_signal = dasp_signal::from_interleaved_samples_iter::<_, [i16; 2]>(cons_48khz.pop_iter());
    
                        let left = from_signal.next();
                        let right = from_signal.next();
                        let interpolator = dasp_interpolate::linear::Linear::new(left, right);
                        let mut converter = interpolate::Converter::from_hz_to_hz(
                            from_signal,
                            interpolator,
                            48_000.0,
                            sample_rate as f64
                        );
    
                        loop {
                            let frame = converter.next();
                            if converter.is_exhausted() {
                            //if frame == <[i16; 2]>::EQUILIBRIUM {
                                break;
                            }
                            prod_16khz.push((frame[0] + frame[1]) / 2).unwrap();
                            //prod_16khz.push(frame[0]).unwrap();
                            //prod_16khz.push(frame[1]).unwrap();
                        }
                    }
                }
            });
        }
    });

    let porcupine_handle = tokio::spawn(async move {
        loop {
            let mut cons_16khz = rx_16khz.recv().await.unwrap();

            tokio::spawn(async move {
                // Porcupine model per voice, should probably mix audio beforehand
                let porcupine = init_porcupine();
                let mut input_frame = Vec::<i16>::new();

                let mut from_signal = dasp_signal::from_interleaved_samples_iter::<_, [i16; 2]>(cons_48khz.pop_iter());
                let left = from_signal.next();
                let right = from_signal.next();
                let interpolator = dasp_interpolate::linear::Linear::new(left, right);
                let mut converter = interpolate::Converter::from_hz_to_hz(
                    from_signal,
                    interpolator,
                    48_000.0,
                    sample_rate as f64
                );

                loop {
                    if converter.is_exhausted() {
                        continue;
                    }
                    let frame = converter.next();
                    input_frame.push((frame[0] + frame[1]) / 2);

                    if input_frame.len() == frame_length {
                        if let Ok(index) = porcupine.process(&input_frame) {
                            if index >= 0 {
                                // we out here gamers
                                println!("Hey bozo detected!");
                                //let mut write_guard = data.write().await;
                                //let buffers = write_guard.get_mut::<VoiceBuffers>().unwrap();
                            }
                        }
                        input_frame.clear();
                    }

                    assert!(input_frame.len() < frame_length);
                }
                    // if frame.len() >= 16_000 * 2 {
                    //     let mut path1 = std::env::current_dir().expect("Couldn't get CWD!");
                    //     path1.push("resources");
                    //     path1.push("output");
                    //     path1.push(format!("output_16.wav"));

                    //     let mut file1 = OpenOptions::new().write(true).create(true).open(path1).unwrap();

                    //     let bit_depth = wav::bit_depth::BitDepth::Sixteen(frame.iter().cloned().collect());
                    //     let header1 = wav::Header::new(WAV_FORMAT_PCM, 1, 16_000, 16);
                    //     wav::write(header1, &bit_depth, &mut file1).unwrap();
                    // }
            });
        }
    });

    discord_handle.await.unwrap();
    downsampler_handle.await.unwrap();
    porcupine_handle.await.unwrap();
}

fn init_porcupine() -> Porcupine {
    let mut ppn_path = std::env::current_dir().expect("Couldn't get CWD!");
    ppn_path.push("resources");
    ppn_path.push("hey-bozo_en_windows_v2_2_0.ppn");

    PorcupineBuilder::new_with_keyword_paths(env::var("PV_KEY").expect("Couldn't get env PV_KEY!"), &[ppn_path])
        .init()
        .expect("Couldn't init porcupine!")
}

struct DiscordProducer {
    prod: Producer<i16, Arc<SharedRb<i16, Vec<MaybeUninit<i16>>>>>,
    //instant: Instant
}

// Discord produces 48khz pcm 16
struct DiscordHandles {
    map: HashMap<u32, DiscordProducer>,
    tx: mpsc::Sender<Consumer<i16, Arc<SharedRb<i16, Vec<MaybeUninit<i16>>>>>>
}

impl TypeMapKey for DiscordHandles {
    type Value = DiscordHandles;
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

                let mut write_guard = self.data.write().await;
                if let Some(buffers) = write_guard.get_mut::<DiscordHandles>() {
                    // Create ring buffer prod/cons
                    let (prod, cons) = ringbuf::HeapRb::<i16>::new(15 * 48_000).split();

                    // Store the producer to insert packets into
                    buffers.map.insert(ssrc.clone(), DiscordProducer {
                        //instant: Instant::now(),
                        prod: prod
                    });

                    // Send the consumer to read packets for processing
                    buffers.tx.send(cons).await.unwrap();
                }
            },
            EventContext::SpeakingUpdate(data) => {
                // let mut write_guard = self.data.write().await;
                //     if let Some(buffers) = write_guard.get_mut::<DiscordHandles>() {
                //         if let Some(discord_prod) = buffers.map.get_mut(&data.ssrc) {
                //             let silence_length = discord_prod.instant.elapsed().as_millis() * (48_000 / 1000);
                //             discord_prod.instant = Instant::now();
                //             for _ in 0..silence_length {
                //                  if let Err(_) = discord_prod.prod.push(0) {
                //                      tokio::time::sleep(Duration::from_millis(100)).await;
                //                      break;
                //                  }
                //             }
                //         }
                //     }

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
                    if let Some(buffers) = write_guard.get_mut::<DiscordHandles>() {
                        if let Some(discord_prod) = buffers.map.get_mut(&data.packet.ssrc) {
                            // let silence_length = discord_prod.instant.elapsed().as_millis() * (48_000 / 1000);
                            // discord_prod.instant = Instant::now();
                            // for _ in 0..silence_length {
                            //     if let Err(_) = discord_prod.prod.push(0) {
                            //         tokio::time::sleep(Duration::from_millis(100)).await;
                            //         break;
                            //     }
                            // }
                            discord_prod.prod.push_iter(&mut audio.iter().cloned());
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

// #[command]
// #[only_in(guilds)]
// async fn dump(ctx: &Context, _msg: &Message) -> CommandResult {

//     let mut write_guard = ctx.data.write().await;

//     if let Some(buffers) = write_guard.get_mut::<DiscordHandles>() {
//         for (key, cons) in buffers.map {
//             let mut path1 = std::env::current_dir().expect("Couldn't get CWD!");
//             path1.push("resources");
//             path1.push("output");
//             path1.push(format!("output_16_{key}.wav"));

//             let mut file1 = OpenOptions::new().write(true).create(true).open(path1).unwrap();

//             let bit_depth = wav::bit_depth::BitDepth::Sixteen(cons.iter().cloned().collect());
//             let header1 = wav::Header::new(WAV_FORMAT_PCM, 1, 16_000, 16);
//             wav::write(header1, &bit_depth, &mut file1).unwrap();

//             let mut path2 = std::env::current_dir().expect("Couldn't get CWD!");
//             path2.push("resources");
//             path2.push("output");
//             path2.push(format!("output_48_{key}.wav"));

//             let mut file2 = OpenOptions::new().write(true).create(true).open(path2).unwrap();

//             let bit_depth = wav::bit_depth::BitDepth::Sixteen(cons.pcm_16_48.iter().cloned().collect());
//             let header2 = wav::Header::new(WAV_FORMAT_PCM, 2, 48_000, 16);
//             wav::write(header2, &bit_depth, &mut file2).unwrap();
//         }
//     }

//     Ok(())
// }

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