# taiko-game

A taiko game written in Rust.

## Keybindings

- `DON`: `' ' | 'f' | 'g' | 'h' | 'j' | 'c' | 'v' | 'b' | 'n' | 'm'`
- `KAT`: `'d' | 's' | 'a' | 't' | 'r' | 'e' | 'w' | 'q' | 'x' | 'z' | 'k' | 'l' | ';' | '\'' | 'y' | 'u' | 'i' | 'o' | 'p' | ',' | '.' | '/'`
- `CANCEL`: `ESC`

```mermaid
graph TD
    LocalInput[Local Input Device]
    RemoteInput[Remote Input Device]
    InputMixer[Input Mixer]
    GameEngine[Game Engine]
    UI[UI]
    LocalOutput[Local Output Device]
    LocalPeer[Local Peer]
    RemotePeer[Remote Peer]

    LocalInput --> |Key Event| InputMixer
    RemoteInput --> |Key Event| RemotePeer
    RemotePeer --> |Data| LocalPeer
    LocalPeer --> |Key Event| InputMixer
    InputMixer --> |Key Event| GameEngine
    GameEngine --> |State| UI
    UI --> |Frame| LocalOutput
```
