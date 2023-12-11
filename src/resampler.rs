use std::{collections::VecDeque, time::Duration};

use rubato::{SincInterpolationParameters, SincInterpolationType, WindowFunction, SincFixedOut, Resampler as RubatoResampler};
use tokio::{sync::mpsc, time::timeout};

pub enum ListenerEvent {
    AudioPacket(Vec<i16>),
    Disconnect
}

pub struct Resampler {
    rx: mpsc::Receiver<ListenerEvent>,
    buf: VecDeque<i16>,
    channels: usize,
    resampler: SincFixedOut<f64>
}

impl Resampler {
    pub fn new(rx: mpsc::Receiver<ListenerEvent>, sample_rate: f64, frame_length: usize, channels: usize) -> Self {
        let resampler_params = SincInterpolationParameters {
            sinc_len: 256,
            f_cutoff: 0.95,
            interpolation: SincInterpolationType::Linear,
            oversampling_factor: 256,
            window: WindowFunction::BlackmanHarris2,
        };
    
        let resampler = SincFixedOut::<f64>::new(
            sample_rate / 48_000 as f64,
            2.0,
            resampler_params,
            frame_length,
            2,
        ).unwrap();

        Self {
            rx: rx,
            buf: VecDeque::with_capacity(frame_length * 2),
            channels: channels,
            resampler: resampler
        }
    }

    async fn read_frames(&mut self) -> Option<Vec<Vec<f64>>> {
        let frame_count = self.resampler.input_frames_next(); 

        let mut out = Vec::with_capacity(self.channels);
        for _ in 0..self.channels {
            out.push(Vec::with_capacity(frame_count));
        }
    
        while self.buf.len() < frame_count * 2 {
            match timeout(Duration::from_millis(100), self.rx.recv()).await {
                Ok(event) => {
                    match event.unwrap() {
                        ListenerEvent::AudioPacket(data) => {
                            self.buf.extend(data);
                        },
                        ListenerEvent::Disconnect => {
                            return None;
                        }
                    }
                },
                Err(_elapsed) => {
                    let sample_count = (100 * 48) as usize;
                    self.buf.extend(vec![0; sample_count]);
                }
            }
        }
        
        for _ in 0..frame_count {
            let l = self.buf.pop_front().unwrap() as f64 / 32768.0;
            let r = self.buf.pop_front().unwrap() as f64 / 32768.0;
            out[0].push(l.clamp(-1.0, 1.0));
            out[1].push(r.clamp(-1.0, 1.0));
        }
    
        Some(out)
    }

    pub async fn read_frames_resample(&mut self, out_frame: &mut Vec<i16>) -> bool {

        let frames = self.read_frames().await;

        // TODO none check
        if frames.is_none() {
            return false;
        }

        let resampled_frame = self.resampler.process(&frames.unwrap(), None).unwrap();

        // Stereo to mono
        out_frame.clear();
        for i in 0..resampled_frame[0].len() {
            let l = resampled_frame[0][i];
            let r = resampled_frame[1][i];
            let v = (l + r) / 2.0;
            out_frame.push((v * 32768.0).clamp(-32768.0, 32768.0) as i16);
        }

        return true;
    }
}