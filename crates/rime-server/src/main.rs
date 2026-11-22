//! RimeMC — a from-scratch, multi-core Minecraft server.

// Anti-cheat, Player Cost and skin conversion are complete and unit-tested;
// they are attached to live player sessions by the play-state protocol.
#[allow(dead_code)]
mod anticheat;
mod components;
mod config;
mod control;
mod data;
mod fleet_net;
mod game;
mod net;
#[allow(dead_code)]
mod nbt;
mod ops;
mod plugins;
#[allow(dead_code)]
mod registry;
mod tick;
mod vanilla;
mod world;
mod worldgen;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand};
use parking_lot::Mutex;
use rime_fleet::balancer::CellId;
use tokio::io::AsyncBufReadExt;
use tokio::sync::Notify;

use config::{Config, FleetMode};
use ops::{CommandSender, Ops};
use world::generator::Generator;
use world::pregen::{predict, Pregen};
use world::storage::{Dim, Kind, WorldStore};

#[derive(Parser)]
#[command(name = "rime", version, about = "RimeMC server")]
struct Cli {
    /// Server directory (contains rime.toml and the world)
    #[arg(long, global = true, default_value = ".")]
    dir: PathBuf,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Start the server (default)
    Run,
    /// Write the RAM world to disk now (operators only: needs access to the server directory)
    Sync,
    /// Show server status
    Status,
    /// Stop the running server gracefully
    Stop,
    /// Print a new random RimeFleet secret
    GenSecret,
}

pub struct ServerState {
    pub cfg: Config,
    pub store: WorldStore,
    pub generator: Arc<Generator>,
    /// generators of the nether and the end
    pub nether_gen: Arc<Generator>,
    pub end_gen: Arc<Generator>,
    pub ops: Ops,
    /// the overworld
    pub game: Arc<game::Game>,
    pub nether: Arc<game::Game>,
    pub end: Arc<game::Game>,
    pub config_data: net::config_data::ConfigData,
    pub seed: i64,
    /// where new players appear (feet position)
    pub spawn: [f64; 3],
    /// world directory (player data lives in `playerdata/`)
    pub world_dir: std::path::PathBuf,
    pub pregen: Mutex<Option<Pregen>>,
    bans: Mutex<std::collections::HashMap<String, Instant>>,
    pub online: AtomicU32,
    mspt_bits: AtomicU64,
    redstone_role: AtomicBool,
    cell_costs: Mutex<std::collections::HashMap<CellId, f32>>,
    shutdown: Notify,
    stopping: AtomicBool,
    started: Instant,
}

impl ServerState {
    /// The game state of a dimension.
    pub fn level(&self, d: Dim) -> &Arc<game::Game> {
        match d {
            Dim::Overworld => &self.game,
            Dim::Nether => &self.nether,
            Dim::End => &self.end,
        }
    }
    pub fn generator_of(&self, d: Dim) -> &Arc<Generator> {
        match d {
            Dim::Overworld => &self.generator,
            Dim::Nether => &self.nether_gen,
            Dim::End => &self.end_gen,
        }
    }
    pub fn levels(&self) -> [&Arc<game::Game>; 3] {
        [&self.game, &self.nether, &self.end]
    }
    /// Everyone online, in any dimension.
    pub fn all_players(&self) -> Vec<Arc<game::Player>> {
        self.levels().iter().flat_map(|g| g.players()).collect()
    }
    pub fn find_player(&self, name: &str) -> Option<Arc<game::Player>> {
        self.levels().iter().find_map(|g| g.find(name))
    }
    /// The dimension a player is in.
    pub fn level_of(&self, p: &game::Player) -> &Arc<game::Game> {
        self.levels().into_iter().find(|g| g.players().iter().any(|q| q.eid == p.eid)).unwrap_or(&self.game)
    }
    /// A packet to every player in every dimension.
    pub fn broadcast_all(&self, pkt: Arc<Vec<u8>>) {
        for p in self.all_players() {
            p.send(pkt.clone());
        }
    }

