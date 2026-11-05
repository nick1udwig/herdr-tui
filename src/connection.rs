use herdr_client_lib::{
    client::{Client, Error, Notifications, Stream},
    protocol::{AttachParams, Notification, Terminal},
    transport::HcConfig,
};
use serde_json::Value;
use std::time::{Duration, Instant};
const DEADLINE: Duration = Duration::from_secs(3);
/// Core sends stream output in frames of at most this size (herdr-core `wire::CHUNK`).
/// A shorter frame means core's queue for the stream has drained.
const CORE_CHUNK: usize = 16 * 1024;
/// The longest a pane waits for its attach repaint before drawing what it has.
const REPAINT_WAIT: Duration = Duration::from_millis(250);

/// An attach repaint still arriving. Drawing waits for it, so a long history
/// shows in its final state instead of replaying on screen.
#[derive(Default)]
pub struct Repaint(Option<Instant>);
impl Repaint {
    pub fn start(&mut self) {
        self.0 = Some(Instant::now() + REPAINT_WAIT);
    }
    /// Core queues the whole repaint at attach, so the first short frame ends it.
    pub fn received(&mut self, bytes: usize) {
        if bytes < CORE_CHUNK {
            self.0 = None;
        }
    }
    pub fn end(&mut self) {
        self.0 = None;
    }
    /// When drawing stops waiting, while the repaint is still arriving.
    pub fn pending_until(&self) -> Option<Instant> {
        self.0.filter(|deadline| *deadline > Instant::now())
    }
}

pub struct Connection {
    pub client: Client,
    pub events: Notifications,
    pub stream: Option<Stream>,
    pub address: String,
    pub record: Terminal,
    display: Value,
}
/// Attach with recent history, as Herdr shows on reattach.
pub fn attach_params(
    terminal: &str,
    size: (u16, u16),
    resumed: bool,
    display: &Value,
) -> AttachParams {
    AttachParams {
        terminal: terminal.into(),
        cols: size.0,
        rows: size.1,
        resumed,
        history: 1000,
        display: display.clone(),
        ..Default::default()
    }
}
impl Connection {
    pub async fn connect(
        config: &HcConfig,
        target: &str,
        size: (u16, u16),
        resumed: bool,
        display: &Value,
    ) -> Result<Self, Error> {
        let client = config.connect(DEADLINE).await?;
        let (_, events) = client.subscribe(DEADLINE).await?;
        let (record, stream) = client
            .attach(&attach_params(target, size, resumed, display), DEADLINE)
            .await?;
        let address = Some(record.address.clone())
            .filter(|a| !a.is_empty())
            .unwrap_or_else(|| target.to_owned());
        Ok(Self {
            client,
            events: Notifications::new(events),
            stream: Some(stream),
            address,
            record,
            display: display.clone(),
        })
    }
    pub async fn attach(&mut self, size: (u16, u16)) -> Result<(), Error> {
        let (record, stream) = self
            .client
            .attach(
                &attach_params(&self.address, size, true, &self.display),
                DEADLINE,
            )
            .await?;
        self.record = record;
        self.stream = Some(stream);
        Ok(())
    }
    pub async fn notification(&mut self, value: Value) -> Result<bool, Error> {
        let event = match Notification::decode(value)? {
            Notification::Missed(snapshot) => {
                if let Some(record) = snapshot
                    .terminals
                    .into_iter()
                    .find(|r| r.address == self.address)
                {
                    self.record = record;
                }
                return Ok(true);
            }
            Notification::Event(event) => event,
            _ => return Ok(false),
        };
        if event.terminal.as_deref() != Some(self.address.as_str()) {
            return Ok(false);
        }
        let mut records = std::collections::BTreeMap::from([(
            self.address.clone(),
            std::mem::take(&mut self.record),
        )]);
        herdr_client_lib::sync::reduce(&mut records, &event);
        self.record = records.remove(&self.address).unwrap_or_default();
        if event.kind == "restarted" {
            self.stream = None;
            return Ok(true);
        }
        Ok(event.kind == "size")
    }
    pub fn size(&self) -> (u16, u16) {
        size(&self.record, (80, 24))
    }
}
/// The terminal's authoritative size, or the fallback before core reports one.
pub fn size(record: &Terminal, fallback: (u16, u16)) -> (u16, u16) {
    (
        Some(record.cols).filter(|n| *n > 0).unwrap_or(fallback.0),
        Some(record.rows).filter(|n| *n > 0).unwrap_or(fallback.1),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn repaint_waits_for_the_first_short_frame_or_the_deadline() {
        let mut repaint = Repaint::default();
        assert!(repaint.pending_until().is_none());
        repaint.start();
        repaint.received(CORE_CHUNK);
        assert!(repaint.pending_until().is_some());
        repaint.received(CORE_CHUNK - 1);
        assert!(repaint.pending_until().is_none());
        repaint.start();
        repaint.end();
        assert!(repaint.pending_until().is_none());
        repaint.0 = Some(Instant::now());
        assert!(repaint.pending_until().is_none());
    }
}
