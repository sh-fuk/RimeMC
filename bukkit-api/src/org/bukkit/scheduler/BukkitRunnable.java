package org.bukkit.scheduler;

import org.bukkit.Bukkit;
import org.bukkit.plugin.Plugin;

public abstract class BukkitRunnable implements Runnable {
    private BukkitTask task;
    public synchronized void cancel() { if (task == null) throw new IllegalStateException("Not scheduled yet"); task.cancel(); }
    public synchronized boolean isCancelled() { return task != null && task.isCancelled(); }
    public synchronized BukkitTask runTask(Plugin p) { check(); return task = Bukkit.getScheduler().runTask(p, this); }
    public synchronized BukkitTask runTaskLater(Plugin p, long delay) { check(); return task = Bukkit.getScheduler().runTaskLater(p, this, delay); }
    public synchronized BukkitTask runTaskTimer(Plugin p, long delay, long period) { check(); return task = Bukkit.getScheduler().runTaskTimer(p, this, delay, period); }
    public synchronized BukkitTask runTaskAsynchronously(Plugin p) { check(); return task = Bukkit.getScheduler().runTaskAsynchronously(p, this); }
    public synchronized int getTaskId() { if (task == null) throw new IllegalStateException("Not scheduled yet"); return task.getTaskId(); }
    private void check() { if (task != null) throw new IllegalStateException("Already scheduled as " + task.getTaskId()); }
}
