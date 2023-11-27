use std::{fs::OpenOptions, env, collections::VecDeque, time::Duration};
use tokio::{sync::mpsc, time::timeout};
use wav::WAV_FORMAT_PCM;

// Resampling
use rubato::{Resampler, SincFixedOut, SincInterpolationType, SincInterpolationParameters, WindowFunction};

// APIs
use cheetah::{CheetahBuilder, Cheetah};
use porcupine::{PorcupineBuilder, Porcupine};
use async_openai::{
    types::{CreateSpeechRequestArgs, SpeechModel, Voice},
    Client as OpenAIClient,
};

pub enum ListenerEvent {
    AudioPacket(Vec<i16>),
    Disconnect
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

pub async fn listener_loop(rx_48khz: &mut mpsc::Receiver<ListenerEvent>) {
    let porcupine = init_porcupine();
    let cheetah: Cheetah = init_cheetah();

    assert!(porcupine.sample_rate() == cheetah.sample_rate());
    assert!(cheetah.frame_length() == porcupine.frame_length());

    let oai_client = OpenAIClient::new();

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
    let mut cringe = Vec::<i16>::with_capacity(16_000 * 10);

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
            cringe.extend(input_frame.clone());
            
            let partial_transcript = cheetah.process(&input_frame).unwrap();
            total_transcript.push_str(&partial_transcript.transcript);

            if partial_transcript.is_endpoint {
                speech_to_text = false;

                let final_transcript = cheetah.flush().unwrap();
                total_transcript.push_str(&final_transcript.transcript);

                println!("{}", total_transcript);

                let voice_request: async_openai::types::CreateSpeechRequest = CreateSpeechRequestArgs::default()
                   .input(total_transcript.clone())
                   .voice(Voice::Onyx)
                   .model(SpeechModel::Tts1)
                   .build().unwrap();

                match oai_client.audio().speech(voice_request).await {
                    Ok(result) => {
                        result.save("./resources/output/openai_output.mp3").await.unwrap();
                    },
                    Err(e) => {
                        println!("{}", e.to_string());
                        continue;
                    } 
                }

                let mut path1 = std::env::current_dir().expect("Couldn't get CWD!");
                path1.push("resources");
                path1.push("output");
                path1.push(format!("output_16.wav"));

                let mut file1 = OpenOptions::new().write(true).create(true).open(path1).unwrap();

                let bit_depth = wav::bit_depth::BitDepth::Sixteen(cringe.iter().cloned().collect());
                let header1 = wav::Header::new(WAV_FORMAT_PCM, 1, porcupine.sample_rate(), 16);
                wav::write(header1, &bit_depth, &mut file1).unwrap();
            }
        }
        else {
            // Listening in for trigger word
            match porcupine.process(&input_frame) {
                Ok(keyword_index) => {
                    if keyword_index >= 0 {
                        // Hit the trigger word, start speech to text
                        speech_to_text = true;
                        total_transcript.clear();
                        cringe.clear();
                        println!("bozo detected!");
                    }
                },
                Err(e) => {
                    println!("Porcupine error: {}", e);
                }
            }
        }
    }
}