package org.bukkit.event;

import org.bukkit.plugin.Plugin;

/** Handlers are kept by the RimeMC bridge; this type exists for API compatibility. */
public class HandlerList {
    public HandlerList() {}
    public static void unregisterAll(Plugin plugin) { dev.rime.bukkit.Bridge.unregister(plugin); }
    public static void unregisterAll(Listener listener) {}
}
