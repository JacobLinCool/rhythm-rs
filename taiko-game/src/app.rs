use color_eyre::eyre::Result;
use kira::sound::static_sound::StaticSoundSettings;
use ratatui::prelude::Rect;
use ratatui::widgets::*;
use std::time::Duration;
use taiko_core::Personalization;
use taiko_streaming::{
    generate_uid, StreamingClient, StreamingServer, WebSocketStreamingClient,
    WebSocketStreamingServer,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use taiko_core::Hit;

use crate::cli::AppArgs;
use crate::trace_dbg;
use crate::{
    action::Action,
    audio::AppAudio,
    input::InputMixer,
    tui,
    uix::{Page, UI},
};
use crate::{
    audio::MusicInstruction,
    loader::{PlaylistLoader, Song},
};

pub struct App {
    pub ui: UI,
    pub input: InputMixer,
    pub state: AppGlobalState,
}

pub struct AppGlobalState {
    pub args: AppArgs,
    pub audio: AppAudio,
    pub client: Option<WebSocketStreamingClient<Hit, Personalization>>,
    pub schedule_cancellation: Option<CancellationToken>,
}

impl AppGlobalState {
    pub fn schedule_demo(&mut self, song: Song) {
        if let Some(token) = self.schedule_cancellation.as_ref() {
            token.cancel();
        }
        let token = CancellationToken::new();
        let cloned_token = token.clone();
        self.schedule_cancellation.replace(token);

        let songvol = self.args.songvol;
        let tx = self.audio.tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs_f64(0.5)).await;
            if cloned_token.is_cancelled() {
                return;
            }

            let demostart = song.tja().header.demostart.unwrap_or(0.0) as f64;
            let settings = StaticSoundSettings::new()
                .loop_region(demostart..)
                .playback_region(demostart..)
                .volume(songvol);

            if let Ok(music) = song.music().await {
                let _ = tx
                    .send(MusicInstruction::Play(Box::new(
                        music.with_settings(settings),
                    )))
                    .await;
            }
        });
    }
}

impl App {
    pub async fn new(mut args: AppArgs) -> Result<Self> {
        let mut course_selector = ListState::default();
        course_selector.select(None);

        let audio = AppAudio::new()?;
        audio.effects.set_volume(args.sevol);

        if args.host.is_some() {
            args.connect.clone_from(&args.host);
            let addr = args.host.clone().unwrap();
            let server = WebSocketStreamingServer::new(addr).unwrap();
            tokio::spawn(async move {
                server.start().await.unwrap();
            });
        }

        let client = if args.connect.is_some() {
            let addr = args.connect.clone().unwrap();
            let uid = generate_uid();
            let client = WebSocketStreamingClient::new(addr, uid).await.unwrap();

            let client_clone = client.clone();
            tokio::spawn(async move {
                let _ = client_clone.enable_pong().await;
            });

            Some(client)
        } else {
            None
        };

        let state = AppGlobalState {
            args,
            audio,
            schedule_cancellation: None,
            client,
        };

        let ui = UI::new()?;
        let input = InputMixer::new();

        Ok(Self { ui, input, state })
    }

