use std::{env, collections::VecDeque, time::Duration, sync::Arc};
use tokio::{sync::{mpsc, Mutex}, time::{timeout, Instant}};
//use wav::WAV_FORMAT_PCM;
//use std::fs::OpenOptions;

// Resampling
use rubato::{Resampler, SincFixedOut, SincInterpolationType, SincInterpolationParameters, WindowFunction};

// APIs
use cheetah::{CheetahBuilder, Cheetah};
use porcupine::{PorcupineBuilder, Porcupine};

use crate::assistant::DiscordAssistant;

pub enum ListenerEvent {
    AudioPacket(Vec<i16>),
    Disconnect
}

pub async fn listener_loop(
    rx_48khz: &mut mpsc::Receiver<ListenerEvent>,
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

    let mut total_transcript = String::new();
    // TODO remove
    //let mut cringe = Vec::<i16>::with_capacity(16_000 * 10);

    let mut speech_to_text = false;
    // Track when the user is waiting for the agent to finish responding.
    // Once the agent has finished responding, start measuring silence to cancel the conversation.
    let mut awaiting_agent_response = false;
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

        // Stereo to mono
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
            
            let partial_transcript = cheetah.process(&input_frame).unwrap();

            if awaiting_agent_response {
                let agent_responding = {
                    let guard = assistant.lock().await;
                    guard.is_responding().await
                };

                if !agent_responding {
                    silence = Instant::now();
                    awaiting_agent_response = false;
                }
            }

            if partial_transcript.transcript.is_empty() {
                if silence.elapsed() > Duration::from_millis(3000) {
                    // User hasn't said anything for 3seconds, end the convo.
                    // TODO
                    // let mut guard = assistant.lock().await;
                    // if !guard.is_responding().await {
                    //     assert!(guard.is_in_conversation);
                    //     awaiting_agent_response = false;
                    //     speech_to_text = false;
                    //     guard.is_in_conversation = false;
                    //     println!("user stopped talking, ending...");
                    //     continue;
                    // }
                }
            }     
            else {
                silence = Instant::now();
            }       

            total_transcript.push_str(&partial_transcript.transcript);

            if partial_transcript.is_endpoint {
                let final_transcript = cheetah.flush().unwrap();
                total_transcript.push_str(&final_transcript.transcript);

                println!("final sentence: {}", total_transcript);

                let guard: tokio::sync::MutexGuard<'_, DiscordAssistant> = assistant.lock().await;
                assert!(guard.is_in_conversation);

                if guard.is_responding().await {
                    // TODO less naive version
                    if total_transcript.contains("stop") {
                        let guard = assistant.lock().await;
                        guard.stop().await;
                    }
                } else {
                    awaiting_agent_response = true;

                    // Play waiting sound
                    guard.speaker.start_ping().await;

                    // Prompt the agent and respond
                    let assistant = assistant.clone();
                    let speaker_transcript = total_transcript.clone();
                    tokio::spawn(async move {
                        let mut guard: tokio::sync::MutexGuard<'_, DiscordAssistant> = assistant.lock().await;
                        guard.send_message(&speaker_transcript).await;
                    });
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
        } else {
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
                            if !guard.is_in_conversation {
                                guard.is_in_conversation = true;
                                guard.flush().await;
                                guard.speaker.acknowledge().await;

                                speech_to_text = true;
                                awaiting_agent_response = false;
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