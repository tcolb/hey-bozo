use std::{time::Duration, sync::Arc, env};
use tokio::{sync::{mpsc, Mutex}, time::Instant};
//use wav::WAV_FORMAT_PCM;
//use std::fs::OpenOptions;

// APIs
use cheetah::{CheetahBuilder, Cheetah};
use porcupine::{PorcupineBuilder, Porcupine};

use crate::{assistant::DiscordAssistant, resampler::{ListenerEvent, self}};

pub async fn listener_loop(
    rx_48khz: mpsc::Receiver<ListenerEvent>,
    assistant: Arc<Mutex<DiscordAssistant>>) {

    let porcupine = init_porcupine();
    let cheetah: Cheetah = init_cheetah();

    assert!(porcupine.sample_rate() == cheetah.sample_rate());
    assert!(cheetah.frame_length() == porcupine.frame_length());
    
    let mut resampler = resampler::Resampler::new(rx_48khz, porcupine.sample_rate() as f64, porcupine.frame_length() as usize, 2);
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
        if !resampler.read_frames_resample(&mut input_frame).await {
            // Client disconnected!
            break;
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
                    //TODO
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

            total_transcript.push_str(&partial_transcript.transcript);

            if partial_transcript.is_endpoint {
                let final_transcript = cheetah.flush().unwrap();
                total_transcript.push_str(&final_transcript.transcript);

                println!("final sentence: {}", total_transcript);

                let guard: tokio::sync::MutexGuard<'_, DiscordAssistant> = assistant.lock().await;
                assert!(guard.is_in_conversation);

                if guard.is_responding().await {
                    // TODO less naive version
                    if total_transcript.ends_with("stop") {
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