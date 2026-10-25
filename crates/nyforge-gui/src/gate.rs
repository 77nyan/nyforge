//! The check that sits in front of every destructive operation.
//!
//! 1. Press the button → the drive is looked up again and compared with what was selected (`Checking`).
//! 2. A confirmation dialog names the drive (`Confirm`).
//! 3. Pressing the confirm button looks once more (`Rechecking`) and only then does the operation start.
//!
//! If the drive was unplugged, swapped, or became unusable at either look, nothing starts.

use crate::drives::{self, Candidate, DriveId, Purpose};
use eframe::egui;
use std::sync::mpsc::{channel, Receiver};

enum Stage {
    Checking(Receiver<Result<Candidate, String>>),
    Confirm(Candidate),
    Rechecking(Candidate, Receiver<Result<Candidate, String>>),
}

pub enum GateEvent {
    Waiting,
    /// The drive is unchanged and the person confirmed: start now with this (freshly read) drive.
    Start(Candidate),
    /// The drive changed or vanished; the text says how.
    Failed(String),
}

pub struct Gate<A> {
    stage: Stage,
    purpose: Purpose,
    pub action: A,
}

fn look(purpose: Purpose, id: DriveId, ctx: &egui::Context) -> Receiver<Result<Candidate, String>> {
    let (tx, rx) = channel();
    let c = ctx.clone();
    std::thread::spawn(move || {
        let _ = tx.send(drives::revalidate(purpose, &id));
        c.request_repaint();
    });
    rx
}

impl<A> Gate<A> {
    pub fn new(purpose: Purpose, id: &DriveId, action: A, ctx: &egui::Context) -> Self {
        Gate { stage: Stage::Checking(look(purpose, id.clone(), ctx)), purpose, action }
    }

    pub fn poll(&mut self) -> GateEvent {
        match &self.stage {
            Stage::Checking(rx) => match rx.try_recv() {
                Ok(Ok(c)) => {
                    self.stage = Stage::Confirm(c);
                    GateEvent::Waiting
                }
                Ok(Err(e)) => GateEvent::Failed(e),
                Err(_) => GateEvent::Waiting,
            },
            Stage::Rechecking(_, rx) => match rx.try_recv() {
                Ok(Ok(c)) => GateEvent::Start(c),
                Ok(Err(e)) => GateEvent::Failed(e),
                Err(_) => GateEvent::Waiting,
            },
            Stage::Confirm(_) => GateEvent::Waiting,
        }
    }

    /// The drive to show in the confirmation dialog, once the first look is done.
    pub fn candidate(&self) -> Option<&Candidate> {
        match &self.stage {
            Stage::Confirm(c) | Stage::Rechecking(c, _) => Some(c),
            Stage::Checking(_) => None,
        }
    }

    pub fn is_rechecking(&self) -> bool {
        matches!(self.stage, Stage::Rechecking(..))
    }

    /// The confirm button was pressed.
    pub fn accept(&mut self, ctx: &egui::Context) {
        if let Stage::Confirm(c) = &self.stage {
            let c = c.clone();
            let rx = look(self.purpose, c.id.clone(), ctx);
            self.stage = Stage::Rechecking(c, rx);
        }
    }
}