    pub async fn run(&mut self) -> Result<()> {
        let (action_tx, mut action_rx) = mpsc::unbounded_channel();

        self.ui.state.songmenu.load(
            PlaylistLoader::new(self.state.args.songdir.clone())
                .list()
                .await?,
        );
        action_tx.send(Action::Switch(Page::SongMenu))?;

        self.input.listen_local_input();
        let mut input_rx = self.input.rx();

        let token = CancellationToken::new();

        // render ticker
        let cloned_token = token.clone();
        let cloned_tx = action_tx.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(1000 / 60));
            loop {
                interval.tick().await;
                if cloned_token.is_cancelled() {
                    break;
                }
                cloned_tx.send(Action::Render).unwrap();
            }
        });

        // game ticker
        let cloned_token = token.clone();
        let cloned_tx = action_tx.clone();
        let tps = self.state.args.tps;
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(1000 / tps));
            loop {
                interval.tick().await;
                if cloned_token.is_cancelled() {
                    break;
                }
                cloned_tx.send(Action::Tick).unwrap();
            }
        });

        self.ui.enter()?;

        let mut csv_writer = if self.state.args.profile {
            Some(csv::Writer::from_path("profile.csv")?)
        } else {
            None
        };

        if let Some(writer) = csv_writer.as_mut() {
            writer.write_record(&["type", "latency_ms", "absolute_time_ms"])?;
        }

        let start_time = std::time::Instant::now();

        loop {
            let start = tokio::time::Instant::now();
            let mut act = None;
            tokio::select! {
                _ = token.cancelled() => {
                    break;
                }
                input = input_rx.recv() => {
                    trace_dbg!(&input);
                    if let Ok(input) = input {
                        match input {
                            crossterm::event::Event::Key(k) => {
                                self.ui.handle(&mut self.state, tui::Event::Key(k), action_tx.clone()).await?;
                            },
                            crossterm::event::Event::FocusGained => {
                                self.ui.handle(&mut self.state, tui::Event::FocusGained, action_tx.clone()).await?;
                            },
                            crossterm::event::Event::FocusLost => {
                                self.ui.handle(&mut self.state, tui::Event::FocusLost, action_tx.clone()).await?;
                            },
                            crossterm::event::Event::Resize(w, h) => {
                                action_tx.send(Action::Resize(w, h))?;
                            },
                            crossterm::event::Event::Mouse(_) => {},
                            crossterm::event::Event::Paste(_) => {},
                        }
                    }
                    if let Some(writer) = csv_writer.as_mut() {
                        let elapsed = start.elapsed();
                        let absolute_time = start_time.elapsed();
                        writer.write_record(&[
                            "Input",
                            &format!("{:.3}", elapsed.as_secs_f64() * 1000.0),
                            &format!("{:.3}", absolute_time.as_secs_f64() * 1000.0)
                        ])?;
                    }
                }
                action = action_rx.recv() => {
                    if let Some(action) = action {
                        act.replace(action.clone());
                        match action {
                            Action::Quit => {
                                token.cancel();
                                break;
                            }
                            Action::Switch(page) => {
                                self.ui.switch_page(&mut self.state, page).await?;
                            }
                            Action::Tick => {
                                self.ui.handle(&mut self.state, tui::Event::Tick, action_tx.clone()).await?;
                            }
                            Action::Render => {
                                self.ui.render()?;
                            }
                            Action::Resize(w, h) => {
                                self.ui.tui.resize(Rect::new(0, 0, w, h))?;
                            }
                        }
                        if let Some(writer) = csv_writer.as_mut() {
                            let elapsed = start.elapsed();
                            let absolute_time = start_time.elapsed();
                            writer.write_record(&[
                                action.to_string(),
                                format!("{:.3}", elapsed.as_secs_f64() * 1000.0),
                                format!("{:.3}", absolute_time.as_secs_f64() * 1000.0)
                            ])?;
                        }
                    }
                }
            }
            let elapsed = start.elapsed();
            if elapsed.as_millis() > 5 {
                trace_dbg!(act);
                trace_dbg!(elapsed);
            }
        }

        if let Some(mut writer) = csv_writer {
            writer.flush()?;
        }
        self.ui.exit()?;
        Ok(())
    }
}

// Add this trait implementation to convert Action to String
impl ToString for Action {
    fn to_string(&self) -> String {
        match self {
            Action::Quit => "Quit".to_string(),
            Action::Switch(_) => "Switch".to_string(),
            Action::Tick => "Tick".to_string(),
            Action::Render => "Render".to_string(),
            Action::Resize(_, _) => "Resize".to_string(),
        }
    }
}
