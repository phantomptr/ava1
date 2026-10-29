//! Data-plane routing (SPEC.md §11.1, §12.1). Every data-plane message starts with its
//! 16-byte job id, so a frame reaches its job without a full decode.
use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::{mpsc, Notify};

use crate::conn::{Frame, FrameBody};
use crate::frame::FLAG_IGNORABLE;
use crate::link::{Outbox, DELIVER_DEPTH};
use crate::wire::FrameMessage;
use crate::Ava1Error;

/// A job's 16-byte identifier (SPEC.md §11.1).
pub type JobId = [u8; 16];
/// A boxed `Send` future, so [`LaneOpener`] stays object-safe.
pub type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Whether frame type `ty` is a data-plane message (0x20–0x3F), whose body starts with its
/// job id (SPEC.md §11.1).
pub fn is_data_type(ty: u8) -> bool {
    (0x20..=0x3F).contains(&ty)
}

/// The job id in the first 16 bytes of the frame's body; `None` when the body is shorter.
/// Meaningful only for a data-plane frame ([`is_data_type`]); the type is not checked here.
pub fn job_of(f: &Frame) -> Option<JobId> {
    f.body.get(..16).and_then(|b| b.try_into().ok())
}

/// What a job's [`Inbox`] yields: frames routed to the job, and the session's lane and
/// lifecycle events.
#[derive(Debug)]
pub enum Inbound {
    /// A data-plane frame for this job that arrived on the control connection.
    Control(Frame),
    /// A data-plane frame for this job that arrived on a data lane.
    Lane {
        /// The lane it arrived on.
        lane: u16,
        /// The frame.
        frame: Frame,
    },
    /// The lane with this id is up (it came up, or was already up when the job registered).
    LaneUp(u16),
    /// The lane with this id went down.
    LaneDown(u16),
    /// The job is cut off: the session ended, or another registration took its id
    /// (`"superseded"`). The string says why. No lane event follows it.
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

    /// Queues one frame of type `ty` with `flags`, `channel` and an encoded body. Waits for
    /// room in the writer queue; fails with [`Ava1Error::Lost`] once the connection has ended.
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

    /// Waits (at most `limit`) until everything queued before this call is on the wire;
    /// false when the limit passed first or the connection has ended.
    pub async fn flush(&self, limit: std::time::Duration) -> bool {
        self.outbox.flush(limit).await
    }

    /// Encodes `m` and queues it as a frame of its own type, flags 0, channel 0. Fails if `m`
    /// does not encode, or as [`send_raw`](Self::send_raw) does.
    pub async fn send<M: FrameMessage>(&self, m: &M) -> Result<(), Ava1Error> {
        self.send_raw(M::TYPE, 0, 0, m.to_bytes()?).await
    }

    /// A receiver that does not know the type skips it (Status).
    pub async fn send_ignorable<M: FrameMessage>(&self, m: &M) -> Result<(), Ava1Error> {
        self.send_raw(M::TYPE, FLAG_IGNORABLE, 0, m.to_bytes()?)
            .await
    }
}

/// One data lane that is up on the session: its id and a sender on its connection.
#[derive(Clone)]
pub struct LaneTx {
    /// The lane id.
    pub id: u16,
    gen: u64,
    /// Sends on the lane's connection.
    pub tx: ConnTx,
    /// Asks the lane's owner to close it (see [`LaneTx::close`]).
    closer: Arc<Notify>,
}

impl LaneTx {
    /// Asks whoever runs this lane's connection to close it, as a receiver does after a credit
    /// violation (SPEC.md §12.4): the lane goes down, the session and its other lanes go on.
    /// A server's lane loop ends on it; on the dialling side the job's [`LaneOpener`] closes
    /// the lane instead. Queue anything that must reach the peer first
    /// and flush it: the close drops what is still queued.
    pub fn close(&self) {
        self.closer.notify_one();
    }
}

/// Opens and closes lanes for jobs. Only the dialling side has one.
pub trait LaneOpener: Send + Sync {
    /// Opens a new data lane on the session and returns its lane id. The lane reports itself up
    /// to every job through the router ([`Inbound::LaneUp`]).
    fn open(&self) -> BoxFut<'_, Result<u16, Ava1Error>>;
    /// Lets go of the lane this opener opened with `id`; the lane then goes down
    /// ([`Inbound::LaneDown`]).
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

/// Routes one session's data-plane frames to its jobs by job id, and tells every job when a
/// lane comes up or goes down and when the session closes.
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

    /// Whether a job is registered under `job` right now.
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
                closer: Arc::default(),
            },
        );
        self.broadcast(|| Inbound::LaneUp(id));
        drop(lanes);
        gen
    }

    /// What [`LaneTx::close`] signals for the lane `id` of generation `gen`, for the task that
    /// runs its connection to wait on; `None` when that lane is not (or no longer) up.
    pub(crate) fn lane_closer(&self, id: u16, gen: u64) -> Option<Arc<Notify>> {
        self.lanes
            .lock()
            .unwrap()
            .get(&id)
            .filter(|t| t.gen == gen)
            .map(|t| t.closer.clone())
    }

    /// Only the generation that came up is taken down (a lane id can be re-joined).
    pub(crate) fn lane_down(&self, id: u16, gen: u64) {
        let mut l = self.lanes.lock().unwrap();
        if l.get(&id).is_some_and(|t| t.gen == gen) {
            l.remove(&id);
            self.broadcast(|| Inbound::LaneDown(id));
        }
    }

    /// The lanes that are up right now.
    pub fn lanes(&self) -> Vec<LaneTx> {
        self.lanes.lock().unwrap().values().cloned().collect()
    }

    /// The lane `id`, if it is up.
    pub fn lane(&self, id: u16) -> Option<LaneTx> {
        self.lanes.lock().unwrap().get(&id).cloned()
    }

    /// Whether `l` is still the lane up on its id (not gone, and not a later lane on the id).
    pub(crate) fn is_current(&self, l: &LaneTx) -> bool {
        self.lanes
            .lock()
            .unwrap()
            .get(&l.id)
            .is_some_and(|t| t.gen == l.gen)
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
    /// The job this link serves.
    pub job_id: JobId,
    /// The job's frames and lifecycle events.
    pub rx: Inbox,
    /// Sends on the session's control connection.
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

    /// The session's lanes that are up right now.
    pub fn lanes(&self) -> Vec<LaneTx> {
        self.router.lanes()
    }

    /// The session's lane `id`, if it is up.
    pub fn lane(&self, id: u16) -> Option<LaneTx> {
        self.router.lane(id)
    }

    /// The session's router, for work that outlives a borrow of this link.
    pub(crate) fn router(&self) -> Arc<Router> {
        self.router.clone()
    }

    /// The session's lane opener; `None` on the side that did not dial (only the dialling side
    /// opens lanes).
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
    async fn closing_a_lane_wakes_the_task_that_runs_it_and_only_that_generation() {
        let router = Arc::new(Router::default());
        let gen = router.lane_up(3, outbox());
        let closer = router.lane_closer(3, gen).expect("the lane is up");
        assert!(
            router.lane_closer(3, gen + 1).is_none(),
            "another generation"
        );
        router.lane(3).expect("the lane is up").close();
        // The permit is stored: a task that starts waiting after the close still sees it.
        timeout(Duration::from_secs(1), closer.notified())
            .await
            .expect("the lane's task was not told to close");
        router.lane_down(3, gen);
        assert!(router.lane_closer(3, gen).is_none(), "a lane that is down");
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
