//! Data-plane routing (SPEC.md §11.1, §12.1). Every data-plane message starts with its
//! 16-byte job id, so a frame reaches its job without a full decode.
use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;

use crate::conn::{Frame, FrameBody};
use crate::frame::FLAG_IGNORABLE;
use crate::link::{Outbox, DELIVER_DEPTH};
use crate::wire::FrameMessage;
use crate::Ava1Error;

pub type JobId = [u8; 16];
pub type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub fn is_data_type(ty: u8) -> bool {
    (0x20..=0x3F).contains(&ty)
}

pub fn job_of(f: &Frame) -> Option<JobId> {
    f.body.get(..16).and_then(|b| b.try_into().ok())
}

#[derive(Debug)]
pub enum Inbound {
    Control(Frame),
    Lane { lane: u16, frame: Frame },
    LaneUp(u16),
    LaneDown(u16),
    Closed(String),
}

/// Sends on one connection. Every send goes through the connection's writer queue,
/// whole or not at all, so a caller that drops the future (a timeout, a `select!`)
/// never leaves half a sealed frame on the wire, and a full queue (the peer not
/// taking bytes) is the backpressure.
///
/// It is backpressure, not delivery: `Ok` means the frame is queued, and the queue
/// drains when the writer gets to it. A job learns what really arrived from the peer
/// (the acks `Received` and `Durable`), never from `Ok`.
#[derive(Clone)]
pub struct ConnTx {
    outbox: Outbox,
}

impl ConnTx {
    pub(crate) fn new(outbox: Outbox) -> Self {
        Self { outbox }
    }

    pub async fn send_raw(
        &self,
        ty: u8,
        flags: u8,
        channel: u32,
        body: impl Into<FrameBody>,
    ) -> Result<(), Ava1Error> {
        self.outbox.send_frame(ty, flags, channel, body).await
    }

    /// `send_raw` with a take-marker: the writer flips it the moment it takes the
    /// frame out of its queue — un-taken at the writer's death, the frame provably
    /// never left this process (the sender releases its window charge, I3).
    pub async fn send_raw_marked(
        &self,
        ty: u8,
        flags: u8,
        channel: u32,
        body: impl Into<FrameBody>,
        taken: Arc<AtomicBool>,
    ) -> Result<(), Ava1Error> {
        self.outbox
            .send_frame_marked(ty, flags, channel, body, Some(taken))
            .await
    }

    /// True once the connection's writer has ended: a frame still un-taken then
    /// provably never left this process.
    pub fn writer_dead(&self) -> bool {
        self.outbox.writer_dead()
    }

    pub async fn send<M: FrameMessage>(&self, m: &M) -> Result<(), Ava1Error> {
        self.send_raw(M::TYPE, 0, 0, m.to_bytes()?).await
    }

    /// A receiver that does not know the type skips it (Status).
    pub async fn send_ignorable<M: FrameMessage>(&self, m: &M) -> Result<(), Ava1Error> {
        self.send_raw(M::TYPE, FLAG_IGNORABLE, 0, m.to_bytes()?)
            .await
    }
}

#[derive(Clone)]
pub struct LaneTx {
    pub id: u16,
    gen: u64,
    pub tx: ConnTx,
}

/// Opens and closes lanes for jobs. Only the dialling side has one.
pub trait LaneOpener: Send + Sync {
    fn open(&self) -> BoxFut<'_, Result<u16, Ava1Error>>;
    fn close(&self, id: u16);
}

/// Hands a job's frames to it; runs inside a server when a job arrives for nobody.
pub trait JobHost: Send + Sync {
    /// `first` is the JobOpen or Resume that created the job; `peer` is the static key
    /// of the device that sent it (a job is bound to it, SPEC.md §11.1).
    ///
    /// This runs on the session's control loop: spawn the job's own task and return, or
    /// every RPC reply and every other job of that session waits behind it. Whatever the
    /// job needs to do — walking a source tree, opening files — happens in that task.
    fn accept(&self, link: JobLink, first: Frame, peer: [u8; 32]);
}

/// One job's two channels. Frames ride a bounded channel so a job that stops reading
/// slows the reader and the peer sees backpressure, instead of us growing a queue that
/// holds whole 16 MiB frames; lifecycle events ride their own channel, which can never
/// be full behind data because a lane change or a session end produces at most one.
struct JobInbox {
    data: mpsc::Sender<Inbound>,
    events: mpsc::UnboundedSender<Inbound>,
    /// This registration's identity, shared with its `Inbox` so a link can prove, when it
    /// lets go, that it is still the current one. Deliberately not a sender clone: an
    /// inbox holding its own sender would keep its channel open, so letting go of the
    /// registration could never end `recv` — the channel must close with the router's copy.
    id: Arc<()>,
}

