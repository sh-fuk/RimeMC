package org.bukkit.scheduler;

import org.bukkit.plugin.Plugin;

public interface BukkitScheduler {
    BukkitTask runTask(Plugin plugin, Runnable task);
    BukkitTask runTaskLater(Plugin plugin, Runnable task, long delay);
    BukkitTask runTaskTimer(Plugin plugin, Runnable task, long delay, long period);
    BukkitTask runTaskAsynchronously(Plugin plugin, Runnable task);
    int scheduleSyncDelayedTask(Plugin plugin, Runnable task, long delay);
    int scheduleSyncDelayedTask(Plugin plugin, Runnable task);
    int scheduleSyncRepeatingTask(Plugin plugin, Runnable task, long delay, long period);
    void cancelTask(int taskId);
    void cancelTasks(Plugin plugin, boolean unused);
    boolean isQueued(int taskId);
}
