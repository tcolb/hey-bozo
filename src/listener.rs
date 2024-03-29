use std::{time::Duration, sync::Arc, env, io::Cursor};
use tokio::{sync::{mpsc, Mutex}, time::Instant};
use wav::WAV_FORMAT_PCM;

use cobra::Cobra;
use porcupine::{PorcupineBuilder, Porcupine};
use async_openai::types::AudioInput;

use crate::{assistant::DiscordAssistant, resampler::{ListenerEvent, self}};

enum ConversationState {
    Detection,
    Listening,
    Responding,
}

pub async fn listener_loop(
    rx_48khz: mpsc::Receiver<ListenerEvent>,
    assistant: Arc<Mutex<DiscordAssistant>>,
    ssrc: u32) {

    let porcupine = init_porcupine();
    let cobra: Cobra = init_cobra();

    assert!(porcupine.sample_rate() == cobra.sample_rate());
    assert!(cobra.frame_length() == porcupine.frame_length());
    let sample_rate = porcupine.sample_rate();

    let mut resampler = resampler::Resampler::new(rx_48khz, sample_rate as f64, porcupine.frame_length() as usize, 2);
    let mut input_frame = Vec::<i16>::with_capacity(porcupine.frame_length() as  usize);

    let mut conversation_state = ConversationState::Detection;
    let mut time_listening: Option<Instant> = None;
    let mut time_not_speaking: Option<Instant> = None;
    let mut transcription_audio = Vec::<i16>::default();

    loop {
        // Consume packets
        if !resampler.read_frames_resample(&mut input_frame).await {
            // Client disconnected!
            break;
        } 

        match conversation_state {
            ConversationState::Detection => {
                // Listening in for the trigger word.
                match porcupine.process(&input_frame) {
                    Ok(keyword_index) => {
                        if keyword_index >= 0 {
                            // Hit the trigger word, start speech to text.
                            println!("Trigger word detected!");

                            // Only one person can talk to the assistant at a time!
                            if let Ok(mut guard) = assistant.try_lock() {
                                if guard.try_grab_attention(ssrc) {
                                    guard.flush().await;
                                    guard.speaker.acknowledge().await;

                                    conversation_state = ConversationState::Listening;
                                    println!("listening");
                                    time_not_speaking = None;
                                    transcription_audio.clear();
                                }
                            }
                        }
                    },
                    Err(e) => {
                        println!("Porcupine error: {}", e);
                    }
                }
            },
            ConversationState::Listening => {
                let speaking_confidence = cobra.process(&input_frame).unwrap();
                transcription_audio.append(&mut input_frame);

                if speaking_confidence < 0.75 {
                    if time_not_speaking.is_none() {
                        time_not_speaking = Some(Instant::now());
                    }
                }
                else {
                    time_not_speaking = None;
                }

                if let Some(time_not_speaking_instant) = time_not_speaking {
                    if time_not_speaking_instant.elapsed() >= Duration::from_secs(3) {

                        if let Some(time_listening_instant) = time_listening {
                            let listening_speaking_delta = time_listening_instant.elapsed() - time_not_speaking_instant.elapsed();
                            if listening_speaking_delta.as_millis() < 500 {
                                conversation_state = ConversationState::Detection;
                                println!("detection");
                                transcription_audio.clear();
                                time_not_speaking = None;
                                time_listening = None;
                                let mut guard: tokio::sync::MutexGuard<'_, DiscordAssistant> = assistant.lock().await;
                                guard.try_clear_attention(ssrc);
                                continue;
                            }
                        }

                        conversation_state = ConversationState::Responding;
                        println!("responding");

                        let guard: tokio::sync::MutexGuard<'_, DiscordAssistant> = assistant.lock().await;
                        guard.set_responding();
    
                        // Play waiting sound
                        guard.speaker.start_ping().await;
        
                        let transcription_buf = transcription_audio.clone();
                        transcription_audio.clear();

                        // Prompt the agent and respond
                        let mut bytes = Cursor::new(vec![]);
                        let bit_depth = wav::bit_depth::BitDepth::Sixteen(transcription_buf);
                        let header = wav::Header::new(WAV_FORMAT_PCM, 1, sample_rate, 16);
                        wav::write(header, &bit_depth, &mut bytes).unwrap();
                        let audio_input: AudioInput = AudioInput::from_bytes("dummy.wav".into(), bytes::Bytes::from(bytes.into_inner()));
                        let assistant = assistant.clone();
                        tokio::spawn(async move {
                            let mut guard: tokio::sync::MutexGuard<'_, DiscordAssistant> = assistant.lock().await;
                            guard.send_message(audio_input).await;
                        });
    
                    }
                }
            },
            ConversationState::Responding => {
                let guard = assistant.lock().await;
                if !guard.is_responding().await {
                    time_not_speaking = None;
                    time_listening = Some(Instant::now());
                    transcription_audio.clear();

                    match guard.get_attention_id() {
                        Some(id) => {
                            assert!(id == ssrc);
                            conversation_state = ConversationState::Listening;
                            println!("listening");
                        },
                        None => {
                            // We lost the bots attention, likely the bot has finished the conversation.
                            conversation_state = ConversationState::Detection;
                            println!("detection");
                        }
                    }
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

fn init_cobra() -> Cobra {
    Cobra::new(env::var("PV_KEY").expect("Couldn't get env PV_KEY!"))
        .expect("Unable to create Cheetah")
}