    /// Writes every online player's file (autosave, sync, shutdown).
    pub fn save_players(&self) {
        for p in self.all_players() {
            if let Err(e) = game::playerdata::save(&self.world_dir, &p) {
                tracing::error!("saving player {}: {e:#}", p.name);
            }
        }
        for g in self.levels() {
            if let Err(e) = g.save_portals(&portals_file(&self.world_dir, g.dim)) {
                tracing::error!("saving portals: {e:#}");
            }
        }
    }
    pub fn online_players(&self) -> u32 {
        self.online.load(Ordering::Relaxed)
    }
    pub fn mspt(&self) -> f32 {
        f64::from_bits(self.mspt_bits.load(Ordering::Relaxed)) as f32
    }
    /// Temporary anti-cheat ban (kept until it expires or the server restarts).
    pub fn ban(&self, name: &str, minutes: u64) {
        self.bans.lock().insert(name.to_ascii_lowercase(), Instant::now() + Duration::from_secs(minutes * 60));
    }
    /// Seconds left on a ban, if any.
    pub fn banned_for(&self, name: &str) -> Option<u64> {
        let mut b = self.bans.lock();
        let key = name.to_ascii_lowercase();
        match b.get(&key) {
            Some(until) if *until > Instant::now() => Some((*until - Instant::now()).as_secs()),
            Some(_) => {
                b.remove(&key);
                None
            }
            None => None,
        }
    }
    pub fn cell_cost(&self, c: &CellId) -> f32 {
        self.cell_costs.lock().get(c).copied().unwrap_or(0.0)
    }
    pub fn set_redstone_role(&self, active: bool) {
        if self.redstone_role.swap(active, Ordering::Relaxed) != active {
            tracing::info!("RimeFleet: this node {} the redstone node", if active { "is now" } else { "is no longer" });
        }
    }
    fn request_stop(&self) {
        self.stopping.store(true, Ordering::SeqCst);
        self.shutdown.notify_waiters();
        self.shutdown.notify_one();
    }

    pub async fn sync(self: &Arc<Self>) -> String {
        let st = self.clone();
        match tokio::task::spawn_blocking(move || {
            st.save_players();
            for g in st.levels() {
                g.stash_mobs(false);
                g.world.save(&st.store, true);
            }
            st.store.flush()
        })
        .await
        {
            Ok(r) if r.failures == 0 => format!("OK synced {} region(s) to disk in {} ms", r.regions_written, r.millis),
            Ok(r) => format!("ERR {} region(s) failed to save ({} saved); see server log", r.failures, r.regions_written),
            Err(e) => format!("ERR sync task failed: {e}"),
        }
    }

    pub fn status(&self) -> String {
        format!(
            "RimeMC {} | uptime {}s | players {}/{} | mspt {:.2} | RAM world {:.1} MiB, {} dirty region(s) | {} chunk(s) loaded | generator {} | fleet {:?}",
            env!("CARGO_PKG_VERSION"),
            self.started.elapsed().as_secs(),
            self.online_players(),
            self.cfg.server.max_players,
            self.mspt(),
            self.store.memory_used() as f64 / 1048576.0,
            self.store.dirty_regions(),
            self.levels().iter().map(|g| g.world.loaded()).sum::<usize>(),
            if self.generator.is_vanilla() { "vanilla" } else if self.generator.gpu_active() { "on" } else { "off" },
            self.cfg.fleet.mode
        )
    }

    /// Commands from the control socket (always operator level).
    pub async fn run_admin_command(self: &Arc<Self>, cmd: &str) -> String {
        self.dispatch(&CommandSender::Console, cmd).await
    }

    /// Unified command dispatcher with permission checks.
    pub async fn dispatch(self: &Arc<Self>, sender: &CommandSender, line: &str) -> String {
        let line = line.trim().trim_start_matches('/');
        let mut words = line.split_whitespace();
        let first = words.next().unwrap_or("");
        let (cmd, arg) = if first == "rime" { (words.next().unwrap_or("help"), words.next()) } else { (first, words.next()) };
        let needs_op = matches!(cmd, "sync" | "stop" | "op" | "deop" | "status");
        if needs_op && !self.ops.is_op(sender) {
            return "ERR You do not have permission (operator only).".into();
        }
        match cmd {
            "sync" => self.sync().await,
            "status" => self.status(),
            "stop" => {
                self.request_stop();
                "OK stopping".into()
            }
            "op" | "deop" => match arg {
                Some(name) if net::java::valid_username(name) => {
                    let r = if cmd == "op" { self.ops.add(name) } else { self.ops.remove(name) };
                    match r {
                        Ok(()) => format!("OK {cmd} {name}"),
                        Err(e) => format!("ERR {e}"),
                    }
                }
                _ => format!("ERR usage: {cmd} <player>"),
            },
            "help" | "" => "commands: rime sync | rime status | stop | op <player> | deop <player>".into(),
            _ => format!("ERR unknown command '{cmd}'"),
        }
    }
}

