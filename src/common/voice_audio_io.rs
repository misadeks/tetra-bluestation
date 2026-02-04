#[cfg(feature = "voice-audio")]
mod imp {
    use std::sync::{Mutex, OnceLock};
    use std::thread;
    use std::time::Duration;

    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use ringbuf::{traits::*, HeapRb};

    use crate::common::bitbuffer::BitBuffer;
    use crate::common::etsi_codec;
    use crate::common::voice_state;

    const PCM_SAMPLE_RATE: u32 = 8000;
    const AUDIO_RING_SECONDS: usize = 4;
    const SPEECH_FRAME_BITS: usize = etsi_codec::SPEECH_FRAME_BITS;
    const SPEECH_FRAME_SAMPLES: usize = etsi_codec::SPEECH_FRAME_SAMPLES;
    const TCH_TYPE1_BITS: usize = etsi_codec::TCH_TYPE1_BITS;
    const PCM_TCH_SAMPLES: usize = SPEECH_FRAME_SAMPLES * 2;

    #[derive(Debug, Clone)]
    struct AudioRxFrame {
        bits: BitBuffer,
        crc_ok: bool,
    }

    struct VoiceAudioHandle {
        dec_tx: crossbeam_channel::Sender<AudioRxFrame>,
        enc_rx: crossbeam_channel::Receiver<BitBuffer>,
    }

    static VOICE_AUDIO_HANDLE: OnceLock<Mutex<Option<VoiceAudioHandle>>> = OnceLock::new();

