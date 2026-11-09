//! Plays a project on the silent clock for 4 s and compares the transport with the wall clock;
//! also reports the render load per block and what the audio thread allocated. With
//! `--load-while-playing` the media loads while the clock runs (one thread per file), as when a
//! set is opened during a show.
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use scythe_core::render_settings::ResampleMode;
use scythe_core::rpp::{read_project, resolve_media_path};
use scythe_engine::{Engine, MediaPool};

struct CountAudioAllocs;

static AUDIO_ALLOCS: AtomicU64 = AtomicU64::new(0);

thread_local! {
    static IS_AUDIO: Cell<Option<bool>> = const { Cell::new(None) };
}

unsafe impl GlobalAlloc for CountAudioAllocs {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let audio = IS_AUDIO.try_with(|c| {
            c.get().unwrap_or_else(|| {
                // Naming the thread may allocate; it counts as not the audio thread meanwhile.
                c.set(Some(false));
                let audio = std::thread::current().name() == Some("scythe-null-audio");
                c.set(Some(audio));
                audio
            })
        });
        if audio == Ok(true) {
            AUDIO_ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: CountAudioAllocs = CountAudioAllocs;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let background = args.iter().any(|a| a == "--load-while-playing");
    let mut args = args.iter().filter(|a| !a.starts_with("--"));
    let path = args.next().expect("project");
    let start: f64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(0.0);
    let project = read_project(path.as_ref()).unwrap();
    let pool = Arc::new(MediaPool::with_resample(ResampleMode::PLAYBACK));
    let mut engine = Engine::start_null(pool.clone(), 48_000, None);
    let started = Instant::now();
    let loaders: Vec<_> = project
        .tracks
        .iter()
        .flat_map(|t| &t.items)
        .flat_map(|i| &i.takes)
        .filter_map(|t| t.source.file().map(|f| resolve_media_path(&project, f)))
        .map(|file| {
            let pool = pool.clone();
            std::thread::spawn(move || {
                let _ = pool.load(&file, 48_000);
            })
        })
        .collect();
    let mut loaders = Some(loaders);
    let wait = |loaders: &mut Option<Vec<std::thread::JoinHandle<()>>>| {
        loaders.take().into_iter().flatten().for_each(|l| l.join().unwrap());
        println!("media loaded in {:.2} s", started.elapsed().as_secs_f64());
    };
    if !background {
        wait(&mut loaders);
    }
    engine.sync(&project);
    engine.seek(start);
    let allocs = AUDIO_ALLOCS.load(Ordering::Relaxed);
    engine.play();
    let (from, at) = (engine.transport().position_seconds(), Instant::now());
    let (mut peak, mut sum, mut n) = (0.0f32, 0.0f32, 0);
    while at.elapsed() < Duration::from_secs(4) {
        std::thread::sleep(Duration::from_millis(50));
        let load = engine.transport().load();
        peak = peak.max(load);
        sum += load;
        n += 1;
    }
    let played = engine.transport().position_seconds() - from;
    let wall = at.elapsed().as_secs_f64();
    println!(
        "played {played:.3} s in {wall:.3} s: clock rate {:.3}; load average {:.1} %, peak {:.1} %; \
         {} allocations on the audio thread",
        played / wall,
        sum / n as f32 * 100.0,
        peak * 100.0,
        AUDIO_ALLOCS.load(Ordering::Relaxed) - allocs
    );
    if background {
        wait(&mut loaders);
    }
}