/// What a job task reads: `Recv`'s items, events first.
pub struct Inbox {
    data: mpsc::Receiver<Inbound>,
    events: mpsc::UnboundedReceiver<Inbound>,
    id: Arc<()>,
    events_done: bool,
}

impl Inbox {
    /// `None` once the router has let go of this job and everything queued is read.
    pub async fn recv(&mut self) -> Option<Inbound> {
        // A `Closed` (or a lane change) behind a full data queue would be invisible, so
        // events are drained first and never wait on data.
        if let Ok(ev) = self.events.try_recv() {
            return Some(ev);
        }
        loop {
            if self.events_done {
                return self.data.recv().await;
            }
            tokio::select! {
                biased;
                ev = self.events.recv() => match ev {
                    Some(i) => return Some(i),
                    None => self.events_done = true,
                },
                d = self.data.recv() => return d,
            }
        }
    }
}

#[derive(Default)]
pub struct Router {
    jobs: Mutex<HashMap<JobId, JobInbox>>,
    lanes: Mutex<BTreeMap<u16, LaneTx>>,
    next_gen: AtomicU64,
    closed: Mutex<Option<String>>,
}

impl Router {
    /// A job's inbox. It first hears about lanes that are already up, and — if it is
    /// registering over its own id (a retry, a resume) — the job it replaces hears why.
    pub fn register(&self, job: JobId) -> Inbox {
        let (data, rx) = mpsc::channel(DELIVER_DEPTH);
        let (events, erx) = mpsc::unbounded_channel();
        // Lanes, then jobs — the same order every other method takes them — and the closed
        // check happens inside `lanes`, because `close` sets its flag *before* it takes
        // `lanes`: whatever it does with lanes happens after the flag, so an inbox that
        // checks the flag while holding lanes either sees it, or its snapshot precedes the
        // clear and the broadcast that follows finds this inbox. A check outside the lock
        // could miss both and leave the job waiting on a session that is gone.
        let lanes = self.lanes.lock().unwrap();
        let closed = self.closed.lock().unwrap().clone();
        if let Some(why) = closed {
            let _ = events.send(Inbound::Closed(why));
        } else {
            for id in lanes.keys() {
                let _ = events.send(Inbound::LaneUp(*id));
            }
        }
        let id = Arc::new(());
        let inbox = JobInbox {
            data,
            events,
            id: id.clone(),
        };
        let replaced = self.jobs.lock().unwrap().insert(job, inbox);
        drop(lanes);
        if let Some(old) = replaced {
            // Dropping the old data sender ends its reader too; the event tells it why
            // rather than leaving it to hang on a job that no longer exists.
            let _ = old.events.send(Inbound::Closed("superseded".into()));
        }
        Inbox {
            data: rx,
            events: erx,
            id,
            events_done: false,
        }
    }

    /// Only the registration that is still current may unregister: a link that was
    /// replaced (or a second one for the same id) leaves the newer one alone.
    pub fn unregister(&self, job: &JobId, id: &Arc<()>) {
        let mut jobs = self.jobs.lock().unwrap();
        if jobs.get(job).is_some_and(|i| Arc::ptr_eq(&i.id, id)) {
            jobs.remove(job);
        }
    }

    /// Jobs registered on this session right now.
    pub fn job_count(&self) -> usize {
        self.jobs.lock().unwrap().len()
    }

    pub fn has_job(&self, job: &JobId) -> bool {
        self.jobs.lock().unwrap().contains_key(job)
    }

    /// Gives the frame back when no job is registered for it (the caller decides). Waits
    /// for room in the job's bounded inbox, so a job that stops reading holds this reader
    /// — and the connection's own queue behind it — rather than this side growing.
    pub async fn route_control(&self, f: Frame) -> Option<Frame> {
        let job = job_of(&f)?;
        let Some(tx) = self.jobs.lock().unwrap().get(&job).map(|i| i.data.clone()) else {
            return Some(f);
        };
        match tx.send(Inbound::Control(f)).await {
            Ok(()) => None,
            // The job let go between the lookup and the send: the caller decides again.
            Err(e) => match e.0 {
                Inbound::Control(f) => Some(f),
                _ => None,
            },
        }
    }

    /// Lane frames for unknown jobs are dropped: credit only exists inside a job. Like
    /// `route_control`, this waits for the job to make room.
    pub async fn route_lane(&self, lane: u16, f: Frame) {
        if let Some(job) = job_of(&f) {
            let tx = self.jobs.lock().unwrap().get(&job).map(|i| i.data.clone());
            if let Some(tx) = tx {
                let _ = tx.send(Inbound::Lane { lane, frame: f }).await;
            }
        }
    }

