//! The whole setup: every backend's view in one report.

use crate::{
    dante::{self, DanteStatus},
    focusrite::{self, FocusriteStatus},
};
use serde::Serialize;

#[derive(Serialize)]
pub struct Setup {
    pub dante: DanteStatus,
    pub focusrite: FocusriteStatus,
}

pub fn read(options: &dante::Options) -> Setup {
    Setup {
        dante: dante::status(options),
        focusrite: focusrite::status(options.timeout),
    }
}

impl Setup {
    pub fn has_errors(&self) -> bool {
        self.dante.has_errors() || self.focusrite.has_errors()
    }

    pub fn render(&self) -> String {
        format!(
            "── Dante ──\n{}── Focusrite (USB) ──\n{}",
            self.dante.render(),
            self.focusrite.render()
        )
    }
}
