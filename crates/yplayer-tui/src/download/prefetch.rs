use anyhow::Result;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

use crate::config::Config;
use crate::download::bridge::Bridge;
use crate::types::Track;

pub struct PrefetchManager {
    tx: mpsc::UnboundedSender<PrefetchCommand>,
    pub result_rx: mpsc::UnboundedReceiver<PrefetchResult>,
}

enum PrefetchCommand {
    SetIndex(usize),
    Stop,
}

#[derive(Debug)]
pub struct PrefetchResult {
    pub index: usize,
    pub track: Track,
}

impl PrefetchManager {
    pub async fn start(entries: Vec<Track>, cfg: Config, lookahead: usize) -> Result<Self> {
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<PrefetchCommand>();
        let (result_tx, result_rx) = mpsc::unbounded_channel::<PrefetchResult>();

        tokio::spawn(async move {
            let mut current_idx: usize = 0;
            let mut downloaded: std::collections::HashSet<usize> = std::collections::HashSet::new();

            // Mark already-cached entries
            for (i, entry) in entries.iter().enumerate() {
                if entry.audio_path.is_some() {
                    downloaded.insert(i);
                }
            }

            // Create worker bridge
            let bridge = match Bridge::new(&cfg).await {
                Ok(b) => Arc::new(Mutex::new(b)),
                Err(_) => return,
            };

            loop {
                // Check for commands (non-blocking)
                match cmd_rx.try_recv() {
                    Ok(PrefetchCommand::SetIndex(idx)) => {
                        current_idx = idx;
                    }
                    Ok(PrefetchCommand::Stop) => break,
                    Err(mpsc::error::TryRecvError::Disconnected) => break,
                    Err(mpsc::error::TryRecvError::Empty) => {}
                }

                // Find next track to prefetch
                let mut prefetched_this_round = false;
                for offset in 0..lookahead {
                    let idx = current_idx + offset;
                    if idx >= entries.len() {
                        break;
                    }
                    if downloaded.contains(&idx) {
                        continue;
                    }

                    let entry = &entries[idx];
                    let url = match &entry.webpage_url {
                        Some(u) => u.clone(),
                        None => continue,
                    };

                    // Download
                    let bridge = bridge.clone();
                    let cfg_clone = cfg.clone();
                    match bridge.lock().await.download(&url, &cfg_clone).await {
                        Ok(result) => {
                            downloaded.insert(idx);
                            let _ = result_tx.send(PrefetchResult {
                                index: idx,
                                track: result.track,
                            });
                        }
                        Err(_) => {
                            // Mark as attempted to avoid retry loop
                            downloaded.insert(idx);
                        }
                    }
                    prefetched_this_round = true;
                    break; // one at a time, then re-check commands
                }

                if !prefetched_this_round {
                    // Nothing to prefetch, wait for index change
                    match cmd_rx.recv().await {
                        Some(PrefetchCommand::SetIndex(idx)) => {
                            current_idx = idx;
                        }
                        Some(PrefetchCommand::Stop) | None => break,
                    }
                }
            }
        });

        Ok(Self {
            tx: cmd_tx,
            result_rx,
        })
    }

    pub fn set_index(&self, idx: usize) {
        let _ = self.tx.send(PrefetchCommand::SetIndex(idx));
    }

    pub fn stop(&self) {
        let _ = self.tx.send(PrefetchCommand::Stop);
    }
}
