use trk_core::error::Result;

use crate::env::Env;
use crate::herdr_ui;
use crate::render::Out;

pub(super) fn open(env: &Env, split: Option<String>, space: Option<String>) -> Result<Out> {
    let place = match split.as_deref() {
        Some("right") => herdr_ui::Place::Right,
        Some("down") => herdr_ui::Place::Down,
        _ => herdr_ui::Place::Tab,
    };
    herdr_ui::open(env, &herdr_ui::app_home(), place, space.as_deref())
}
