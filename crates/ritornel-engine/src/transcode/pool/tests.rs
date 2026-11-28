use super::*;
use crate::testing::{CountingEntropy, FixedClock};
use crate::transcode::{FakeEncoder, tests::mp3};
use crate::{Id, JobStop};
use std::fs;
use std::io;
use std::path::Path;
use std::sync::mpsc;

const WAIT: Duration = Duration::from_secs(60);

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/audio")
        .join(name)
}

struct Setup {
    pool: Arc<TranscodePool>,
    store: Arc<ServerStore>,
    dir: tempfile::TempDir,
}

impl Setup {
    fn new(workers: usize, budget: u64) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(
            ServerStore::open_in_memory(
                Arc::new(FixedClock::at(100)),
                Arc::new(CountingEntropy::default()),
            )
            .unwrap(),
        );
        let cache =
            TranscodeCache::open(&store, dir.path(), Id::from_bytes([1; 16]), budget).unwrap();
        let pool = Arc::new(
            TranscodePool::new(store.clone(), cache, NonZeroUsize::new(workers).unwrap()).unwrap(),
        );
        Self { pool, store, dir }
    }

    fn job(&self, id: i64) -> JobCancel {
        self.store.job_cancel(id, Arc::default()).unwrap()
    }
}

fn master(n: u8) -> Master {
    let path = fixture("a.flac");
    Master {
        asset: AudioAsset {
            scheme: 1,
            hash: blake3::hash(&[n]),
        },
        facts: FileFacts::of(&path.metadata().unwrap()),
        path,
    }
}

fn encoding(backend: Arc<dyn Encoder>) -> Encoding {
    Encoding {
        id: backend.probe().unwrap().unwrap(),
        profile: mp3(),
        gain_tags: GainTags::default(),
        backend,
    }
}

fn item(n: u8, backend: &Arc<dyn Encoder>) -> BatchItem {
    BatchItem {
        master: master(n),
        encoding: encoding(backend.clone()),
    }
}

/// A batch's encodes, none of which failed.
fn entries<'a>(batch: &'a Batch<'_>) -> Vec<&'a CacheEntry> {
    batch.results().map(Result::unwrap).collect()
}

/// A transfer of the master as it is, tagged with `title`.
fn as_it_is(n: u8, title: &str) -> TransferRequest {
    TransferRequest {
        master: master(n),
        audio: TransferAudio::PassThrough(Codec::Flac),
        tagging: Arc::new(Tagging {
            tags: Tags {
                title: Some(title.into()),
                ..Tags::default()
            },
            cover: None,
            id3v1: false,
        }),
    }
}

/// The scratch files prepared transfers hold now.
fn scratch(setup: &Setup) -> usize {
    setup
        .dir
        .path()
        .join("tmp")
        .read_dir()
        .unwrap()
        .filter(|entry| {
            entry
                .as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("transfer-")
        })
        .count()
}

struct Started {
    finish: mpsc::Sender<io::Result<()>>,
    thread: std::thread::ThreadId,
}

async fn wait_for_encoder(
    batch: &mut (impl Future + Unpin),
    started: &mut tokio::sync::mpsc::UnboundedReceiver<Started>,
) -> Started {
    tokio::time::timeout(WAIT, async {
        tokio::select! {
            _ = batch => panic!("batch returned before an encode started"),
            started = started.recv() => started.unwrap(),
        }
    })
    .await
    .unwrap()
}

struct Held(tokio::sync::mpsc::UnboundedSender<Started>);

fn held() -> (
    Arc<dyn Encoder>,
    tokio::sync::mpsc::UnboundedReceiver<Started>,
) {
    let (send, receive) = tokio::sync::mpsc::unbounded_channel();
    (Arc::new(Held(send)), receive)
}

