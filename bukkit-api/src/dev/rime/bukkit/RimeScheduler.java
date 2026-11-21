package dev.rime.bukkit;

import java.util.ArrayList;
import java.util.Iterator;
import java.util.List;

import org.bukkit.plugin.Plugin;
import org.bukkit.scheduler.BukkitScheduler;
import org.bukkit.scheduler.BukkitTask;

/** Sync tasks run on the server tick (20/s); async tasks on their own threads. */
public final class RimeScheduler implements BukkitScheduler {
    public static final RimeScheduler INSTANCE = new RimeScheduler();
    private final List<Task> tasks = new ArrayList<>();
    private long tick;
    private int nextId = 1;

    final class Task implements BukkitTask {
        final int id; final Plugin plugin; final Runnable r; final long period; long next; volatile boolean cancelled; final boolean sync;
        Task(Plugin p, Runnable r, long delay, long period, boolean sync) {
            this.id = nextId++; this.plugin = p; this.r = r; this.period = period; this.next = tick + Math.max(0, delay); this.sync = sync;
        }
        @Override public int getTaskId() { return id; }
        @Override public Plugin getOwner() { return plugin; }
        @Override public boolean isSync() { return sync; }
        @Override public boolean isCancelled() { return cancelled; }
        @Override public void cancel() { cancelled = true; }
    }

    synchronized Task add(Plugin p, Runnable r, long delay, long period) {
        if (p == null || !p.isEnabled()) throw new IllegalStateException("Plugin attempted to register task while disabled");
        Task t = new Task(p, r, delay, period, true);
        tasks.add(t);
        return t;
    }

    void tick() {
        List<Task> due = new ArrayList<>();
        synchronized (this) {
            tick++;
            for (Iterator<Task> it = tasks.iterator(); it.hasNext(); ) {
                Task t = it.next();
                if (t.cancelled) { it.remove(); continue; }
                if (t.next <= tick) {
                    due.add(t);
                    if (t.period > 0) t.next = tick + t.period; else it.remove();
                }
            }
        }
        for (Task t : due) {
            if (t.cancelled) continue;
            try { t.r.run(); }
            catch (Throwable e) { Bridge.log(1, "Task #" + t.id + " of " + t.plugin.getName() + " threw " + e); }
        }
    }

    synchronized void cancelTasks(Plugin p) { for (Task t : tasks) if (t.plugin == p) t.cancelled = true; }

    @Override public BukkitTask runTask(Plugin p, Runnable r) { return add(p, r, 0, -1); }
    @Override public BukkitTask runTaskLater(Plugin p, Runnable r, long delay) { return add(p, r, delay, -1); }
    @Override public BukkitTask runTaskTimer(Plugin p, Runnable r, long delay, long period) { return add(p, r, delay, Math.max(1, period)); }
    @Override public BukkitTask runTaskAsynchronously(Plugin p, Runnable r) {
        Task t = new Task(p, r, 0, -1, false);
        Thread th = new Thread(() -> { if (!t.cancelled) r.run(); }, "rime-plugin-async-" + t.id);
        th.setDaemon(true);
        th.start();
        return t;
    }
    @Override public int scheduleSyncDelayedTask(Plugin p, Runnable r, long delay) { return runTaskLater(p, r, delay).getTaskId(); }
    @Override public int scheduleSyncDelayedTask(Plugin p, Runnable r) { return runTask(p, r).getTaskId(); }
    @Override public int scheduleSyncRepeatingTask(Plugin p, Runnable r, long delay, long period) { return runTaskTimer(p, r, delay, period).getTaskId(); }
    @Override public synchronized void cancelTask(int id) { for (Task t : tasks) if (t.id == id) t.cancelled = true; }
    @Override public void cancelTasks(Plugin p, boolean unused) { cancelTasks(p); }
    @Override public synchronized boolean isQueued(int id) { for (Task t : tasks) if (t.id == id && !t.cancelled) return true; return false; }
}
