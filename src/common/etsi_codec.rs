use crate::common::bitbuffer::BitBuffer;

pub const SPEECH_FRAME_BITS: usize = 137;
pub const SPEECH_FRAME_SAMPLES: usize = 240;
pub const TCH_TYPE1_BITS: usize = 274;
pub const TCH_ENCODED_BITS: usize = 432;

#[derive(Debug)]
pub enum CodecError {
    Unavailable,
    InvalidLen { expected: usize, got: usize },
}

impl std::fmt::Display for CodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CodecError::Unavailable => write!(f, "ETSI codec unavailable"),
            CodecError::InvalidLen { expected, got } => {
                write!(f, "invalid length (expected {}, got {})", expected, got)
            }
        }
    }
}

impl std::error::Error for CodecError {}

#[cfg(etsi_codec)]
mod etsi {
    use super::{CodecError, SPEECH_FRAME_BITS, SPEECH_FRAME_SAMPLES, TCH_ENCODED_BITS, TCH_TYPE1_BITS};
    use crate::common::bitbuffer::BitBuffer;
    use crate::entities::lmac::components::scramble::scrambler;
    use std::ptr;
    use std::sync::{Mutex, OnceLock};

    type Word16 = i16;

    unsafe extern "C" {
        fn Channel_Encoding(first_pass: i16, frame_stealing: Word16, input: *const Word16, output: *mut Word16);
        fn Channel_Decoding(first_pass: i16, frame_stealing: Word16, input: *const Word16, output: *mut Word16) -> Word16;

        fn Init_Pre_Process();
        fn Init_Coder_Tetra();
        fn Pre_Process(signal: *mut Word16, lg: Word16);
        fn Coder_Tetra(ana: *mut Word16, synth: *mut Word16);
        fn Prm2bits_Tetra(prm: *const Word16, bits: *mut Word16);

        fn Init_Decod_Tetra();
        fn Bits2prm_Tetra(bits: *const Word16, prm: *mut Word16);
        fn Decod_Tetra(parm: *mut Word16, synth: *mut Word16);
        fn Post_Process(signal: *mut Word16, lg: Word16);

        static mut new_speech: *mut Word16;
    }

    #[derive(Default)]
    struct EtsiState {
        chan_enc_init: bool,
        chan_dec_init: bool,
        speech_enc_init: bool,
        speech_dec_init: bool,
    }

    static ETSI_STATE: OnceLock<Mutex<EtsiState>> = OnceLock::new();