impl Encoder for Held {
    fn probe(&self) -> io::Result<Option<EncoderId>> {
        FakeEncoder.probe()
    }
    fn supports(&self, profile: Profile) -> bool {
        FakeEncoder.supports(profile)
    }
    fn encode(
        &self,
        _pcm: Pcm,
        _profile: Profile,
        _key: blake3::Hash,
        output: &Path,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        let (finish, result) = mpsc::channel();
        self.0
            .send(Started {
                finish,
                thread: std::thread::current().id(),
            })
            .unwrap();
        loop {
            super::super::check_cancel(cancel)?;
            match result.recv_timeout(Duration::from_millis(5)) {
                Ok(outcome) => {
                    outcome?;
                    break;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(_) => return Err(io::Error::other("test encoder abandoned")),
            }
        }
        fs::copy(fixture("a.mp3"), output)?;
        Ok(())
    }
}

#[tokio::test]
async fn two_workers_encode_together_and_a_third_item_waits() {
    let setup = Setup::new(2, u64::MAX);
    let (encoder, mut started) = held();
    let job = setup.job(1);
    let mut events = Vec::new();
    let run = setup
        .pool
        .ensure_batch((0..3).map(|n| item(n, &encoder)).collect(), &job, |event| {
            events.push(event)
        });
    let control = async {
        let a = started.recv().await.unwrap();
        let b = started.recv().await.unwrap();
        assert_ne!(a.thread, b.thread);
        assert_ne!(a.thread, std::thread::current().id());
        assert!(started.try_recv().is_err());
        // An unrelated writer can commit with both encoders still held.
        setup.store.transact(|_, _, _| Ok(())).unwrap();
        b.finish.send(Ok(())).unwrap();
        let c = started.recv().await.unwrap();
        a.finish.send(Ok(())).unwrap();
        c.finish.send(Ok(())).unwrap();
    };
    let (batch, ()) = tokio::time::timeout(WAIT, async { tokio::join!(run, control) })
        .await
        .unwrap();
    let batch = batch.unwrap();
    assert_eq!(batch.results().len(), 3);
    for (n, entry) in entries(&batch).into_iter().enumerate() {
        let e = encoding(encoder.clone());
        assert_eq!(
            entry.key(),
            e.id.derivative_key(master(n as u8).asset, e.profile, e.gain_tags)
                .unwrap()
        );
    }
    assert_eq!(
        events.iter().map(|e| e.completed).collect::<Vec<_>>(),
        [1, 2, 3]
    );
    assert!(events.iter().all(|e| e.total == 3 && e.size_bytes > 0));
}

#[tokio::test]
async fn cancelling_one_job_leaves_its_shared_encode_and_surviving_pin_intact() {
    let setup = Setup::new(1, 0);
    let (encoder, mut started) = held();
    let a = setup.job(1);
    let b = setup.job(2);
    let mut first = Box::pin(setup.pool.ensure_batch(vec![item(0, &encoder)], &a, |_| {}));
    let mut second = Box::pin(setup.pool.ensure_batch(vec![item(0, &encoder)], &b, |_| {}));
    let only = wait_for_encoder(&mut first, &mut started).await;
    assert!(second.as_mut().now_or_never().is_none());
    setup.store.cancels.lock().unwrap().insert(1);
    let e = tokio::time::timeout(WAIT, first)
        .await
        .unwrap()
        .unwrap_err();
    assert!(matches!(&*e, Error::JobStopped(JobStop::Cancelled)));
    only.finish.send(Ok(())).unwrap();
    let survivor = tokio::time::timeout(WAIT, second).await.unwrap().unwrap();
    let path = entries(&survivor)[0].path().to_owned();
    assert!(path.exists());
    assert!(started.try_recv().is_err());
    // The second batch independently pins a hit, even under a zero-byte budget.
    let another = setup
        .pool
        .ensure_batch(vec![item(0, &encoder)], &b, |_| {})
        .await
        .unwrap();
    drop(survivor);
    assert!(path.exists());
    let pressure = setup.pool.ensure_batch(vec![item(1, &encoder)], &b, |_| {});
    let complete = async { started.recv().await.unwrap().finish.send(Ok(())).unwrap() };
    let (pressure, ()) = tokio::time::timeout(WAIT, async { tokio::join!(pressure, complete) })
        .await
        .unwrap();
    let pressure = pressure.unwrap();
    let other_path = entries(&pressure)[0].path().to_owned();
    drop(pressure);
    tokio::time::timeout(WAIT, async {
        while other_path.exists() {
            // sleep: eviction runs on a worker after the last pin drops.
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(path.exists(), "the surviving batch must still own its pin");
    drop(another);
    drop(setup.pool);
    assert!(!path.exists());
    assert_eq!(setup.dir.path().join("tmp").read_dir().unwrap().count(), 0);
}

#[tokio::test]
async fn a_shared_error_reaches_both_callers_and_the_next_attempt_can_retry() {
    let setup = Setup::new(1, 0);
    let (encoder, mut started) = held();
    let job = setup.job(1);
    let mut a = Box::pin(
        setup
            .pool
            .ensure_batch(vec![item(0, &encoder)], &job, |_| {}),
    );
    let mut b = Box::pin(
        setup
            .pool
            .ensure_batch(vec![item(0, &encoder)], &job, |_| {}),
    );
    let run = async {
        let once = wait_for_encoder(&mut a, &mut started).await;
        assert!(b.as_mut().now_or_never().is_none());
        once.finish
            .send(Err(io::Error::other("codec failure")))
            .unwrap();
        // One item's failure is its own result, and not its batch's.
        let a = a.await.unwrap();
        let a = a.results().next().unwrap().unwrap_err();
        assert!(a.to_string().contains("codec failure"));
        let retry = setup
            .pool
            .ensure_batch(vec![item(0, &encoder)], &job, |_| {});
        let control = async {
            started.recv().await.unwrap().finish.send(Ok(())).unwrap();
        };
        tokio::join!(retry, control).0.unwrap();
        // This waiter still receives the original error after the retry succeeded.
        let b = b.await.unwrap();
        assert!(std::ptr::eq(a, b.results().next().unwrap().unwrap_err()));
    };
    tokio::time::timeout(WAIT, run).await.unwrap();
}

#[tokio::test]
async fn a_prepared_transfer_is_the_same_bytes_each_time_and_holds_its_encode_until_it_ends() {
    // No budget at all: an encode is kept only while something holds it.
    let setup = Setup::new(2, 0);
    let (encoder, mut started) = held();
    let request = |title: &str| TransferRequest {
        master: master(0),
        audio: TransferAudio::Encode(encoding(encoder.clone())),
        tagging: Arc::new(Tagging {
            tags: Tags {
                title: Some(title.into()),
                ritornel_id: Some("1:a:b:c".into()),
                ..Tags::default()
            },
            cover: None,
            id3v1: true,
        }),
    };
    let work = async {
        // Two transfers of one track are one encode, tagged twice.
        let one = setup.pool.prepare(request("One"));
        let two = setup.pool.prepare(request("One"));
        let control = async {
            started.recv().await.unwrap().finish.send(Ok(())).unwrap();
        };
        let (one, two, ()) = tokio::join!(one, two, control);
        let (one, two) = (one.unwrap(), two.unwrap());
        assert!(started.try_recv().is_err());
        assert_ne!(one.id(), two.id());
        assert_eq!(
            (one.len(), one.digest(), one.sampled_hash()),
            (two.len(), two.digest(), two.sampled_hash())
        );
        // A caller gets a reader of exactly the bytes it was told of.
        let mut bytes = Vec::new();
        io::Read::read_to_end(&mut one.reader().unwrap(), &mut bytes).unwrap();
        assert_eq!(bytes.len() as u64, one.len());
        assert_eq!(blake3::hash(&bytes), one.digest());
        assert_eq!(one.codec(), Codec::Mp3);
        // Other tags are other bytes, from the same encode.
        let other = setup.pool.prepare(request("Two")).await.unwrap();
        assert_ne!(other.digest(), one.digest());
        assert!(started.try_recv().is_err());
        // Each transfer owns one scratch file, which goes when it ends, and
        // the encode they were tagged from goes with the last of them.
        assert_eq!(scratch(&setup), 3);
        let encodes = || -> i64 {
            setup
                .store
                .read(|db| {
                    Ok(db.query_row("SELECT count(*) FROM transcode_entry", [], |r| r.get(0))?)
                })
                .unwrap()
        };
        assert_eq!(encodes(), 1);
        drop((one, two));
        assert_eq!(scratch(&setup), 1);
        assert_eq!(encodes(), 1, "a transfer still holds its encode");
        drop(other);
        assert_eq!(scratch(&setup), 0);
        while encodes() != 0 {
            // sleep: eviction runs on a worker after the last pin drops.
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    };
    tokio::time::timeout(WAIT, work).await.unwrap();
}

#[tokio::test]
async fn one_failed_item_is_its_own_result_and_the_rest_of_its_batch_is_made() {
    let setup = Setup::new(2, u64::MAX);
    let job = setup.job(1);
    let fast: Arc<dyn Encoder> = Arc::new(FakeEncoder);
    let mut offline = item(1, &fast);
    offline.master.path = setup.dir.path().join("offline.flac");
    let batch = setup
        .pool
        .ensure_batch(vec![item(0, &fast), offline, item(2, &fast)], &job, |_| {})
        .await
        .unwrap();
    let results: Vec<bool> = batch.results().map(|result| result.is_ok()).collect();
    assert_eq!(results, [true, false, true]);
    // A transfer whose master is gone fails alone too.
    let mut gone = as_it_is(3, "Gone");
    gone.master.path = setup.dir.path().join("offline.flac");
    assert!(setup.pool.prepare(gone).await.is_err());
    assert_eq!(scratch(&setup), 0);
}

#[cfg(unix)]
#[tokio::test]
async fn the_last_waiter_cancels_and_reaps_the_cli_and_removes_all_staging() {
    use rustix::process::{Pid, WaitOptions, waitpid};
    use std::process::Command;
    struct SleepingCli(PathBuf);
    impl Encoder for SleepingCli {
        fn probe(&self) -> io::Result<Option<EncoderId>> {
            FakeEncoder.probe()
        }
        fn supports(&self, p: Profile) -> bool {
            FakeEncoder.supports(p)
        }
        fn encode(
            &self,
            pcm: Pcm,
            p: Profile,
            _: blake3::Hash,
            out: &Path,
            cancel: &AtomicBool,
        ) -> io::Result<()> {
            super::super::cli::encode(
                Command::new("sh")
                    .args(["-c", r#"echo $$ > "$0"; exec sleep 300"#])
                    .arg(&self.0),
                pcm,
                p,
                out,
                cancel,
            )
        }
    }
    let setup = Setup::new(1, 0);
    let marker = setup.dir.path().join("pid");
    let encoder: Arc<dyn Encoder> = Arc::new(SleepingCli(marker.clone()));
    let job = setup.job(10);
    let mut run = Box::pin(setup.pool.ensure_batch(
        vec![item(0, &encoder), item(1, &encoder)],
        &job,
        |_| {},
    ));
    let read_pid = async {
        loop {
            if let Ok(pid) = fs::read_to_string(&marker)
                && let Ok(pid) = pid.trim().parse::<i32>()
            {
                break pid;
            }
            // sleep: the child announces that it is running by writing its pid file.
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    };
    let pid = tokio::time::timeout(WAIT, async {
        tokio::select! {
            _ = &mut run => panic!("encoder exited before announcing its pid"),
            pid = read_pid => pid,
        }
    })
    .await
    .unwrap();
    setup.store.cancels.lock().unwrap().insert(10);
    assert!(matches!(
        &*tokio::time::timeout(WAIT, run).await.unwrap().unwrap_err(),
        Error::JobStopped(JobStop::Cancelled)
    ));
    drop(setup.pool);
    let child = Pid::from_raw(pid).unwrap();
    assert_eq!(
        waitpid(Some(child), WaitOptions::NOHANG).unwrap_err(),
        rustix::io::Errno::CHILD
    );
    assert_eq!(setup.dir.path().join("tmp").read_dir().unwrap().count(), 0);
    let (entries, pins) = setup
        .store
        .read(|db| {
            Ok(db.query_row(
                "SELECT (SELECT count(*) FROM transcode_entry),
                        (SELECT count(*) FROM transcode_pin)",
                [],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
            )?)
        })
        .unwrap();
    assert_eq!((entries, pins), (0, 0));
}

#[tokio::test]
async fn dropping_an_incomplete_batch_releases_earlier_results_and_stops_the_last_encode() {
    let setup = Setup::new(1, 0);
    let job = setup.job(1);
    let (held, mut started) = held();
    let fast: Arc<dyn Encoder> = Arc::new(FakeEncoder);
    let mut progress = Vec::new();
    let mut run = Box::pin(setup.pool.ensure_batch(
        vec![item(0, &fast), item(1, &held)],
        &job,
        |p| progress.push(p.index),
    ));
    let _hold = tokio::time::timeout(WAIT, async {
        tokio::select! {
            result = &mut run => panic!("batch ended before the second encoder was released: {result:?}"),
            running = started.recv() => running.unwrap(),
        }
    })
    .await
    .unwrap();
    drop(run);
    assert_eq!(progress, [0]);
    tokio::time::timeout(WAIT, async {
        loop {
            let count: i64 = setup
                .store
                .read(|db| {
                    Ok(db.query_row("SELECT count(*) FROM transcode_entry", [], |row| row.get(0))?)
                })
                .unwrap();
            if count == 0 && setup.dir.path().join("tmp").read_dir().unwrap().count() == 0 {
                break;
            }
            // sleep: the abandoned worker must unwind and drain its queued pin release.
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn closing_the_pool_reports_a_failed_deferred_eviction() {
    let setup = Setup::new(1, 0);
    let fast: Arc<dyn Encoder> = Arc::new(FakeEncoder);
    let job = setup.job(1);
    let batch = setup
        .pool
        .ensure_batch(vec![item(0, &fast)], &job, |_| {})
        .await
        .unwrap();
    let path = entries(&batch)[0].path().to_owned();
    fs::remove_file(&path).unwrap();
    fs::create_dir(path).unwrap();
    drop(batch);
    let pool = Arc::try_unwrap(setup.pool).unwrap();
    assert!(matches!(pool.close(), Err(Error::Io(_))));
    let indexed: i64 = setup
        .store
        .read(|db| Ok(db.query_row("SELECT count(*) FROM transcode_entry", [], |row| row.get(0))?))
        .unwrap();
    assert_eq!(indexed, 1, "an unsuccessful unlink must be retryable");
}

#[tokio::test]
async fn cached_audio_is_available_with_every_encoder_busy() {
    let setup = Setup::new(1, u64::MAX);
    let job = setup.job(1);
    let fast: Arc<dyn Encoder> = Arc::new(FakeEncoder);
    let cached = vec![item(0, &fast)];
    let first = setup
        .pool
        .ensure_batch(cached.clone(), &job, |_| {})
        .await
        .unwrap();
    let (encoder, mut started) = held();
    let mut busy = Box::pin(
        setup
            .pool
            .ensure_batch(vec![item(2, &encoder)], &job, |_| {}),
    );
    let active = wait_for_encoder(&mut busy, &mut started).await;
    let hit = tokio::time::timeout(WAIT, setup.pool.ensure_batch(cached, &job, |_| {}))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(entries(&hit)[0].key(), entries(&first)[0].key());
    active.finish.send(Ok(())).unwrap();
    busy.await.unwrap();
}

#[tokio::test]
async fn a_retry_waits_for_cleanup_while_another_track_can_be_made() {
    struct Unwind {
        encoder: Held,
        unwinding: tokio::sync::mpsc::UnboundedSender<mpsc::Sender<()>>,
    }
    impl Encoder for Unwind {
        fn probe(&self) -> io::Result<Option<EncoderId>> {
            self.encoder.probe()
        }
        fn supports(&self, profile: Profile) -> bool {
            self.encoder.supports(profile)
        }
        fn encode(
            &self,
            pcm: Pcm,
            p: Profile,
            k: blake3::Hash,
            out: &Path,
            cancel: &AtomicBool,
        ) -> io::Result<()> {
            let result = self.encoder.encode(pcm, p, k, out, cancel);
            if cancel.load(Ordering::Acquire) {
                let (release, wait) = mpsc::channel();
                self.unwinding.send(release).unwrap();
                let _ = wait.recv_timeout(WAIT);
            }
            result
        }
    }
    let setup = Setup::new(2, 0);
    let job = setup.job(1);
    let (start, mut started) = tokio::sync::mpsc::unbounded_channel();
    let (unwinding, mut stopped) = tokio::sync::mpsc::unbounded_channel();
    let encoder: Arc<dyn Encoder> = Arc::new(Unwind {
        encoder: Held(start),
        unwinding,
    });
    let mut old = Box::pin(
        setup
            .pool
            .ensure_batch(vec![item(0, &encoder)], &job, |_| {}),
    );
    let _old = wait_for_encoder(&mut old, &mut started).await;
    drop(old);
    let release = tokio::time::timeout(WAIT, stopped.recv())
        .await
        .unwrap()
        .unwrap();
    let mut retry = Box::pin(
        setup
            .pool
            .ensure_batch(vec![item(0, &encoder)], &job, |_| {}),
    );
    assert!(retry.as_mut().now_or_never().is_none());
    let other = setup.pool.prepare(as_it_is(1, "Other"));
    tokio::time::timeout(WAIT, other).await.unwrap().unwrap();
    assert!(started.try_recv().is_err());
    release.send(()).unwrap();
    let active = wait_for_encoder(&mut retry, &mut started).await;
    // Tag another master while the second encode is still consuming its worker.
    tokio::time::timeout(WAIT, setup.pool.prepare(as_it_is(2, "Another")))
        .await
        .unwrap()
        .unwrap();
    active.finish.send(Ok(())).unwrap();
    retry.await.unwrap();
}