/// Nether portal positions of a dimension (vanilla keeps them as POIs).
fn portals_file(world_dir: &Path, d: Dim) -> std::path::PathBuf {
    world_dir.join(format!("rime-portals-{}.txt", d.key().trim_start_matches("minecraft:")))
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    tracing_subscriber::fmt().with_target(false).init();
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().thread_name("rime-io").build()?;
    match cli.cmd.unwrap_or(Cmd::Run) {
        Cmd::Run => run(&cli.dir, rt),
        Cmd::Sync => client(&cli.dir, rt, "rime sync"),
        Cmd::Status => client(&cli.dir, rt, "rime status"),
        Cmd::Stop => client(&cli.dir, rt, "stop"),
        Cmd::GenSecret => {
            println!("{}", hex::encode(rand::random::<[u8; 32]>()));
            Ok(())
        }
    }
}

fn client(dir: &Path, rt: tokio::runtime::Runtime, cmd: &str) -> anyhow::Result<()> {
    let reply = rt.block_on(control::send(dir, cmd))?;
    println!("{reply}");
    if reply.starts_with("ERR") {
        std::process::exit(1);
    }
    Ok(())
}

fn run(dir: &Path, rt: tokio::runtime::Runtime) -> anyhow::Result<()> {
    let t0 = Instant::now();
    std::fs::create_dir_all(dir)?;
    let dir = dir.canonicalize()?;
    let cfg = Config::load_or_create(&dir.join("rime.toml"))?;
    let threads = cfg.worker_threads();
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .thread_name(|i| format!("rime-worker-{i}"))
        .build_global()
        .map_err(|e| anyhow::anyhow!("thread pool: {e}"))?;
    tracing::info!("RimeMC {} starting with {threads} worker thread(s)", env!("CARGO_PKG_VERSION"));

    let world_dir = dir.join(&cfg.world.name);
    let store = WorldStore::open(&world_dir, cfg.world.format, cfg.world.compression_level, cfg.ram_world.enabled, cfg.ram_world.max_memory_mb)?;
    if cfg.ram_world.enabled {
        let (n, bytes) = store.preload()?;
        tracing::info!("RAM world: {n} region file(s) loaded into memory ({:.1} MiB)", bytes as f64 / 1048576.0);
    }
    let seed = load_or_create_seed(&world_dir, cfg.world.seed)?;
    let ops = Ops::load(&dir.join("ops.json"))?;

    // official vanilla data (registries, tags, worldgen) — downloaded from Mojang once
    let vanilla = vanilla::VanillaData::ensure(&dir)?;
    let mut generator = Generator::new(seed, cfg.world.generator == config::GeneratorKind::Fast && cfg.gpu.enabled, cfg.gpu.min_batch);
    if cfg.world.generator == config::GeneratorKind::Vanilla {
        let load = |rel: &str| match rel.strip_prefix("structure/") {
            Some(_) => crate::nbt::gz_file_json(&vanilla.path(&format!("minecraft/{rel}.nbt"))),
            None => vanilla.read_json(&format!("minecraft/{rel}.json")),
        };
        let t = std::time::Instant::now();
        generator = generator.with_vanilla(worldgen::VanillaGenerator::new(&load, seed)?);
        tracing::info!("vanilla 26.1 world generation ready (seed {seed}, {} ms); runs on all CPU cores", t.elapsed().as_millis());
    }
    let generator = Arc::new(generator);
    // the nether and the end always use the vanilla generator
    let dim_gen = |d: worldgen::DimKind| -> anyhow::Result<Arc<Generator>> {
        let load = |rel: &str| match rel.strip_prefix("structure/") {
            Some(_) => crate::nbt::gz_file_json(&vanilla.path(&format!("minecraft/{rel}.nbt"))),
            None => vanilla.read_json(&format!("minecraft/{rel}.json")),
        };
        let t = std::time::Instant::now();
        let g = Generator::new(seed, false, cfg.gpu.min_batch).with_vanilla(worldgen::VanillaGenerator::new_dim(&load, seed, d)?);
        tracing::info!("{d:?} generation ready ({} ms)", t.elapsed().as_millis());
        Ok(Arc::new(g))
    };
    let nether_gen = dim_gen(worldgen::DimKind::Nether)?;
    let end_gen = dim_gen(worldgen::DimKind::End)?;
    // compute the stronghold rings (vanilla's slow first-chunk step) in the
    // background instead of on the first player's chunk request
    // (cached in the world folder: it takes vanilla's biome search ~30 s)
    if let Some(v) = generator.vanilla() {
        let cache = world_dir.join("rime-strongholds.txt");
        if let Some(r) = load_rings(&cache, seed) {
            v.set_rings(r);
        } else {
            let g = generator.clone();
            std::thread::Builder::new().name("rime-warmup".into()).spawn(move || {
                let t = std::time::Instant::now();
                let Some(v) = g.vanilla() else { return };
                v.warm_up();
                if let Some(r) = v.rings() {
                    save_rings(&cache, seed, &r);
                }
                tracing::info!("stronghold placement computed in {} ms", t.elapsed().as_millis());
            })?;
        }
    }
    backup_on_generator_change(&world_dir, generator.gen_id())?;
    let config_data = net::config_data::ConfigData::build(&vanilla)?;
    components::set_data_registries(config_data.names());
    tracing::info!("loaded {} synchronized registries from vanilla {} data", config_data.registries.len(), registry::blocks::MC_VERSION);

    game::combat::load(&vanilla)?;
    let game_data = data::GameData::load(&vanilla)?;
    tracing::info!("loaded {} crafting recipes and vanilla loot tables", game_data.recipes.len());
    let game_data = Arc::new(game_data);
    let game = Arc::new(game::Game::new_dim(cfg.server.compression_threshold, game_data.clone(), Dim::Overworld));
    let nether = Arc::new(game::Game::new_dim(cfg.server.compression_threshold, game_data.clone(), Dim::Nether));
    let end = Arc::new(game::Game::new_dim(cfg.server.compression_threshold, game_data, Dim::End));
    for g in [&game, &nether, &end] {
        g.load_portals(&portals_file(&world_dir, g.dim));
    }
    let spawn = load_or_find_spawn(&world_dir, &game, &store, &generator)?;
    tracing::info!("world spawn at {:.0} {:.0} {:.0}", spawn[0], spawn[1], spawn[2]);

    let state = Arc::new(ServerState {
        cfg,
        store,
        generator,
        nether_gen,
        end_gen,
        ops,
        game,
        nether,
        end,
        config_data,
        seed,
        spawn,
        world_dir: world_dir.clone(),
        pregen: Mutex::new(None),
        bans: Mutex::new(Default::default()),
        online: AtomicU32::new(0),
        mspt_bits: AtomicU64::new(0f64.to_bits()),
        redstone_role: AtomicBool::new(false),
        cell_costs: Mutex::new(Default::default()),
        shutdown: Notify::new(),
        stopping: AtomicBool::new(false),
        started: Instant::now(),
    });

    // ---- pre-generation pool (leaves one core for the tick thread)
    if state.cfg.pregen.enabled {
        let threads = if state.cfg.pregen.threads == 0 { threads.saturating_sub(1).max(1) } else { state.cfg.pregen.threads };
        let (s1, s2) = (state.clone(), state.clone());
        let p = Pregen::start(
            state.generator.clone(),
            threads,
            state.cfg.pregen.max_queue,
            Arc::new(move |x, z| s1.store.has_chunk(Dim::Overworld, x, z)),
            Arc::new(move |c| {
                let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0);
                if let Err(e) = s2.store.save_chunk(Dim::Overworld, Kind::Region, c.x, c.z, &c.to_nbt(now)) {
                    tracing::error!("saving generated chunk {},{}: {e:#}", c.x, c.z);
                }
            }),
        );
        // spawn area first: everything within view distance of the spawn
        let view = state.cfg.server.view_distance as i32;
        let (scx, scz) = ((state.spawn[0].floor() as i32) >> 4, (state.spawn[2].floor() as i32) >> 4);
        let mut spawn: Vec<(i32, i32)> = (-view..=view).flat_map(|x| (-view..=view).map(move |z| (x, z))).collect();
        spawn.sort_by_key(|(x, z)| x * x + z * z);
        let spawn: Vec<(i32, i32)> = spawn.into_iter().map(|(x, z)| (x + scx, z + scz)).collect();
        p.enqueue(&spawn);
        p.enqueue(&predict((state.spawn[0], state.spawn[2]), (0.0, 0.0), view, 1.0, state.cfg.pregen.candidates));
        *state.pregen.lock() = Some(p);
    }

    // ---- Bukkit plugins (only when the plugin folder holds jars)
    {
        let _rt = rt.enter();
        if let Err(e) = plugins::start(&state, &dir) {
            tracing::error!("plugins disabled: {e:#}");
        }
    }

    // ---- tick thread (20 TPS, fixed timestep, parallel cells)
    let tick_thread = {
        let st = state.clone();
        std::thread::Builder::new().name("rime-tick".into()).spawn(move || tick_loop(st))?
    };

    let result = rt.block_on(async {
        let mut tasks = Vec::new();
        match state.cfg.fleet.mode {
            FleetMode::Node => {
                tasks.push(tokio::spawn(log_err("fleet node", fleet_net::run_node(state.clone()))));
            }
            mode => {
                if state.cfg.server.online_mode {
                    // the RSA key takes a moment: make it before the first login
                    std::thread::spawn(|| {
                        net::crypt::server_key();
                    });
                }
                tasks.push(tokio::spawn(log_err("java listener", net::java::listen(state.clone()))));
                if state.cfg.bedrock.enabled {
                    tasks.push(tokio::spawn(log_err("bedrock listener", net::bedrock::listen(state.clone()))));
                }
                if mode == FleetMode::Router {
                    tasks.push(tokio::spawn(log_err("fleet router", fleet_net::run_router(state.clone()))));
                }
            }
        }
        {
            let (st, d) = (state.clone(), dir.clone());
            tasks.push(tokio::spawn(async move { log_err("control", control::serve(st, &d)).await }));
        }
        // autosave
        {
            let st = state.clone();
            let every = Duration::from_secs(st.cfg.ram_world.save_interval_seconds);
            tasks.push(tokio::spawn(async move {
                let mut t = tokio::time::interval(every);
                t.tick().await;
                loop {
                    t.tick().await;
                    let r = st.sync().await;
                    if r.starts_with("ERR") {
                        tracing::error!("autosave: {r}");
                    } else {
                        tracing::debug!("autosave: {r}");
                    }
                }
            }));
        }
        // console
        {
            let st = state.clone();
            tasks.push(tokio::spawn(async move {
                let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if line.trim().is_empty() {
                        continue;
                    }
                    let reply = st.dispatch(&CommandSender::Console, &line).await;
                    println!("{reply}");
                }
            }));
        }
        tracing::info!("Done ({:.2}s)! Type \"help\" for commands.", t0.elapsed().as_secs_f64());
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = state.shutdown.notified() => {}
        }
        tracing::info!("stopping…");
        state.stopping.store(true, Ordering::SeqCst);
        for t in tasks {
            t.abort();
        }
        anyhow::Ok(())
    });

    let _ = tick_thread.join();
    plugins::stop();
    if let Some(p) = state.pregen.lock().take() {
        p.shutdown();
    }
    state.save_players();
    let mut saved = 0;
    for g in state.levels() {
        g.stash_mobs(true);
        saved += g.world.save(&state.store, false);
    }
    tracing::info!("saved {saved} modified chunk(s)");
    let r = state.store.flush();
    tracing::info!("saved {} region(s) on shutdown ({} failed)", r.regions_written, r.failures);
    let _ = std::fs::remove_file(dir.join(control::CONTROL_FILE));
    rt.shutdown_timeout(Duration::from_secs(2));
    if r.failures > 0 {
        anyhow::bail!("{} region(s) could not be saved", r.failures);
    }
    result
}

