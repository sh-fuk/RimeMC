package dev.rime.bukkit;

import java.util.*;
import java.util.logging.Logger;

import org.bukkit.Server;
import org.bukkit.World;
import org.bukkit.command.CommandSender;
import org.bukkit.command.ConsoleCommandSender;
import org.bukkit.entity.Player;
import org.bukkit.plugin.PluginManager;
import org.bukkit.scheduler.BukkitScheduler;

public final class RimeServer implements Server {
    public static final RimeServer INSTANCE = new RimeServer();
    static final Logger LOGGER = RimeLogger.create("");

    @Override public String getName() { return "RimeMC"; }
    @Override public String getVersion() { return "RimeMC (MC: 26.1)"; }
    @Override public String getBukkitVersion() { return "26.1-R0.1-RIME"; }
    @Override public Collection<? extends Player> getOnlinePlayers() {
        List<Player> out = new ArrayList<>();
        for (String n : Bridge.players()) { Player p = Bridge.player(n); if (p != null) out.add(p); }
        return Collections.unmodifiableList(out);
    }
    @Override public int getMaxPlayers() { return Bridge.maxPlayers(); }
    @Override public String getMotd() { return Bridge.motd(); }
    @Override public Player getPlayer(String name) {
        Player exact = getPlayerExact(name);
        if (exact != null) return exact;
        String lower = name.toLowerCase(Locale.ROOT);
        for (Player p : getOnlinePlayers()) if (p.getName().toLowerCase(Locale.ROOT).startsWith(lower)) return p;
        return null;
    }
    @Override public Player getPlayerExact(String name) {
        for (String n : Bridge.players()) if (n.equalsIgnoreCase(name)) return Bridge.player(n);
        return null;
    }
    @Override public Player getPlayer(UUID id) {
        for (Player p : getOnlinePlayers()) if (p.getUniqueId().equals(id)) return p;
        return null;
    }
    @Override public int broadcastMessage(String message) { Bridge.broadcast(message); return Bridge.players().length; }
    @Override public PluginManager getPluginManager() { return RimePluginManager.INSTANCE; }
    @Override public BukkitScheduler getScheduler() { return RimeScheduler.INSTANCE; }
    @Override public Logger getLogger() { return LOGGER; }
    @Override public List<World> getWorlds() { return Collections.singletonList(RimeWorld.INSTANCE); }
    @Override public World getWorld(String name) { return RimeWorld.INSTANCE.getName().equals(name) ? RimeWorld.INSTANCE : null; }
    @Override public ConsoleCommandSender getConsoleSender() { return RimeConsole.INSTANCE; }
    @Override public boolean dispatchCommand(CommandSender sender, String line) {
        if (sender instanceof Player) { ((Player) sender).performCommand(line); return true; }
        String l = line.startsWith("/") ? line.substring(1) : line;
        String[] parts = l.trim().split("\\s+");
        if (parts.length > 0) {
            org.bukkit.command.PluginCommand c = Bridge.commands.get(parts[0].toLowerCase(Locale.ROOT));
            if (c != null) { c.execute(sender, parts[0], Arrays.copyOfRange(parts, 1, parts.length)); return true; }
        }
        String reply = Bridge.dispatchConsole(l);
        if (reply != null && !reply.isEmpty()) sender.sendMessage(reply);
        return true;
    }
    @Override public boolean isPrimaryThread() { return true; }
}
