use lazy_static;

lazy_static::lazy_static! {
    pub static ref DON_WAV: Vec<u8> = include_bytes!("../assets/don.wav").to_vec();
    pub static ref KAT_WAV: Vec<u8> = include_bytes!("../assets/kat.wav").to_vec();
}