async fn log_err<F: std::future::Future<Output = anyhow::Result<()>>>(what: &'static str, f: F) {
    if let Err(e) = f.await {
        tracing::error!("{what} stopped: {e:#}");
    }
}

fn tick_loop(state: Arc<ServerState>) {
    let mut sched = tick::scheduler::Scheduler::new(state.cfg.fleet.cell_shift);
    let period = Duration::from_millis(50);
    let mut next = Instant::now();
    let mut avg = 0.0f64;
    while !state.stopping.load(Ordering::Relaxed) {
        let stats = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let t = Instant::now();
            for g in state.levels() {
                g.tick();
            }
            if plugins::active() {
                plugins::tick();
            }
            let mut s = game::tick_pool().install(|| sched.tick(&state.cfg.level_tick));
            s.mspt += t.elapsed().as_secs_f64() * 1000.0;
            s
        }));
        match stats {
            Ok(s) => {
                avg = avg * 0.95 + s.mspt * 0.05;
                state.mspt_bits.store(avg.to_bits(), Ordering::Relaxed);
                if s.tick % 20 == 0 {
                    let mut costs = state.cell_costs.lock();
                    costs.clear();
                    for (k, c) in &sched.cells {
                        costs.insert(CellId { world: 0, x: k.0, z: k.1 }, (c.last_ms * 20.0) as f32);
                    }
                }
            }
            Err(_) => tracing::error!("panic during tick {} was contained; continuing", sched.tick),
        }
        next += period;
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        } else if now - next > Duration::from_secs(2) {
            // We fell far behind (e.g. machine suspended): don't try to catch up in a burst.
            next = now;
        }
    }
}

