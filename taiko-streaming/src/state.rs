use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TaikoGameState {
    MenuSelectSong,     // host send SongSelect
    MenuCheckResources, // peer may SongRequest, host may SongData, peer may SongReady
    MenuWaitForReady,   // host wait for SongReady from peer
    Personization,      // host and peer send CourseSelect, wait for others
    TimeSync,           // host and peer sync time
    GamePlay,           // host and peer send Key
    GameEnd,            // host and peer send GameEnd (sync all)
    Finish,             // host and peer send Finish (sync all)
}