    fn state() -> &'static Mutex<EtsiState> {
        ETSI_STATE.get_or_init(|| Mutex::new(EtsiState::default()))
    }

    fn bits_to_word16(buf: &BitBuffer, out: &mut [Word16]) -> Result<(), CodecError> {
        if buf.get_len() != out.len() {
            return Err(CodecError::InvalidLen { expected: out.len(), got: buf.get_len() });
        }
        let mut tmp = vec![0u8; out.len()];
        let mut copy = buf.clone();
        copy.seek(0);
        copy.to_bitarr(&mut tmp);
        for (dst, src) in out.iter_mut().zip(tmp.iter()) {
            *dst = *src as Word16;
        }
        Ok(())
    }

    fn word16_to_bits(input: &[Word16]) -> BitBuffer {
        let mut tmp = vec![0u8; input.len()];
        for (dst, src) in tmp.iter_mut().zip(input.iter()) {
            *dst = (*src & 1) as u8;
        }
        let mut buf = BitBuffer::from_bitarr(&tmp);
        buf.seek(0);
        buf
    }

    pub fn channel_encode_tch(bits: &BitBuffer, scrambling_code: u32) -> Result<BitBuffer, CodecError> {
        if bits.get_len() != TCH_TYPE1_BITS {
            return Err(CodecError::InvalidLen { expected: TCH_TYPE1_BITS, got: bits.get_len() });
        }

        let mut state = state().lock().unwrap();
        let first_pass = if state.chan_enc_init { 0 } else { state.chan_enc_init = true; 1 };

        let mut input = [0i16; TCH_TYPE1_BITS];
        let mut output = [0i16; TCH_ENCODED_BITS];
        bits_to_word16(bits, &mut input)?;

        unsafe {
            Channel_Encoding(first_pass, 0, input.as_ptr(), output.as_mut_ptr());
        }

        let mut buf = word16_to_bits(&output);
        buf.seek(0);
        scrambler::tetra_scramb_bits(scrambling_code, &mut buf);
        buf.seek(0);
        Ok(buf)
    }

    pub fn channel_decode_tch(bits: &BitBuffer, scrambling_code: u32) -> Result<(BitBuffer, bool), CodecError> {
        if bits.get_len() != TCH_ENCODED_BITS {
            return Err(CodecError::InvalidLen { expected: TCH_ENCODED_BITS, got: bits.get_len() });
        }

        let mut state = state().lock().unwrap();
        let first_pass = if state.chan_dec_init { 0 } else { state.chan_dec_init = true; 1 };

        let mut scrambled = bits.clone();
        scrambled.seek(0);
        scrambler::tetra_scramb_bits(scrambling_code, &mut scrambled);
        scrambled.seek(0);

        let mut input = [0i16; TCH_ENCODED_BITS];
        let mut output = [0i16; TCH_TYPE1_BITS];
        bits_to_word16(&scrambled, &mut input)?;

        let bfi = unsafe { Channel_Decoding(first_pass, 0, input.as_ptr(), output.as_mut_ptr()) };
        let crc_ok = bfi == 0;
        Ok((word16_to_bits(&output), crc_ok))
    }

    pub fn speech_encode(pcm: &[Word16; SPEECH_FRAME_SAMPLES]) -> Result<[u8; SPEECH_FRAME_BITS], CodecError> {
        let mut state = state().lock().unwrap();
        if !state.speech_enc_init {
            unsafe {
                Init_Pre_Process();
                Init_Coder_Tetra();
            }
            state.speech_enc_init = true;
        }

        let mut ana = [0i16; 23];
        let mut synth = [0i16; SPEECH_FRAME_SAMPLES];
        let mut serial = [0i16; 138];

        unsafe {
            if new_speech.is_null() {
                return Err(CodecError::Unavailable);
            }
            ptr::copy_nonoverlapping(pcm.as_ptr(), new_speech, SPEECH_FRAME_SAMPLES);
            Pre_Process(new_speech, SPEECH_FRAME_SAMPLES as Word16);
            Coder_Tetra(ana.as_mut_ptr(), synth.as_mut_ptr());
            Prm2bits_Tetra(ana.as_ptr(), serial.as_mut_ptr());
        }

        let mut out = [0u8; SPEECH_FRAME_BITS];
        for i in 0..SPEECH_FRAME_BITS {
            out[i] = (serial[i + 1] & 1) as u8;
        }
        Ok(out)
    }

    pub fn speech_decode(bits: &[u8; SPEECH_FRAME_BITS], bfi: bool) -> Result<[Word16; SPEECH_FRAME_SAMPLES], CodecError> {
        let mut state = state().lock().unwrap();
        if !state.speech_dec_init {
            unsafe {
                Init_Decod_Tetra();
            }
            state.speech_dec_init = true;
        }

        let mut serial = [0i16; 138];
        serial[0] = if bfi { 1 } else { 0 };
        for i in 0..SPEECH_FRAME_BITS {
            serial[i + 1] = bits[i] as i16;
        }

        let mut prm = [0i16; 24];
        let mut synth = [0i16; SPEECH_FRAME_SAMPLES];

        unsafe {
            Bits2prm_Tetra(serial.as_ptr(), prm.as_mut_ptr());
            Decod_Tetra(prm.as_mut_ptr(), synth.as_mut_ptr());
            Post_Process(synth.as_mut_ptr(), SPEECH_FRAME_SAMPLES as Word16);
        }

        Ok(synth)
    }
}

#[cfg(not(etsi_codec))]
mod etsi {
    use super::{CodecError, SPEECH_FRAME_BITS, SPEECH_FRAME_SAMPLES, TCH_ENCODED_BITS, TCH_TYPE1_BITS};
    use crate::common::bitbuffer::BitBuffer;

    pub fn channel_encode_tch(_bits: &BitBuffer, _scrambling_code: u32) -> Result<BitBuffer, CodecError> {
        Err(CodecError::Unavailable)
    }

    pub fn channel_decode_tch(_bits: &BitBuffer, _scrambling_code: u32) -> Result<(BitBuffer, bool), CodecError> {
        Err(CodecError::Unavailable)
    }

    pub fn speech_encode(_pcm: &[i16; SPEECH_FRAME_SAMPLES]) -> Result<[u8; SPEECH_FRAME_BITS], CodecError> {
        Err(CodecError::Unavailable)
    }

    pub fn speech_decode(_bits: &[u8; SPEECH_FRAME_BITS], _bfi: bool) -> Result<[i16; SPEECH_FRAME_SAMPLES], CodecError> {
        Err(CodecError::Unavailable)
    }

    #[allow(dead_code)]
    const _TCH_ENCODED_BITS: usize = TCH_ENCODED_BITS;
    #[allow(dead_code)]
    const _TCH_TYPE1_BITS: usize = TCH_TYPE1_BITS;
}

pub fn channel_encode_tch(bits: &BitBuffer, scrambling_code: u32) -> Result<BitBuffer, CodecError> {
    etsi::channel_encode_tch(bits, scrambling_code)
}

pub fn channel_decode_tch(bits: &BitBuffer, scrambling_code: u32) -> Result<(BitBuffer, bool), CodecError> {
    etsi::channel_decode_tch(bits, scrambling_code)
}

pub fn speech_encode(pcm: &[i16; SPEECH_FRAME_SAMPLES]) -> Result<[u8; SPEECH_FRAME_BITS], CodecError> {
    etsi::speech_encode(pcm)
}

pub fn speech_decode(bits: &[u8; SPEECH_FRAME_BITS], bfi: bool) -> Result<[i16; SPEECH_FRAME_SAMPLES], CodecError> {
    etsi::speech_decode(bits, bfi)
}

pub fn available() -> bool {
    cfg!(etsi_codec)
}