/// World spawn, chosen once per world: the vanilla spawn search for
/// vanilla worlds, otherwise the surface at 0,0.
fn load_or_find_spawn(world_dir: &Path, game: &game::Game, store: &WorldStore, generator: &Generator) -> anyhow::Result<[f64; 3]> {
    let p = world_dir.join("rime-spawn.txt");
    if let Ok(s) = std::fs::read_to_string(&p) {
        let v: Vec<f64> = s.split_whitespace().filter_map(|t| t.parse().ok()).collect();
        if let [x, y, z] = v[..] {
            return Ok([x, y, z]);
        }
    }
    let spawn = if let Some(v) = generator.vanilla() {
        let (x, y, z) = v.find_spawn(&|cx, cz| v.generate_many(&[(cx, cz)]).pop().expect("chunk"));
        [x as f64 + 0.5, y as f64, z as f64 + 0.5]
    } else {
        game.world.ensure_loaded(&[(0, 0)], store, generator);
        [0.5, (game.world.surface_y(0, 0).unwrap_or(64) + 1) as f64, 0.5]
    };
    std::fs::write(&p, format!("{} {} {}", spawn[0], spawn[1], spawn[2]))?;
    Ok(spawn)
}

/// Chunks saved by another generator are generated again (unless edited by
/// players). Before the first run with a new generator the old region files
/// are copied to `region-backup-<old id>` so nothing is ever lost.
fn backup_on_generator_change(world_dir: &Path, id: u32) -> anyhow::Result<()> {
    let p = world_dir.join("rime-generator.txt");
    let old = std::fs::read_to_string(&p).ok().and_then(|s| s.trim().parse::<u32>().ok());
    if old == Some(id) {
        return Ok(());
    }
    let region = world_dir.join("region");
    if region.is_dir() && std::fs::read_dir(&region)?.next().is_some() {
        let dst = world_dir.join(format!("region-backup-{}", old.unwrap_or(0)));
        if !dst.exists() {
            std::fs::create_dir_all(&dst)?;
            for e in std::fs::read_dir(&region)? {
                let e = e?;
                if e.file_type()?.is_file() {
                    std::fs::copy(e.path(), dst.join(e.file_name()))?;
                }
            }
            tracing::info!("world generator changed: old chunks backed up to {}; unedited old chunks will be generated again", dst.display());
        }
    }
    std::fs::write(&p, id.to_string())?;
    Ok(())
}

