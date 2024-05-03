use crate::{app::AppGlobalState, tui::Frame};
use color_eyre::eyre::Result;
use ratatui::{prelude::*, widgets::*};
use taiko_core::constant::COURSE_TYPE;

use super::Component;

pub struct ErrorPage {}

impl Component for ErrorPage {
    fn new() -> Self {
        Self {}
    }
}
