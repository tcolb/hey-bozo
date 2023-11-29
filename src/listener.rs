use std::{env, collections::VecDeque, time::Duration, sync::Arc};
use serenity::model::id::GuildId;
use tokio::{sync::{mpsc, Mutex}, time::{timeout, Instant}};
//use wav::WAV_FORMAT_PCM;
//use std::fs::OpenOptions;

// Resampling
use rubato::{Resampler, SincFixedOut, SincInterpolationType, SincInterpolationParameters, WindowFunction};

// APIs
use cheetah::{CheetahBuilder, Cheetah};
use porcupine::{PorcupineBuilder, Porcupine};

use crate::{discord::AgentVoiceEvent, assistant::DiscordAssistant};

pub enum ListenerEvent {
    AudioPacket(Vec<i16>),
    Disconnect
}

pub async fn listener_loop(
    rx_48khz: &mut mpsc::Receiver<ListenerEvent>,
    guild_id: GuildId, 
    tx_audio: &mut mpsc::Sender<(GuildId, AgentVoiceEvent)>, 
    assistant: Arc<Mutex<DiscordAssistant>>) {

    let porcupine = init_porcupine();
    let cheetah: Cheetah = init_cheetah();

    assert!(porcupine.sample_rate() == cheetah.sample_rate());
    assert!(cheetah.frame_length() == porcupine.frame_length());

    let resampler_params = SincInterpolationParameters {
        sinc_len: 256,
        f_cutoff: 0.95,
        interpolation: SincInterpolationType::Linear,
        oversampling_factor: 256,
        window: WindowFunction::BlackmanHarris2,
    };

    let mut resampler = SincFixedOut::<f64>::new(
        porcupine.sample_rate() as f64 / 48_000 as f64,
        2.0,
        resampler_params,
        porcupine.frame_length() as usize,
        2,
    ).unwrap();

    let mut buf = VecDeque::with_capacity(porcupine.frame_length() as usize * 2);
    let mut input_frame = Vec::<i16>::with_capacity(porcupine.frame_length() as  usize);

    let mut speech_to_text = false;
    let mut total_transcript = String::new();

    // TODO remove
    //let mut cringe = Vec::<i16>::with_capacity(16_000 * 10);

    let mut silence = Instant::now();

    // Consume packets
    loop {
        let frames_needed = resampler.input_frames_next();
        let frames = read_frames(rx_48khz, &mut buf, frames_needed, 2).await;

        if frames.is_none() {
            // Client disconnected!
            break;
        }

        let resampled_frame = resampler.process(&frames.unwrap(), None).unwrap();

        input_frame.clear();
        for i in 0..resampled_frame[0].len() {
            let l = resampled_frame[0][i];
            let r = resampled_frame[1][i];
            let v = (l + r) / 2.0;
            input_frame.push((v * 32768.0).clamp(-32768.0, 32768.0) as i16);
        }

        if speech_to_text {

            // TODO remove
            //cringe.extend(input_frame.clone());
            
            // User hasn't said anything for 3seconds, end the convo.
            let partial_transcript = cheetah.process(&input_frame).unwrap();
            // if partial_transcript.transcript.is_empty() && silence.elapsed() > Duration::from_millis(3000) {
            //     let mut guard = assistant.lock().await;
            //     assert!(guard.busy);
            //     guard.busy = false;
            //     speech_to_text = false;
            //     println!("user stopped talking, ending...");
            //     continue;
            // }

            total_transcript.push_str(&partial_transcript.transcript);
            silence = Instant::now();

            if partial_transcript.is_endpoint {
                let final_transcript = cheetah.flush().unwrap();
                total_transcript.push_str(&final_transcript.transcript);

                println!("final sentence: {}", total_transcript);

                let mut guard = assistant.lock().await;
                assert!(guard.busy);
                match guard.send_message(&total_transcript).await {
                    Some(response) => {
                        tx_audio.send((guild_id, AgentVoiceEvent::Text(response))).await.unwrap();
                        total_transcript.clear();

                        // TODO fix this
                        speech_to_text = false;
                        guard.busy = false;
                    },
                    None => {
                        speech_to_text = false;
                        guard.busy = false;
                    }
                }
                // let mut path1 = std::env::current_dir().expect("Couldn't get CWD!");
                // path1.push("resources");
                // path1.push("output");
                // path1.push(format!("output_16.wav"));

                // let mut file1 = OpenOptions::new().write(true).create(true).open(path1).unwrap();

                // let bit_depth = wav::bit_depth::BitDepth::Sixteen(cringe.iter().cloned().collect());
                // let header1 = wav::Header::new(WAV_FORMAT_PCM, 1, porcupine.sample_rate(), 16);
                // wav::write(header1, &bit_depth, &mut file1).unwrap();
            }
        }
        else {
            // Listening in for the trigger word.
            match porcupine.process(&input_frame) {
                Ok(keyword_index) => {
                    if keyword_index >= 0 {
                        // Hit the trigger word, start speech to text.
                        println!("Trigger word detected!");
                        total_transcript.clear();
                        //cringe.clear();

                        // Only one person can talk to the assistant at a time!
                        if let Ok(mut guard) = assistant.try_lock() {
                            if !guard.busy {
                                guard.busy = true;
                                guard.flush().await;
                                speech_to_text = true;
                                tx_audio.send((guild_id, AgentVoiceEvent::Acknowledge)).await.unwrap();
                                silence = Instant::now();
                            }
                        }
                    }
                },
                Err(e) => {
                    println!("Porcupine error: {}", e);
                }
            }
        }
    }
}

async fn read_frames(rx: &mut mpsc::Receiver<ListenerEvent>, buf: &mut VecDeque<i16>, n: usize, channels: usize) -> Option<Vec<Vec<f64>>> {
    let mut out = Vec::with_capacity(channels);
    for _ in 0..channels {
        out.push(Vec::with_capacity(n));
    }

    while buf.len() < n * 2 {
        match timeout(Duration::from_millis(100), rx.recv()).await {
            Ok(event) => {
                match event.unwrap() {
                    ListenerEvent::AudioPacket(data) => {
                        buf.extend(data);
                    },
                    ListenerEvent::Disconnect => {
                        return None;
                    }
                }
            },
            Err(_elapsed) => {
                let sample_count = (100 * 48) as usize;
                buf.extend(vec![0; sample_count]);
            }
        }
    }
    
    for _ in 0..n {
        let l = buf.pop_front().unwrap() as f64 / 32768.0;
        let r = buf.pop_front().unwrap() as f64 / 32768.0;
        out[0].push(l.clamp(-1.0, 1.0));
        out[1].push(r.clamp(-1.0, 1.0));
    }

    Some(out)
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
        .enable_automatic_punctuation(env::var("AUTO_PUNCTUATION").expect("Couldn't get env AUTO_PUNCTUATION!").parse().unwrap())
        .endpoint_duration_sec(env::var("ENDPOINT_DURATION").expect("Couldn't get env ENDPOINT_DURATION!").parse().unwrap())
        .init()
        .expect("Unable to create Cheetah")
}