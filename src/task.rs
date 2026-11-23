//! Kerja latar belakang tanpa memblokir thread UI.
//!
//! Setiap tugas berjalan di thread sendiri; hasilnya diambil UI lewat `poll`.
//! Membatalkan = membuang `Pending` (hasil diabaikan saat tiba).

use std::sync::mpsc::{Receiver, TryRecvError, channel};

pub struct Pending<T>(Receiver<T>);

impl<T: Send + 'static> Pending<T> {
    pub fn spawn(ctx: &eframe::egui::Context, job: impl FnOnce() -> T + Send + 'static) -> Self {
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(job());
            ctx.request_repaint();
        });
        Self(rx)
    }
}

/// Ambil hasil jika sudah selesai. Slot dikosongkan saat hasil diambil
/// atau saat thread mati tanpa hasil (panic), supaya UI tidak loading selamanya.
pub fn poll<T>(slot: &mut Option<Pending<T>>) -> Option<T> {
    let result = match slot.as_ref()?.0.try_recv() {
        Ok(value) => Some(value),
        Err(TryRecvError::Empty) => return None,
        Err(TryRecvError::Disconnected) => None,
    };
    *slot = None;
    result
}