    pub(crate) fn lane_up(&self, id: u16, outbox: Outbox) -> u64 {
        let gen = self.next_gen.fetch_add(1, Ordering::Relaxed) + 1;
        // Lanes, then jobs (see `register`), and the closed check under `lanes` for the same
        // reason: a lane that comes up as the session ends is nobody's — the job has already
        // been told the session closed, and that message is terminal.
        let mut lanes = self.lanes.lock().unwrap();
        let closed = self.closed.lock().unwrap().is_some();
        if closed {
            return gen;
        }
        lanes.insert(
            id,
            LaneTx {
                id,
                gen,
                tx: ConnTx::new(outbox),
            },
        );
        self.broadcast(|| Inbound::LaneUp(id));
        drop(lanes);
        gen
    }

    /// Only the generation that came up is taken down (a lane id can be re-joined).
    pub(crate) fn lane_down(&self, id: u16, gen: u64) {
        let mut l = self.lanes.lock().unwrap();
        if l.get(&id).is_some_and(|t| t.gen == gen) {
            l.remove(&id);
            self.broadcast(|| Inbound::LaneDown(id));
        }
    }

    pub fn lanes(&self) -> Vec<LaneTx> {
        self.lanes.lock().unwrap().values().cloned().collect()
    }

    pub fn lane(&self, id: u16) -> Option<LaneTx> {
        self.lanes.lock().unwrap().get(&id).cloned()
    }

    pub(crate) fn close(&self, why: &str) {
        {
            let mut c = self.closed.lock().unwrap();
            if c.is_some() {
                return;
            }
            *c = Some(why.to_string());
        }
        let mut lanes = self.lanes.lock().unwrap();
        lanes.clear();
        self.broadcast(|| Inbound::Closed(why.to_string()));
        drop(lanes);
    }

    /// Lifecycle events only: they go on the channel that is never full behind data.
    fn broadcast(&self, ev: impl Fn() -> Inbound) {
        for inbox in self.jobs.lock().unwrap().values() {
            let _ = inbox.events.send(ev());
        }
    }
}

/// What a job task holds: its inbox, the control connection, and the session's lanes.
/// `rx.recv()` yields the session's frames and its lifecycle events, events first; `None`
/// means this job is over (the session closed, or another registration took the id) and
/// nothing queued remains.
pub struct JobLink {
    pub job_id: JobId,
    pub rx: Inbox,
    pub control: ConnTx,
    router: Arc<Router>,
    opener: Option<Arc<dyn LaneOpener>>,
}

impl JobLink {
    pub(crate) fn new(
        job_id: JobId,
        router: Arc<Router>,
        control: ConnTx,
        opener: Option<Arc<dyn LaneOpener>>,
    ) -> Self {
        let rx = router.register(job_id);
        Self {
            job_id,
            rx,
            control,
            router,
            opener,
        }
    }

    pub fn lanes(&self) -> Vec<LaneTx> {
        self.router.lanes()
    }

    pub fn lane(&self, id: u16) -> Option<LaneTx> {
        self.router.lane(id)
    }

    pub fn opener(&self) -> Option<&Arc<dyn LaneOpener>> {
        self.opener.as_ref()
    }
}

