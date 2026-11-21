package dev.rime.bukkit;

import org.bukkit.event.Event;
import org.bukkit.event.Listener;
import org.bukkit.plugin.Plugin;
import org.bukkit.plugin.PluginManager;
import org.bukkit.plugin.java.JavaPlugin;

final class RimePluginManager implements PluginManager {
    static final RimePluginManager INSTANCE = new RimePluginManager();
    @Override public void registerEvents(Listener listener, Plugin plugin) { Bridge.registerEvents(listener, plugin); }
    @Override public void callEvent(Event event) { Bridge.callEvent(event); }
    @Override public Plugin getPlugin(String name) {
        for (JavaPlugin p : Bridge.plugins) if (p.getName().equalsIgnoreCase(name)) return p;
        return null;
    }
    @Override public Plugin[] getPlugins() { return Bridge.plugins.toArray(new Plugin[0]); }
    @Override public boolean isPluginEnabled(String name) { Plugin p = getPlugin(name); return p != null && p.isEnabled(); }
    @Override public void disablePlugin(Plugin plugin) {
        if (plugin instanceof JavaPlugin && plugin.isEnabled()) {
            ((JavaPlugin) plugin).setEnabled(false);
            Bridge.unregister(plugin);
            RimeScheduler.INSTANCE.cancelTasks(plugin);
        }
    }
}