    fn state() -> &'static Mutex<Option<VoiceAudioHandle>> {
        VOICE_AUDIO_HANDLE.get_or_init(|| Mutex::new(None))
    }

    pub fn ensure_started() {
        if !voice_state::get().enabled {
            return;
        }
        if !etsi_codec::available() {
            tracing::warn!("VOICE audio: ETSI codec not available; set ETSI_CODEC_DIR and rebuild");
            return;
        }
        if etsi_codec::speech_encode(&[0i16; SPEECH_FRAME_SAMPLES]).is_err() {
            tracing::warn!("VOICE audio: ETSI codec init failed; set ETSI_CODEC_DIR and rebuild");
            return;
        }

        let mut guard = state().lock().unwrap();
        if guard.is_some() {
            return;
        }

        let (dec_tx, dec_rx) = crossbeam_channel::bounded::<AudioRxFrame>(32);
        let (enc_tx, enc_rx) = crossbeam_channel::bounded::<BitBuffer>(32);

        thread::spawn(move || {
            if let Err(err) = audio_thread(dec_rx, enc_tx) {
                tracing::warn!("VOICE audio thread exited: {}", err);
            }
        });

        *guard = Some(VoiceAudioHandle { dec_tx, enc_rx });
    }

    pub fn push_ul_tch(bits: BitBuffer, crc_ok: bool) {
        ensure_started();
        if bits.get_len() != TCH_TYPE1_BITS {
            tracing::warn!("VOICE audio: unexpected UL TCH length {}", bits.get_len());
            return;
        }
        let guard = state().lock().unwrap();
        let Some(handle) = guard.as_ref() else { return; };
        let _ = handle.dec_tx.try_send(AudioRxFrame { bits, crc_ok });
    }

    pub fn try_take_tx_tch() -> Option<BitBuffer> {
        ensure_started();
        let guard = state().lock().unwrap();
        let Some(handle) = guard.as_ref() else { return None; };
        handle.enc_rx.try_recv().ok()
    }

    pub fn has_pending_tx() -> bool {
        let guard = state().lock().unwrap();
        let Some(handle) = guard.as_ref() else { return false; };
        !handle.enc_rx.is_empty()
    }

    fn audio_thread(
        dec_rx: crossbeam_channel::Receiver<AudioRxFrame>,
        enc_tx: crossbeam_channel::Sender<BitBuffer>,
    ) -> anyhow::Result<()> {
        let (in_stream, input_cons) = build_input_stream()?;
        let (out_stream, output_prod) = build_output_stream()?;

        let mut output_prod = output_prod;
        let dec_thread = thread::spawn(move || {
            while let Ok(frame) = dec_rx.recv() {
                if let Ok(pcm) = decode_tch_to_pcm(frame.bits, frame.crc_ok) {
                    for s in pcm {
                        let _ = output_prod.try_push(s);
                    }
                }
            }
        });

        let mut input_cons = input_cons;
        let enc_thread = thread::spawn(move || {
            let mut frame = Vec::with_capacity(PCM_TCH_SAMPLES);
            loop {
                while frame.len() < PCM_TCH_SAMPLES {
                    if let Some(sample) = input_cons.try_pop() {
                        frame.push(sample);
                    } else {
                        thread::sleep(Duration::from_millis(5));
                    }
                }
                if let Ok(bits) = encode_pcm_to_tch(&frame) {
                    let _ = enc_tx.try_send(bits);
                }
                frame.clear();
            }
        });

        // Keep streams alive and threads running.
        let _dec_thread = dec_thread;
        let _enc_thread = enc_thread;
        let _in_stream = in_stream;
        let _out_stream = out_stream;
        loop {
            thread::sleep(Duration::from_secs(3600));
        }
    }

    fn decode_tch_to_pcm(bits: BitBuffer, crc_ok: bool) -> Result<Vec<i16>, etsi_codec::CodecError> {
        let raw = bitbuffer_to_bits(bits);
        if raw.len() != TCH_TYPE1_BITS {
            return Err(etsi_codec::CodecError::InvalidLen { expected: TCH_TYPE1_BITS, got: raw.len() });
        }
        let mut pcm = Vec::with_capacity(PCM_TCH_SAMPLES);
        let bfi = !crc_ok;

        let mut frame_bits = [0u8; SPEECH_FRAME_BITS];
        frame_bits.copy_from_slice(&raw[0..SPEECH_FRAME_BITS]);
        let pcm1 = etsi_codec::speech_decode(&frame_bits, bfi)?;
        pcm.extend_from_slice(&pcm1);

        frame_bits.copy_from_slice(&raw[SPEECH_FRAME_BITS..TCH_TYPE1_BITS]);
        let pcm2 = etsi_codec::speech_decode(&frame_bits, bfi)?;
        pcm.extend_from_slice(&pcm2);

        Ok(pcm)
    }

    fn encode_pcm_to_tch(pcm: &[i16]) -> Result<BitBuffer, etsi_codec::CodecError> {
        if pcm.len() < PCM_TCH_SAMPLES {
            return Err(etsi_codec::CodecError::InvalidLen { expected: PCM_TCH_SAMPLES, got: pcm.len() });
        }
        let mut frame1 = [0i16; SPEECH_FRAME_SAMPLES];
        let mut frame2 = [0i16; SPEECH_FRAME_SAMPLES];
        frame1.copy_from_slice(&pcm[..SPEECH_FRAME_SAMPLES]);
        frame2.copy_from_slice(&pcm[SPEECH_FRAME_SAMPLES..PCM_TCH_SAMPLES]);

        let bits1 = etsi_codec::speech_encode(&frame1)?;
        let bits2 = etsi_codec::speech_encode(&frame2)?;

        let mut combined = Vec::with_capacity(TCH_TYPE1_BITS);
        combined.extend_from_slice(&bits1);
        combined.extend_from_slice(&bits2);

        let mut buf = BitBuffer::from_bitarr(&combined);
        buf.seek(0);
        Ok(buf)
    }

    fn bitbuffer_to_bits(mut buf: BitBuffer) -> Vec<u8> {
        let len = buf.get_len();
        buf.seek(0);
        let mut raw = vec![0u8; len];
        buf.to_bitarr(&mut raw);
        raw
    }

    fn build_input_stream() -> anyhow::Result<(cpal::Stream, ringbuf::HeapCons<i16>)> {
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or_else(|| anyhow::anyhow!("no input audio device"))?;

        let (config, format) = select_config(&device, true)?;
        let rb = HeapRb::<i16>::new(PCM_SAMPLE_RATE as usize * AUDIO_RING_SECONDS);
        let (mut prod, cons) = rb.split();

        let err_fn = |err| tracing::warn!("VOICE audio input error: {}", err);

        let stream = match format {
            cpal::SampleFormat::I16 => device.build_input_stream(
                &config,
                move |data: &[i16], _| {
                    for &s in data {
                        let _ = prod.try_push(s);
                    }
                },
                err_fn,
                None,
            )?,
            cpal::SampleFormat::U16 => device.build_input_stream(
                &config,
                move |data: &[u16], _| {
                    for &s in data {
                        let v = (s as i32 - 32768) as i16;
                        let _ = prod.try_push(v);
                    }
                },
                err_fn,
                None,
            )?,
            cpal::SampleFormat::F32 => device.build_input_stream(
                &config,
                move |data: &[f32], _| {
                    for &s in data {
                        let v = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
                        let _ = prod.try_push(v);
                    }
                },
                err_fn,
                None,
            )?,
            _ => return Err(anyhow::anyhow!("unsupported input sample format")),
        };

        stream.play()?;
        Ok((stream, cons))
    }

    fn build_output_stream() -> anyhow::Result<(cpal::Stream, ringbuf::HeapProd<i16>)> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| anyhow::anyhow!("no output audio device"))?;

        let (config, format) = select_config(&device, false)?;
        let rb = HeapRb::<i16>::new(PCM_SAMPLE_RATE as usize * AUDIO_RING_SECONDS);
        let (prod, mut cons) = rb.split();

        let err_fn = |err| tracing::warn!("VOICE audio output error: {}", err);

        let stream = match format {
            cpal::SampleFormat::I16 => device.build_output_stream(
                &config,
                move |data: &mut [i16], _| {
                    for out in data.iter_mut() {
                        *out = cons.try_pop().unwrap_or(0);
                    }
                },
                err_fn,
                None,
            )?,
            cpal::SampleFormat::U16 => device.build_output_stream(
                &config,
                move |data: &mut [u16], _| {
                    for out in data.iter_mut() {
                        let s = cons.try_pop().unwrap_or(0);
                        *out = (s as i32 + 32768).clamp(0, u16::MAX as i32) as u16;
                    }
                },
                err_fn,
                None,
            )?,
            cpal::SampleFormat::F32 => device.build_output_stream(
                &config,
                move |data: &mut [f32], _| {
                    for out in data.iter_mut() {
                        let s = cons.try_pop().unwrap_or(0);
                        *out = (s as f32) / (i16::MAX as f32);
                    }
                },
                err_fn,
                None,
            )?,
            _ => return Err(anyhow::anyhow!("unsupported output sample format")),
        };

        stream.play()?;
        Ok((stream, prod))
    }

    fn select_config(device: &cpal::Device, is_input: bool) -> anyhow::Result<(cpal::StreamConfig, cpal::SampleFormat)> {
        let mut best: Option<cpal::SupportedStreamConfig> = None;
        let configs = if is_input {
            device.supported_input_configs()?.collect::<Vec<_>>()
        } else {
            device.supported_output_configs()?.collect::<Vec<_>>()
        };
        for cfg in configs {
            let fmt = cfg.sample_format();
            if cfg.channels() != 1 {
                continue;
            }
            if cfg.min_sample_rate().0 > PCM_SAMPLE_RATE || cfg.max_sample_rate().0 < PCM_SAMPLE_RATE {
                continue;
            }
            if best.is_none() || fmt == cpal::SampleFormat::I16 {
                best = Some(cfg.with_sample_rate(cpal::SampleRate(PCM_SAMPLE_RATE)));
                if fmt == cpal::SampleFormat::I16 {
                    break;
                }
            }
        }
        let chosen = best.ok_or_else(|| anyhow::anyhow!("no 8kHz mono config available"))?;
        let format = chosen.sample_format();
        Ok((chosen.config(), format))
    }
}

#[cfg(not(feature = "voice-audio"))]
mod imp {
    use crate::common::bitbuffer::BitBuffer;

    pub fn ensure_started() {}

    pub fn push_ul_tch(_bits: BitBuffer, _crc_ok: bool) {}

    pub fn try_take_tx_tch() -> Option<BitBuffer> { None }

    pub fn has_pending_tx() -> bool { false }
}

pub use imp::{ensure_started, has_pending_tx, push_ul_tch, try_take_tx_tch};