impl Drop for JobLink {
    fn drop(&mut self) {
        self.router.unregister(&self.job_id, &self.rx.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conn::{FrameReader, FrameWriter};
    use crate::session::Timing;
    use tokio::io::{duplex, split};
    use tokio::time::{timeout, Duration};

    /// A lane frame for `job`: only the first 16 bytes matter to the router.
    fn frame_for(job: JobId) -> Frame {
        let mut body = vec![0u8; 32];
        body[..16].copy_from_slice(&job);
        Frame {
            ty: crate::gen::Chunk::TYPE,
            flags: 0,
            channel: 0,
            body,
        }
    }

    /// A real `Outbox` (a duplex pipe with `drive` behind it), for the tests that need a
    /// lane to come up.
    fn outbox() -> Outbox {
        let (a, _b) = duplex(4096);
        let (r, w) = split(a);
        let (tx, _rx) = mpsc::channel(1);
        let (_link, outbox) = crate::link::drive(
            FrameReader::new(r),
            FrameWriter::new(w),
            Timing::default(),
            tx,
        );
        outbox
    }

    #[tokio::test]
    async fn a_job_that_stops_reading_slows_the_router_instead_of_queueing_frames() {
        // SPEC.md §12: every hop is bounded so a slow consumer pushes back to the sender.
        // A job that stops reading must hold the reader, never grow a queue of frames.
        let router = Router::default();
        let mut inbox = router.register([7; 16]);
        for _ in 0..DELIVER_DEPTH {
            router.route_lane(1, frame_for([7; 16])).await;
        }
        assert!(
            timeout(
                Duration::from_millis(100),
                router.route_lane(1, frame_for([7; 16]))
            )
            .await
            .is_err(),
            "the inbox held more than its bound"
        );
        // Reading again makes room, and the held frame lands.
        assert!(matches!(inbox.recv().await, Some(Inbound::Lane { .. })));
        assert!(
            timeout(
                Duration::from_secs(1),
                router.route_lane(1, frame_for([7; 16]))
            )
            .await
            .is_ok(),
            "the router kept waiting after the job read"
        );
    }

    #[tokio::test]
    async fn a_full_inbox_still_hears_the_session_end() {
        // Lifecycle events ride their own channel: a `Closed` behind 64 unread frames
        // would leave a job waiting on a session that is gone.
        let router = Router::default();
        let mut inbox = router.register([8; 16]);
        for _ in 0..DELIVER_DEPTH {
            router.route_lane(1, frame_for([8; 16])).await;
        }
        router.close("done");
        let ev = timeout(Duration::from_secs(1), inbox.recv())
            .await
            .expect("no event within 1 s");
        assert!(matches!(ev, Some(Inbound::Closed(why)) if why == "done"));
    }

    #[tokio::test]
    async fn a_second_registration_ends_the_first_and_a_stale_link_cannot_take_the_id() {
        let router = Router::default();
        let mut first = router.register([9; 16]);
        let mut second = router.register([9; 16]);
        let ev = timeout(Duration::from_secs(1), first.recv())
            .await
            .expect("no event within 1 s");
        assert!(
            matches!(ev, Some(Inbound::Closed(_))),
            "the replaced job is told why"
        );
        assert!(first.recv().await.is_none(), "and then its channel ends");
        // The old link letting go must not unregister the one that replaced it.
        let stale = first.id.clone();
        router.unregister(&[9; 16], &stale);
        assert!(router.has_job(&[9; 16]), "a stale link took the id away");
        router.route_lane(1, frame_for([9; 16])).await;
        assert!(matches!(second.recv().await, Some(Inbound::Lane { .. })));
    }

    #[tokio::test]
    async fn a_lane_that_comes_up_as_the_session_ends_is_not_reported() {
        let router = Router::default();
        let mut inbox = router.register([10; 16]);
        router.close("done");
        let ev = timeout(Duration::from_secs(1), inbox.recv())
            .await
            .expect("no event within 1 s");
        assert!(matches!(ev, Some(Inbound::Closed(_))));
        router.lane_up(3, outbox());
        assert!(router.lanes().is_empty(), "a closed router has no lanes");
        assert!(
            timeout(Duration::from_millis(100), inbox.recv())
                .await
                .is_err(),
            "a lane came up after the session ended"
        );
    }

    #[tokio::test]
    async fn a_registration_that_races_close_still_hears_it() {
        // `close` sets its flag before it takes `lanes`, and `register` checks the flag
        // *under* `lanes`: an inbox that reaches the insert while the router closes in
        // between must still be told, or its job waits on a session that is gone forever.
        // The test holds `lanes` so the register thread reads the flag (still open), then
        // sets it and lets go — the exact interleaving a check outside the lock misses.
        let router = Arc::new(Router::default());
        let held = router.lanes.lock().unwrap();
        let r = router.clone();
        let t = std::thread::spawn(move || r.register([11; 16]));
        std::thread::sleep(Duration::from_millis(50));
        *router.closed.lock().unwrap() = Some("done".into());
        drop(held);
        let mut inbox = t.join().unwrap();
        let ev = timeout(Duration::from_secs(1), inbox.recv())
            .await
            .expect("a closed router told the new job");
        assert!(matches!(ev, Some(Inbound::Closed(why)) if why == "done"));
    }

    #[tokio::test]
    async fn a_lane_that_lands_while_the_session_is_closing_is_not_kept() {
        let router = Arc::new(Router::default());
        let ob = outbox();
        let held = router.lanes.lock().unwrap();
        let r = router.clone();
        let t = std::thread::spawn(move || r.lane_up(3, ob));
        std::thread::sleep(Duration::from_millis(50));
        *router.closed.lock().unwrap() = Some("done".into());
        drop(held);
        t.join().unwrap();
        assert!(router.lanes().is_empty(), "a closed router kept a lane");
    }
}
