package org.bukkit;

import java.util.Collection;
import java.util.List;
import java.util.UUID;
import java.util.logging.Logger;

import org.bukkit.command.CommandSender;
import org.bukkit.command.ConsoleCommandSender;
import org.bukkit.entity.Player;
import org.bukkit.plugin.PluginManager;
import org.bukkit.scheduler.BukkitScheduler;

public final class Bukkit {
    private static Server server = dev.rime.bukkit.RimeServer.INSTANCE;
    private Bukkit() {}
    public static Server getServer() { return server; }
    public static void setServer(Server s) { if (s != null) server = s; }
    public static String getName() { return server.getName(); }
    public static String getVersion() { return server.getVersion(); }
    public static String getBukkitVersion() { return server.getBukkitVersion(); }
    public static Collection<? extends Player> getOnlinePlayers() { return server.getOnlinePlayers(); }
    public static int getMaxPlayers() { return server.getMaxPlayers(); }
    public static String getMotd() { return server.getMotd(); }
    public static Player getPlayer(String name) { return server.getPlayer(name); }
    public static Player getPlayerExact(String name) { return server.getPlayerExact(name); }
    public static Player getPlayer(UUID id) { return server.getPlayer(id); }
    public static int broadcastMessage(String message) { return server.broadcastMessage(message); }
    public static PluginManager getPluginManager() { return server.getPluginManager(); }
    public static BukkitScheduler getScheduler() { return server.getScheduler(); }
    public static Logger getLogger() { return server.getLogger(); }
    public static List<World> getWorlds() { return server.getWorlds(); }
    public static World getWorld(String name) { return server.getWorld(name); }
    public static ConsoleCommandSender getConsoleSender() { return server.getConsoleSender(); }
    public static boolean dispatchCommand(CommandSender sender, String line) { return server.dispatchCommand(sender, line); }
    public static boolean isPrimaryThread() { return server.isPrimaryThread(); }
}