/// `rime-strongholds.txt`: "seed <n>", then per ring set a line "set" and
/// "x z" chunk positions.
fn load_rings(path: &std::path::Path, seed: i64) -> Option<Vec<Vec<(i32, i32)>>> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut lines = text.lines();
    if lines.next()?.strip_prefix("seed ")?.trim().parse::<i64>().ok()? != seed {
        return None;
    }
    let mut out: Vec<Vec<(i32, i32)>> = Vec::new();
    for l in lines {
        if l.trim() == "set" {
            out.push(Vec::new());
            continue;
        }
        let mut it = l.split_whitespace();
        let (x, z) = (it.next()?.parse().ok()?, it.next()?.parse().ok()?);
        out.last_mut()?.push((x, z));
    }
    (!out.is_empty()).then_some(out)
}

fn save_rings(path: &std::path::Path, seed: i64, rings: &[Vec<(i32, i32)>]) {
    let mut s = format!("seed {seed}\n");
    for r in rings {
        s.push_str("set\n");
        for (x, z) in r {
            s.push_str(&format!("{x} {z}\n"));
        }
    }
    if let Err(e) = std::fs::write(path, s) {
        tracing::warn!("could not cache stronghold placement: {e}");
    }
}

fn load_or_create_seed(world_dir: &Path, cfg_seed: i64) -> anyhow::Result<i64> {
    let p = world_dir.join("rime-seed.txt");
    if let Ok(s) = std::fs::read_to_string(&p) {
        if let Ok(v) = s.trim().parse() {
            return Ok(v);
        }
    }
    let seed = if cfg_seed != 0 { cfg_seed } else { rand::random() };
    std::fs::write(&p, seed.to_string())?;
    Ok(seed)
}
